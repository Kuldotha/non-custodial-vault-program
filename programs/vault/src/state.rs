use anchor_lang::prelude::*;

/// The SOL entry's mint key: the System Program, because it is what actually
/// controls lamports. Slot 0 of every ledger is SOL, always, from creation —
/// so a zero mint at index 0 means SOL and a zero mint anywhere else means a
/// free slot. See spec §2.3.
pub const SOL_MINT: Pubkey = Pubkey::new_from_array([0u8; 32]);

/// Defaults. A human's ledger only ever holds the mints they win, so 32 slots with a
/// 16-slot floor is generous. A *program's* ledger holds every mint it ever pays out,
/// so games should pass much larger values when they create theirs.
pub const DEFAULT_SLOTS: u16 = 32;
pub const DEFAULT_MIN_FREE: u16 = 16;
/// Solana caps a single realloc at 10 KiB, which is 256 entries.
pub const MAX_SLOT_INCREASE: u16 = 256;
/// Largest capacity a ledger may be opened with. Rent scales with it and is paid up front, so an
/// accidental zero too many is an expensive mistake — 256 slots is already far past any realistic
/// mint set, and a ledger that genuinely needs more can be grown a step at a time.
pub const MAX_SLOTS: u16 = 256;

pub const ENTRY_SIZE: usize = 32 + 8;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, PartialEq)]
pub struct Entry {
    pub mint: Pubkey,
    pub amount: u64,
}

/// ["ledger", owner] — one per owner, human or program. Holds claims, never value.
#[account]
#[derive(Default)]
pub struct Ledger {
    pub owner: Pubkey,
    /// true when `owner` is off-curve, i.e. a program's PDA. Set once at creation
    /// from the owner's curve and never mutated (spec §2.5).
    pub pda_auth: bool,
    pub bump: u8,
    pub _pad: [u8; 6],
    /// Where the rent goes when this account is closed, and the only key that may grow it.
    /// Always the owner — the lamports may come from any signer, but they come back here.
    pub rent_payer: Pubkey,
    /// For a wallet: a session key that may consent to debits in the owner's place and end its
    /// rollup session, or zero. Unscoped, so it spends the whole balance, but it cannot reach the
    /// wallet — `withdraw` derives its ledger from the signer. For a PDA ledger: the member
    /// program it belongs to, which settle checks each receipt against.
    pub authorized: Pubkey,
    /// Pre-allocated slots. `entries.len()` is the capacity; a delegated ledger
    /// cannot realloc, so the space has to be there before the session starts.
    pub entries: Vec<Entry>,
}

impl Ledger {
    /// 8 discriminator + owner + pda_auth + bump + pad + rent_payer + authorized + vec len.
    pub const HEADER: usize = 8 + 32 + 1 + 1 + 6 + 32 + 32 + 4;

    pub fn space(slots: usize) -> usize {
        Self::HEADER + slots * ENTRY_SIZE
    }

    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    pub fn free_slots(&self) -> usize {
        self.entries
            .iter()
            .enumerate()
            .filter(|(i, e)| *i != 0 && e.mint == SOL_MINT)
            .count()
    }

    /// Initialises a fresh ledger: every slot present and zeroed, slot 0 claimed by SOL.
    pub fn init(&mut self, owner: Pubkey, pda_auth: bool, bump: u8, rent_payer: Pubkey, slots: usize) {
        self.owner = owner;
        self.pda_auth = pda_auth;
        self.bump = bump;
        self._pad = [0u8; 6];
        self.rent_payer = rent_payer;
        self.authorized = Pubkey::default();
        self.entries = vec![Entry::default(); slots];
        self.entries[0].mint = SOL_MINT;
    }

    /// Whether `who` may end this ledger's rollup session. A PDA's owner signs by CPI, so a
    /// program ledger is reached the same way it was delegated.
    pub fn may_end_session(&self, who: &Pubkey) -> bool {
        *who == self.owner
            || (!self.pda_auth && self.authorized != Pubkey::default() && *who == self.authorized)
    }

    /// Index of the entry for `mint`, or None. SOL is positional — never a scan.
    pub fn index_of(&self, mint: &Pubkey) -> Option<usize> {
        if *mint == SOL_MINT {
            return Some(0);
        }
        self.entries.iter().position(|e| e.mint == *mint)
    }

    /// Index for `mint`, claiming a free slot if it has none yet.
    pub fn index_or_claim(&mut self, mint: &Pubkey) -> Result<usize> {
        if let Some(i) = self.index_of(mint) {
            return Ok(i);
        }
        let free = self
            .entries
            .iter()
            .enumerate()
            .position(|(i, e)| i != 0 && e.mint == SOL_MINT)
            .ok_or(VaultError::LedgerFull)?;
        self.entries[free].mint = *mint;
        Ok(free)
    }

    pub fn index_for_credit(&mut self, mint: &Pubkey, may_claim: bool) -> Result<usize> {
        if let Some(i) = self.index_of(mint) {
            return Ok(i);
        }
        require!(may_claim, VaultError::SlotClaimNeedsConsent);
        self.index_or_claim(mint)
    }

    /// Debits an entry, releasing its slot at zero. The release is what keeps a slot from
    /// being spent for the life of the ledger: a credit needs no consent, so without it
    /// anyone could fill a ledger with dust of mints its owner never asked for.
    pub fn debit(&mut self, index: usize, amount: u64) -> Result<()> {
        let entry = &mut self.entries[index];
        entry.amount = entry.amount.checked_sub(amount).ok_or(VaultError::Insufficient)?;
        if entry.amount == 0 && index != 0 {
            entry.mint = SOL_MINT;
        }
        Ok(())
    }

    pub fn credit(&mut self, index: usize, amount: u64) -> Result<()> {
        let entry = &mut self.entries[index];
        entry.amount = entry.amount.checked_add(amount).ok_or(VaultError::Overflow)?;
        Ok(())
    }
}

#[error_code]
pub enum VaultError {
    #[msg("Ledger has no free slot for this mint — grow it first")]
    LedgerFull,
    #[msg("No balance for this mint")]
    NoBalance,
    #[msg("Insufficient balance")]
    Insufficient,
    #[msg("Arithmetic overflow")]
    Overflow,
    #[msg("At least one side of a movement must be a program ledger")]
    NotProgramMediated,
    #[msg("The program side must sign, by invoke_signed over its own seeds")]
    MissingProgramSignature,
    #[msg("A human must sign to be debited")]
    MissingUserSignature,
    #[msg("Reserves cannot cover this withdrawal")]
    InsufficientReserve,
    #[msg("Token account does not match the mint argument")]
    MintMismatch,
    #[msg("Slot count must be between 1 and 256")]
    BadSlotCount,
    #[msg("Creating the ledger's permission account failed")]
    PermissionFailed,
    #[msg("This ledger already has a permission account")]
    PermissionExists,
    #[msg("A token account pair is missing for a non-zero balance")]
    MissingTokenAccounts,
    #[msg("The reserve must be the vault's associated token account for this mint")]
    NotCanonicalReserve,
    #[msg("The vault is not funded to its rent floor — call initialize_vault first")]
    VaultNotInitialized,
    #[msg("Ledger account is not owned by this program")]
    BadLedgerOwner,
    #[msg("Malformed receipt — bad counts, sizes, or account list")]
    NoAuthorization,
    #[msg("The authority account does not match the one on record")]
    BadAuthority,
    #[msg("deposit and withdraw are wallet-only; program ledgers move value through settle")]
    OffCurveOwnerNotAllowed,
    #[msg("A wallet must fund its own ledger — only a PDA's rent may be sponsored")]
    MustFundOwnLedger,
    #[msg("Only the ledger's recorded rent payer may do this")]
    NotRentPayer,
    #[msg("That ledger already exists")]
    LedgerExists,
    #[msg("The account passed in the receipt slot is not a receipt")]
    NotAReceipt,
    #[msg("This instruction is for wallet ledgers — the owner must be on-curve")]
    OwnerNotWallet,
    #[msg("This instruction is for program ledgers — the owner must be a PDA")]
    OwnerNotPda,
    #[msg("The seeds do not derive the owner under the claimed member program")]
    MemberProgramMismatch,
    #[msg("A program ledger cannot carry a session key — only wallets delegate consent")]
    CannotAuthorizePdaLedger,
    #[msg("The authorized session key must be on-curve")]
    BadAuthorizedKey,
    #[msg("The signer is neither the debited ledger's owner nor its session key")]
    NotAuthorizedToConsent,
    #[msg("A receipt created in this slot is still live — it may not be overwritten")]
    ReceiptLive,
    #[msg("A receipt must be settled in the slot it was created in")]
    ReceiptExpired,
    #[msg("The last receipt at this address was never closed by the member program")]
    ReceiptNotConsumed,
    #[msg("The callback program is not the member program proven into this receipt")]
    CallbackProgramMismatch,
    #[msg("A receipt names the same ledger more than once")]
    DuplicateLedger,
    #[msg("Opening a new slot on a human ledger needs the owner's or session key's consent")]
    SlotClaimNeedsConsent,
}

/// The reserve for a mint is the vault PDA's **associated** token account, created by the
/// developer with `create_idempotent` in the same transaction — never here.
///
/// Asserting the derivation is not what gives authority; the token account's own `owner` field
/// does. It is asserted so a mint has exactly one pool: any vault-owned account would let
/// deposits and withdrawals hit different ones and strand funds, though never lose them, since
/// the ledgers are the only accounting.
pub fn require_reserve(info: &AccountInfo, vault: &Pubkey, mint: &Pubkey) -> Result<u64> {
    require_keys_eq!(
        info.key(),
        anchor_spl::associated_token::get_associated_token_address(vault, mint),
        VaultError::NotCanonicalReserve
    );
    let (m, o, amount) = token_fields(info)?;
    require_keys_eq!(m, *mint, VaultError::MintMismatch);
    require_keys_eq!(o, *vault, VaultError::MintMismatch);
    Ok(amount)
}

/// A human can never sign for an off-curve address and `find_program_address` never
/// returns an on-curve one, so the curve of the owner is a sound discriminator.
///
/// Uses the `sol_curve_validate_point` syscall — `solana_pubkey::bytes_are_curve_point`
/// is `unimplemented!()` when compiled for the SBF target and panics at runtime.
pub fn is_pda(owner: &Pubkey) -> bool {
    let point = solana_curve25519::edwards::PodEdwardsPoint(owner.to_bytes());
    !solana_curve25519::edwards::validate_edwards(&point)
}

/// The vault's own rent-exempt minimum. Not part of any ledger's balance, so every SOL
/// path subtracts it before deciding what is spendable.
pub fn vault_floor() -> Result<u64> {
    Ok(Rent::get()?.minimum_balance(0))
}

/// Reads (mint, owner, amount) out of a raw SPL token account. By hand rather than
/// `Account<TokenAccount>` because these slots carry a placeholder on the SOL path and so
/// cannot be typed in the accounts struct.
pub fn token_fields(info: &AccountInfo) -> Result<(Pubkey, Pubkey, u64)> {
    require_keys_eq!(*info.owner, anchor_spl::token::ID, VaultError::MintMismatch);
    let data = info.try_borrow_data()?;
    require!(data.len() >= 165, VaultError::MintMismatch);
    let mint = Pubkey::new_from_array(data[0..32].try_into().unwrap());
    let owner = Pubkey::new_from_array(data[32..64].try_into().unwrap());
    let amount = u64::from_le_bytes(data[64..72].try_into().unwrap());
    Ok((mint, owner, amount))
}

/// Adds `slot_increase` slots when fewer than `min_free` are free, funding the rent from `payer`.
/// basenet only: a delegated account cannot be reallocated, which is why slots are
/// pre-allocated at all.
///
/// Takes a raw `AccountInfo` and an in-memory `Ledger` because `Account<Ledger>` cannot grow
/// one: Anchor serialises its own copy on exit, after the handler, so the longer `entries`
/// would be overwritten by the original — leaving the account paying rent for bytes that
/// `capacity()` never reports.
pub fn ensure_headroom<'info>(
    info: &AccountInfo<'info>,
    ledger: &mut Ledger,
    payer: &Signer<'info>,
    system_program: &Program<'info, System>,
    min_free: u16,
    slot_increase: u16,
) -> Result<()> {
    // Growth is funded by whoever funded the account, and by nobody else. Any other payer
    // would reopen the channel a slot at a time, since the extra rent leaves with the refund.
    require_keys_eq!(payer.key(), ledger.rent_payer, VaultError::NotRentPayer);

    if ledger.free_slots() >= min_free as usize {
        return Ok(());
    }
    require!(slot_increase > 0 && slot_increase <= MAX_SLOT_INCREASE, VaultError::BadSlotCount);

    for _ in 0..slot_increase {
        ledger.entries.push(Entry::default());
    }
    let new_space = Ledger::space(ledger.capacity());

    let needed = Rent::get()?.minimum_balance(new_space);
    let have = info.lamports();
    if needed > have {
        anchor_lang::system_program::transfer(
            CpiContext::new(
                system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: payer.to_account_info(),
                    to: info.clone(),
                },
            ),
            needed - have,
        )?;
    }
    info.resize(new_space)?;
    Ok(())
}

/// Creates `["ledger", owner]` — a program-owned account sized for `DEFAULT_SLOTS`.
///
/// Done by hand rather than with Anchor's `init_if_needed`, which re-evaluates its `space`
/// expression against the account's real length on *every* call. A ledger grows, so that
/// check starts failing the moment it does, and every later deposit reverts. There is no
/// way to express "size it on creation, ignore it afterwards" with that constraint.
pub fn create_ledger_account<'info>(
    info: &AccountInfo<'info>,
    owner: &Signer<'info>,
    system_program: &Program<'info, System>,
    bump: u8,
) -> Result<Ledger> {
    // Only wallets reach this path — `deposit` refuses an off-curve owner — so the owner is
    // always its own rent payer here.
    create_ledger_account_sized(info, owner, owner, system_program, bump, DEFAULT_SLOTS as usize)
}

/// As above, but the rent may come from somebody other than the owner and the capacity is
/// chosen rather than defaulted. A PDA cannot pay its own rent, and a program that pays out
/// fourteen tokens wants room for fourteen from the start.
pub fn create_ledger_account_sized<'info>(
    info: &AccountInfo<'info>,
    payer: &Signer<'info>,
    owner: &Signer<'info>,
    system_program: &Program<'info, System>,
    bump: u8,
    slots: usize,
) -> Result<Ledger> {
    // A wallet funds its own ledger and nobody else may: rent returns to the payer on close, so
    // paying somebody else's would be a way to hand them money. A PDA cannot pay, so whoever
    // does becomes the rent payer — the lamports go back where they came from, and that account
    // is then the only one that may grow or close it.
    let rent_payer = if is_pda(&owner.key()) {
        payer.key()
    } else {
        require_keys_eq!(payer.key(), owner.key(), VaultError::MustFundOwnLedger);
        owner.key()
    };

    let space = Ledger::space(slots);
    let rent = Rent::get()?.minimum_balance(space);
    let owner_key = owner.key();
    let bump_arr = [bump];
    let signer_seeds: &[&[&[u8]]] = &[&[b"ledger", owner_key.as_ref(), &bump_arr]];

    // Not `create_account`: it aborts if the PDA already holds lamports, so anyone could block a
    // ledger's creation by sending a single lamport to its address first. transfer-then-allocate
    // -then-assign reaches the same end state but tolerates a pre-funded account.
    let have = info.lamports();
    if have < rent {
        anchor_lang::system_program::transfer(
            CpiContext::new(
                system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: payer.to_account_info(),
                    to: info.clone(),
                },
            ),
            rent - have,
        )?;
    }
    anchor_lang::system_program::allocate(
        CpiContext::new_with_signer(
            system_program.to_account_info(),
            anchor_lang::system_program::Allocate { account_to_allocate: info.clone() },
            signer_seeds,
        ),
        space as u64,
    )?;
    anchor_lang::system_program::assign(
        CpiContext::new_with_signer(
            system_program.to_account_info(),
            anchor_lang::system_program::Assign { account_to_assign: info.clone() },
            signer_seeds,
        ),
        &crate::ID,
    )?;

    let mut ledger = Ledger::default();
    ledger.init(owner.key(), is_pda(&owner.key()), bump, rent_payer, slots);
    Ok(ledger)
}

/// Reads an existing ledger out of its account, checking the program owns it.
pub fn load_ledger(info: &AccountInfo) -> Result<Ledger> {
    require_keys_eq!(*info.owner, crate::ID, VaultError::BadLedgerOwner);
    let data = info.try_borrow_data()?;
    Ledger::try_deserialize(&mut &data[..])
}

/// Writes a ledger back. The account must already be large enough — grow first.
pub fn store_ledger(info: &AccountInfo, ledger: &Ledger) -> Result<()> {
    let mut data = info.try_borrow_mut_data()?;
    ledger.try_serialize(&mut &mut data[..])?;
    Ok(())
}
