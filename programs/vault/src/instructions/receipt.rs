use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use ephemeral_rollups_sdk::consts::EPHEMERAL_VAULT_ID;
use ephemeral_rollups_sdk::ephemeral_accounts::EphemeralAccount;

use crate::instructions::open_pda_ledger::verify_pda_owner;
use crate::state::*;

pub const RECEIPT_HEADER: usize = 123;
pub const MOVEMENT_SIZE: usize = 32 + 8 + 1 + 1;
pub const OWNER_SIZE: usize = 32;

/// The receipt's 8-byte account discriminator, in Anchor's `account:<Name>` convention
/// (`sha256("account:Receipt")[..8]`). The receipt is written and read by hand rather than as
/// `Account<T>` — its length is variable and the ephemeral vault mints it — so `settle_receipt`
/// checks this itself. It is the only thing that stops another vault-owned account, a `Ledger`
/// above all, from being passed in the receipt slot and settled with no consent: there is no
/// "open vs consumed" state to check, because a consumed receipt is owned by the member program
/// and never reaches the handler.
pub const RECEIPT_DISCRIMINATOR: [u8; 8] = [39, 154, 73, 106, 80, 102, 145, 153];

const O_DISCRIMINATOR: usize = 0;
const O_OWNER0: usize = 8;
const O_AUTHORITY: usize = 40;
const O_MEMBER: usize = 72;
const O_CALLBACK_DISC: usize = 104;
const O_SLOT: usize = 112;
const O_LEDGERS: usize = 120;
const O_COUNT: usize = 121;
const O_ARGS_LEN: usize = 122;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct Movement {
    pub mint: Pubkey,
    pub amount: u64,
    /// Indices into the receipt's owner list — each names one exact ledger the movement moves between.
    pub from: u8,
    pub to: u8,
}

#[derive(Accounts)]
#[instruction(movements: Vec<Movement>, member_program: Pubkey, authority_seeds: Vec<Vec<u8>>, owners: Vec<Pubkey>)]
pub struct CreateReceipt<'info> {
    /// CHECK: the program's authority PDA. Must sign, which a program does via `invoke_signed`.
    #[account(mut)]
    pub authority: UncheckedAccount<'info>,

    /// CHECK: created here and owned by this program, so nothing else can rewrite the terms.
    /// PDA `["receipt", authority, owners[0]]`, derived and checked in the handler — a Vec-indexed
    /// seed cannot be expressed in the account macro's IDL codegen.
    #[account(mut)]
    pub receipt: UncheckedAccount<'info>,

    /// CHECK: the ephemeral rent vault.
    #[account(mut, address = EPHEMERAL_VAULT_ID)]
    pub ephemeral_vault: UncheckedAccount<'info>,

    /// CHECK: the magic program.
    pub magic_program: UncheckedAccount<'info>,
}

pub fn create_handler<'info>(
    ctx: Context<'_, '_, '_, 'info, CreateReceipt<'info>>,
    movements: Vec<Movement>,
    member_program: Pubkey,
    authority_seeds: Vec<Vec<u8>>,
    owners: Vec<Pubkey>,
    callback_disc: [u8; 8],
    args: Vec<u8>,
) -> Result<()> {
    require!(ctx.accounts.authority.is_signer, VaultError::MissingProgramSignature);
    require!(movements.len() <= u8::MAX as usize, VaultError::NoAuthorization);
    require!(args.len() <= u8::MAX as usize, VaultError::NoAuthorization);
    require!(!owners.is_empty() && owners.len() <= u8::MAX as usize, VaultError::NoAuthorization);

    // Not `authority.owner` — delegation rewrites that field.
    verify_pda_owner(&ctx.accounts.authority, &member_program, &authority_seeds)?;

    for (i, o) in owners.iter().enumerate() {
        require!(!owners[..i].contains(o), VaultError::DuplicateLedger);
    }
    for m in movements.iter() {
        require!(
            (m.from as usize) < owners.len() && (m.to as usize) < owners.len(),
            VaultError::NoAuthorization
        );
        require!(m.from != m.to, VaultError::NoAuthorization);
    }
    let ledger_count = owners.len();

    // Approval happens here, so settle needs no consent. Every debited owner approves, and so
    // does any wallet a credit would open a new slot for — the same rule plain settle enforces,
    // so a program cannot seed a wallet with mints it never asked for. A PDA approves through its
    // program's invoke_signed, a wallet by its own key or its session key. The owners' ledgers
    // come first in remaining_accounts, in index order; the approver signers follow.
    require!(ctx.remaining_accounts.len() >= ledger_count, VaultError::NoAuthorization);
    let (ledger_infos, signers) = ctx.remaining_accounts.split_at(ledger_count);
    for (idx, info) in ledger_infos.iter().enumerate() {
        require_keys_eq!(*info.owner, crate::ID, VaultError::BadLedgerOwner);
        let ledger = load_ledger(info)?;
        require_keys_eq!(ledger.owner, owners[idx], VaultError::BadLedgerOwner);
        let derived = Pubkey::create_program_address(
            &[b"ledger", owners[idx].as_ref(), &[ledger.bump]],
            &crate::ID,
        )
        .map_err(|_| error!(VaultError::BadLedgerOwner))?;
        require_keys_eq!(derived, *info.key, VaultError::BadLedgerOwner);

        let debited = movements.iter().any(|m| m.from as usize == idx);
        let new_slot_credit = !ledger.pda_auth
            && movements
                .iter()
                .any(|m| m.to as usize == idx && ledger.index_of(&m.mint).is_none());
        if debited || new_slot_credit {
            let approved = signers
                .iter()
                .any(|s| s.is_signer && (s.key() == owners[idx] || s.key() == ledger.authorized));
            require!(approved, VaultError::NotAuthorizedToConsent);
        }
    }

    let slot = Clock::get()?.slot;
    let receipt_info = ctx.accounts.receipt.to_account_info();

    // A settled receipt the member program never closed still belongs to it. Named here, or the
    // write below fails as `ExternalAccountDataModified` and says nothing about the cause.
    require!(
        receipt_info.data_is_empty() || receipt_info.owner == &crate::ID,
        VaultError::ReceiptNotConsumed
    );

    // An earlier slot's receipt can never be settled, so it is debris. This slot's may be a
    // live purchase whose terms would be rewritten under the player. Anything at this address is
    // a receipt — the seeds are unique to them — so the slot alone decides live vs debris.
    if receipt_info.data_len() >= RECEIPT_HEADER {
        let d = receipt_info.try_borrow_data()?;
        let created = u64::from_le_bytes(d[O_SLOT..O_SLOT + 8].try_into().unwrap());
        require!(created != slot, VaultError::ReceiptLive);
    }

    let extra = owners.len() - 1;
    let len = RECEIPT_HEADER + extra * OWNER_SIZE + movements.len() * MOVEMENT_SIZE + args.len();
    let (receipt_pda, bump) = Pubkey::find_program_address(
        &[b"receipt", ctx.accounts.authority.key().as_ref(), owners[0].as_ref()],
        ctx.program_id,
    );
    require_keys_eq!(ctx.accounts.receipt.key(), receipt_pda, VaultError::BadAuthority);
    let authority_key = ctx.accounts.authority.key();

    let authority_info = ctx.accounts.authority.to_account_info();
    let vault_info = ctx.accounts.ephemeral_vault.to_account_info();
    let bump_arr = [bump];
    let seeds: [&[u8]; 4] = [b"receipt", authority_key.as_ref(), owners[0].as_ref(), &bump_arr];
    let signer_seeds = [&seeds[..]];
    let account = EphemeralAccount::new(&authority_info, &receipt_info, &vault_info)
        .with_signer_seeds(&signer_seeds);
    if receipt_info.data_len() == 0 {
        account.create(len as u32)?;
    } else if receipt_info.data_len() != len {
        account.resize(len as u32)?;
    }

    let mut d = ctx.accounts.receipt.try_borrow_mut_data()?;
    d[O_DISCRIMINATOR..O_DISCRIMINATOR + 8].copy_from_slice(&RECEIPT_DISCRIMINATOR);
    d[O_OWNER0..O_OWNER0 + 32].copy_from_slice(&owners[0].to_bytes());
    d[O_AUTHORITY..O_AUTHORITY + 32].copy_from_slice(&authority_key.to_bytes());
    d[O_MEMBER..O_MEMBER + 32].copy_from_slice(&member_program.to_bytes());
    d[O_CALLBACK_DISC..O_CALLBACK_DISC + 8].copy_from_slice(&callback_disc);
    d[O_SLOT..O_SLOT + 8].copy_from_slice(&slot.to_le_bytes());
    d[O_LEDGERS] = ledger_count as u8;
    d[O_COUNT] = movements.len() as u8;
    d[O_ARGS_LEN] = args.len() as u8;
    for (i, o) in owners.iter().skip(1).enumerate() {
        let at = RECEIPT_HEADER + i * OWNER_SIZE;
        d[at..at + 32].copy_from_slice(&o.to_bytes());
    }
    let mv = RECEIPT_HEADER + extra * OWNER_SIZE;
    for (i, m) in movements.iter().enumerate() {
        let o = mv + i * MOVEMENT_SIZE;
        d[o..o + 32].copy_from_slice(&m.mint.to_bytes());
        d[o + 32..o + 40].copy_from_slice(&m.amount.to_le_bytes());
        d[o + 40] = m.from;
        d[o + 41] = m.to;
    }
    let ao = mv + movements.len() * MOVEMENT_SIZE;
    d[ao..ao + args.len()].copy_from_slice(&args);
    Ok(())
}

/// The ledgers come first in `remaining_accounts`, one per index the movements reach, in index
/// order; everything after them is forwarded to the callback.
///
/// - **Each index is pinned to its stored owner**, and every `pda_auth` ledger must store
///   `member_program` as its authority — so a receipt reaches only that program's own ledgers.
/// - **At most one ledger may be non-`pda_auth`.** That is the no-payment-rail property.
/// - **No consent runs here** — every debit was approved by its owner at creation, so settle is
///   permissionless and only executes.
/// - **The callback should close the receipt**, though nothing here depends on it. Zeroed and
///   handed over before the callback runs, one left behind is inert — it cannot be settled
///   again, and proof of payment is the signature, not the account. Skipping the close costs
///   the member program its rent and blocks its own receipt address until it closes it.
#[derive(Accounts)]
pub struct SettleReceipt<'info> {
    /// CHECK: must be owned by this program, which is what proves the vault wrote the terms.
    #[account(mut, owner = crate::ID)]
    pub receipt: UncheckedAccount<'info>,

    /// CHECK: the authority named on the receipt. Compared against it, nothing more.
    pub authority: UncheckedAccount<'info>,

    /// CHECK: must equal the `member_program` proven into the receipt at creation.
    pub callback_program: UncheckedAccount<'info>,

    /// CHECK: the vault's own authority, seedless so there is exactly one. It signs the
    /// callback, which is what tells the member program a real settle from a direct call —
    /// the receipt itself cannot say so, having been zeroed by then.
    #[account(seeds = [], bump)]
    pub vault_authority: UncheckedAccount<'info>,
}

pub fn settle_handler<'info>(
    ctx: Context<'_, '_, '_, 'info, SettleReceipt<'info>>,
) -> Result<()> {
    let (owners, authority, member_program, callback_disc, movements, args) = {
        let d = ctx.accounts.receipt.try_borrow_data()?;
        require!(d.len() >= RECEIPT_HEADER, VaultError::NoAuthorization);
        // The discriminator is the whole defense against a `Ledger` — or any other vault-owned
        // account — being passed here: `receipt` is only checked to be owned by this program, and
        // settle runs no consent, trusting the terms were approved at creation.
        require!(
            d[O_DISCRIMINATOR..O_DISCRIMINATOR + 8] == RECEIPT_DISCRIMINATOR,
            VaultError::NotAReceipt
        );

        // Same slot, or nothing. Not primarily an attacker control — two transactions can share
        // a slot — but it makes a client that splits creation from settlement fail immediately
        // rather than work while leaving a window to rewrite the terms.
        let created = u64::from_le_bytes(d[O_SLOT..O_SLOT + 8].try_into().unwrap());
        require!(created == Clock::get()?.slot, VaultError::ReceiptExpired);

        let owner0 = Pubkey::new_from_array(d[O_OWNER0..O_OWNER0 + 32].try_into().unwrap());
        let authority = Pubkey::new_from_array(d[O_AUTHORITY..O_AUTHORITY + 32].try_into().unwrap());
        let member = Pubkey::new_from_array(d[O_MEMBER..O_MEMBER + 32].try_into().unwrap());
        let disc: [u8; 8] = d[O_CALLBACK_DISC..O_CALLBACK_DISC + 8].try_into().unwrap();
        let n = d[O_LEDGERS] as usize;
        let count = d[O_COUNT] as usize;
        let args_len = d[O_ARGS_LEN] as usize;
        require!(n >= 1, VaultError::NoAuthorization);
        let mv = RECEIPT_HEADER + (n - 1) * OWNER_SIZE;
        require!(
            d.len() >= mv + count * MOVEMENT_SIZE + args_len,
            VaultError::NoAuthorization
        );

        let mut owners = vec![owner0];
        for i in 0..n - 1 {
            let at = RECEIPT_HEADER + i * OWNER_SIZE;
            owners.push(Pubkey::new_from_array(d[at..at + 32].try_into().unwrap()));
        }
        let mut movements = Vec::with_capacity(count);
        for i in 0..count {
            let o = mv + i * MOVEMENT_SIZE;
            movements.push(Movement {
                mint: Pubkey::new_from_array(d[o..o + 32].try_into().unwrap()),
                amount: u64::from_le_bytes(d[o + 32..o + 40].try_into().unwrap()),
                from: d[o + 40],
                to: d[o + 41],
            });
        }
        let ao = mv + count * MOVEMENT_SIZE;
        (owners, authority, member, disc, movements, d[ao..ao + args_len].to_vec())
    };
    let ledger_count = owners.len();

    require_keys_eq!(ctx.accounts.authority.key(), authority, VaultError::BadAuthority);
    require_keys_eq!(
        ctx.accounts.callback_program.key(),
        member_program,
        VaultError::CallbackProgramMismatch
    );

    // Handled raw rather than as `Account<Ledger>`: the count is variable, and a deserialized
    // copy would go stale behind the callback's CPI.
    require!(ctx.remaining_accounts.len() >= ledger_count, VaultError::NoAuthorization);
    let (ledger_infos, forwarded) = ctx.remaining_accounts.split_at(ledger_count);

    let mut ledgers = Vec::with_capacity(ledger_count);
    for (idx, (info, owner)) in ledger_infos.iter().zip(owners.iter()).enumerate() {
        // Codes are off-enum on purpose: 7000 + check*100 + index. Transaction logs are off in
        // a private rollup, so the error code is the only channel wide enough to say *which*
        // ledger failed *which* check.
        let code = |c: u32| -> Error {
            ProgramError::Custom(7000 + c * 100 + idx as u32).into()
        };
        if info.owner != &crate::ID {
            return Err(code(0));
        }
        let ledger = load_ledger(info)?;
        if ledger.owner != *owner {
            return Err(code(1));
        }
        let derived = Pubkey::create_program_address(
            &[b"ledger", owner.as_ref(), &[ledger.bump]],
            &crate::ID,
        )
        .map_err(|_| code(2))?;
        if derived != *info.key {
            return Err(code(2));
        }
        if ledger.pda_auth && ledger.authorized != member_program {
            return Err(code(3));
        }
        if ledger_infos[..idx].iter().any(|p| p.key == info.key) {
            return Err(code(4));
        }
        ledgers.push(ledger);
    }

    // Value can never move between two people, whatever the shape of the receipt.
    require!(
        ledgers.iter().filter(|l| !l.pda_auth).count() <= 1,
        VaultError::NotProgramMediated
    );

    // No consent here: the debits were approved by their owners at creation. Settle only
    // executes, so it is permissionless.
    for m in movements.iter() {
        let (f, t) = (m.from as usize, m.to as usize);
        let i = ledgers[f].index_of(&m.mint).ok_or(VaultError::NoBalance)?;
        ledgers[f].debit(i, m.amount)?;
        let j = ledgers[t].index_or_claim(&m.mint)?;
        ledgers[t].credit(j, m.amount)?;
    }
    for (info, ledger) in ledger_infos.iter().zip(ledgers.iter()) {
        store_ledger(info, ledger)?;
    }

    // Consumed before the callback, not after: single-use is ours to guarantee, and a
    // callback that never sees a live receipt cannot replay one. What replaces it as proof of
    // payment is the signature below.
    ctx.accounts.receipt.try_borrow_mut_data()?.fill(0);

    // Handed over so the callback can close it and take the rent back. Only the account's
    // owner may close it, and only the member program can sign for the sponsor that paid —
    // the two requirements sit in different programs, so ownership has to move first.
    ctx.accounts.receipt.to_account_info().assign(&member_program);

    let mut metas = vec![
        AccountMeta::new(ctx.accounts.receipt.key(), false),
        AccountMeta::new_readonly(ctx.accounts.vault_authority.key(), true),
    ];
    let mut infos = vec![
        ctx.accounts.receipt.to_account_info(),
        ctx.accounts.vault_authority.to_account_info(),
        ctx.accounts.callback_program.to_account_info(),
    ];
    for a in forwarded {
        metas.push(AccountMeta {
            pubkey: a.key(),
            // A callback signs its own work with its own seeds; it inherits nothing.
            is_signer: false,
            is_writable: a.is_writable,
        });
        infos.push(a.clone());
    }
    // Owner 0 travels in the data because the receipt has been zeroed by now.
    let mut data = Vec::with_capacity(8 + 32 + args.len());
    data.extend_from_slice(&callback_disc);
    data.extend_from_slice(&owners[0].to_bytes());
    data.extend_from_slice(&args);

    invoke_signed(
        &Instruction { program_id: member_program, accounts: metas, data },
        &infos,
        &[&[&[ctx.bumps.vault_authority]]],
    )?;
    Ok(())
}
