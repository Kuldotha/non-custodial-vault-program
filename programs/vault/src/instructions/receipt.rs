use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::consts::EPHEMERAL_VAULT_ID;
use ephemeral_rollups_sdk::ephemeral_accounts::EphemeralAccount;

use crate::state::*;

/// A receipt is terms a program has authorised, written into an ephemeral account the vault
/// owns, to be settled by a later top-level vault instruction.
///
/// It exists because of how a private rollup filters access: only a transaction's **top-level**
/// program is checked against an account's permission, so a game can never reach a ledger by
/// CPI. It can reach *this* — no permissioned account is involved — and the vault then does the
/// settle itself, where it is top-level and always a member.
///
/// Only the **program's** consent is captured here: the authority signs via `invoke_signed`,
/// committing to the terms. The human's consent for a debit is checked at settle instead —
/// against the debited ledger, which this instruction deliberately cannot read (it is the
/// permissioned account), and which is the only place that knows who may consent: the owner,
/// or the session key the owner assigned for this authority.
///
/// Creation is therefore human-permissionless: a program can write a receipt debiting anyone,
/// and it means nothing until someone entitled to consent signs the settle. An existing open
/// receipt at the same nonce is overwritten — only this authority can ever write at its own
/// addresses, and the real flows create and settle in one transaction, so whatever was
/// sitting there is debris from a flow that never finished.
///
/// Layout: `state | human | authority | count | (mint, amount, to_human) * count`

pub const RECEIPT_HEADER: usize = 1 + 32 + 32 + 1;
pub const MOVEMENT_SIZE: usize = 32 + 8 + 1;
pub const RECEIPT_OPEN: u8 = 0;
pub const RECEIPT_SETTLED: u8 = 1;

#[derive(Accounts)]
#[instruction(nonce: u64)]
pub struct CreateReceipt<'info> {
    /// CHECK: the party on the human side. Named, never required to sign — consent for a
    /// debit is the settle's business, checked against this party's ledger there.
    pub human: UncheckedAccount<'info>,

    /// CHECK: the program's authority PDA. Must sign, which a game does via `invoke_signed`;
    /// that signature propagates into this frame. Also pays the ephemeral rent, so it needs
    /// lamports in the rollup.
    #[account(mut)]
    pub authority: UncheckedAccount<'info>,

    /// CHECK: created here and owned by this program, so nothing else can rewrite the terms.
    #[account(mut, seeds = [b"receipt", authority.key().as_ref(), &nonce.to_le_bytes()], bump)]
    pub receipt: UncheckedAccount<'info>,

    /// CHECK: the ephemeral rent vault.
    #[account(mut, address = EPHEMERAL_VAULT_ID)]
    pub ephemeral_vault: UncheckedAccount<'info>,

    /// CHECK: the magic program.
    pub magic_program: UncheckedAccount<'info>,
}

pub fn create_handler(
    ctx: Context<CreateReceipt>,
    nonce: u64,
    movements: Vec<Movement>,
) -> Result<()> {
    require!(ctx.accounts.authority.is_signer, VaultError::MissingProgramSignature);
    // An empty receipt is allowed. A losing card authorises nothing, and the caller still
    // needs the account to exist — the runtime rejects a transaction that declares a writable
    // account which is never created.
    require!(movements.len() <= u8::MAX as usize, VaultError::NoAuthorization);

    let bump = ctx.bumps.receipt;
    let authority_key = ctx.accounts.authority.key();
    let nonce_le = nonce.to_le_bytes();
    let len = RECEIPT_HEADER + movements.len() * MOVEMENT_SIZE;

    let receipt_info = ctx.accounts.receipt.to_account_info();
    let authority_info = ctx.accounts.authority.to_account_info();
    let vault_info = ctx.accounts.ephemeral_vault.to_account_info();
    let bump_arr = [bump];
    let seeds: [&[u8]; 4] = [b"receipt", authority_key.as_ref(), &nonce_le, &bump_arr];
    let signer_seeds = [&seeds[..]];
    let account = EphemeralAccount::new(&authority_info, &receipt_info, &vault_info)
        .with_signer_seeds(&signer_seeds);
    // Overwrite, never fail: a vault-owned account at this address can only be an OPEN
    // receipt this same authority once wrote and abandoned. Settled receipts leave vault
    // ownership before anything else can run, so they cannot be sitting here.
    if receipt_info.data_len() == 0 {
        account.create(len as u32)?;
    } else if receipt_info.data_len() != len {
        account.resize(len as u32)?;
    }

    let mut d = ctx.accounts.receipt.try_borrow_mut_data()?;
    d[0] = RECEIPT_OPEN;
    d[1..33].copy_from_slice(&ctx.accounts.human.key().to_bytes());
    d[33..65].copy_from_slice(&authority_key.to_bytes());
    d[65] = movements.len() as u8;
    for (i, m) in movements.iter().enumerate() {
        let o = RECEIPT_HEADER + i * MOVEMENT_SIZE;
        d[o..o + 32].copy_from_slice(&m.mint.to_bytes());
        d[o + 32..o + 40].copy_from_slice(&m.amount.to_le_bytes());
        d[o + 40] = m.to_human as u8;
    }
    Ok(())
}

/// One movement. A receipt may carry several — collecting a card can pay out in more than one
/// mint at once.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct Movement {
    pub mint: Pubkey,
    pub amount: u64,
    /// true: program → human (a payout). false: human → program (a charge).
    pub to_human: bool,
}

/// Settles a receipt. The program side consented at creation (its `invoke_signed`), and the
/// human side consents **here**, where the debited ledger is finally in hand: any movement
/// that debits the human requires `consenter` to sign and to be either the ledger's owner or
/// the session key its owner assigned — for this receipt's authority, not just any program.
/// A credit-only receipt still needs no signature at all, which is what keeps collects
/// permissionless. Neither ledger can be substituted, because the receipt names their owners.
///
/// A debit of an off-curve human side is unrepresentable now: a PDA cannot sign a top-level
/// instruction and a PDA ledger can never hold a session key. No live flow ever did that —
/// programs pay each other through `settle` — and the door is better closed.
///
/// Afterwards the receipt is emptied and handed to the program that authorised it, so that
/// program can close it and recover the rent it sponsored. The vault cannot close it itself:
/// closing an ephemeral account needs the *sponsor's* signature, and the sponsor is the other
/// program. Leaving it open would bleed the sponsor a little on every settle.
///
/// The handover doubles as proof. The receipt is a PDA of this program, so the authorising
/// program could never have created it — finding it in their own hands means the vault
/// settled and released it.
#[derive(Accounts)]
pub struct SettleReceipt<'info> {
    /// CHECK: must be owned by this program, which is what proves the vault wrote the terms.
    #[account(mut, owner = crate::ID)]
    pub receipt: UncheckedAccount<'info>,

    /// CHECK: the authority named on the receipt. Only its `owner` is read — that is the
    /// program the emptied receipt is handed back to.
    pub authority: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [b"ledger", human_ledger.owner.as_ref()],
        bump = human_ledger.bump,
    )]
    pub human_ledger: Account<'info, Ledger>,

    #[account(
        mut,
        seeds = [b"ledger", program_ledger.owner.as_ref()],
        bump = program_ledger.bump,
    )]
    pub program_ledger: Account<'info, Ledger>,

    /// CHECK: whoever consents to the debit of the human side — the ledger's owner, or its
    /// assigned session key. Only examined when a movement debits the human; a credit-only
    /// receipt ignores it entirely, so a collect passes any account here, unsigned.
    pub consenter: UncheckedAccount<'info>,
}

pub fn settle_handler(ctx: Context<SettleReceipt>) -> Result<()> {
    let (human, authority, movements) = {
        let d = ctx.accounts.receipt.try_borrow_data()?;
        require!(d.len() >= RECEIPT_HEADER, VaultError::NoAuthorization);
        // The state byte is the only signal: an ephemeral account holds no lamports of its
        // own — its rent lives with the magic vault — so a balance check says nothing here.
        // A closed receipt keeps its bytes, so a replayed settle still sees SETTLED; and if the
        // runtime ever reaps it, the owner check above fails instead. Either way, once.
        require!(d[0] == RECEIPT_OPEN, VaultError::AlreadySettled);

        let human = Pubkey::new_from_array(d[1..33].try_into().unwrap());
        let authority = Pubkey::new_from_array(d[33..65].try_into().unwrap());
        let count = d[65] as usize;
        require!(d.len() >= RECEIPT_HEADER + count * MOVEMENT_SIZE, VaultError::NoAuthorization);

        let mut movements = Vec::with_capacity(count);
        for i in 0..count {
            let o = RECEIPT_HEADER + i * MOVEMENT_SIZE;
            movements.push(Movement {
                mint: Pubkey::new_from_array(d[o..o + 32].try_into().unwrap()),
                amount: u64::from_le_bytes(d[o + 32..o + 40].try_into().unwrap()),
                to_human: d[o + 40] == 1,
            });
        }
        (human, authority, movements)
    };

    // Neither side is caller-chosen: the receipt names both owners.
    require_keys_eq!(ctx.accounts.human_ledger.owner, human, VaultError::BadAuthority);
    require_keys_eq!(ctx.accounts.program_ledger.owner, authority, VaultError::BadAuthority);

    // The human side's consent, deferred from creation to the one instruction that holds
    // their ledger. The session key is program-locked: it counts only for receipts of the
    // authority the owner named, so a key leaked from one game consents to nothing else.
    if movements.iter().any(|m| !m.to_human) {
        let consenter = &ctx.accounts.consenter;
        require!(consenter.is_signer, VaultError::MissingUserSignature);
        let allowed = consenter.key() == human
            || read_authorization(&ctx.accounts.human_ledger.to_account_info())?
                .is_some_and(|(key, for_authority)| {
                    key == consenter.key() && for_authority == authority
                });
        require!(allowed, VaultError::NotAuthorizedToConsent);
    }
    // The same two rules as `settle`, and for the same reasons — see the long note there.
    // Never two humans, and both sides consented when the receipt was written: the program
    // signed for its PDA, the human signed if debited.
    //
    // `human_ledger` is a name for the common case, not a constraint: it may itself be a
    // program's ledger, which is how one program pays another (a game moving a share of each
    // sale into a jackpot it cannot later withdraw). What is never allowed is *two* humans.
    require!(
        ctx.accounts.program_ledger.pda_auth || ctx.accounts.human_ledger.pda_auth,
        VaultError::NotProgramMediated,
    );

    for m in movements.iter() {
        let (from, to): (&mut Account<Ledger>, &mut Account<Ledger>) = if m.to_human {
            (&mut ctx.accounts.program_ledger, &mut ctx.accounts.human_ledger)
        } else {
            (&mut ctx.accounts.human_ledger, &mut ctx.accounts.program_ledger)
        };

        let i = from.index_of(&m.mint).ok_or(VaultError::NoBalance)?;
        from.entries[i].amount = from.entries[i]
            .amount
            .checked_sub(m.amount)
            .ok_or(VaultError::Insufficient)?;

        let j = to.index_or_claim(&m.mint)?;
        to.entries[j].amount = to.entries[j]
            .amount
            .checked_add(m.amount)
            .ok_or(VaultError::Overflow)?;
    }

    // Hand it back: zero first, because an account's owner may only change while its data is
    // all zeros. That also drops the SETTLED byte, but replay is already impossible — the
    // account is no longer ours, so `owner = crate::ID` rejects a second settle.
    require_keys_eq!(ctx.accounts.authority.key(), authority, VaultError::BadAuthority);
    let new_owner = *ctx.accounts.authority.owner;
    ctx.accounts.receipt.try_borrow_mut_data()?.fill(0);
    ctx.accounts.receipt.assign(&new_owner);
    Ok(())
}
