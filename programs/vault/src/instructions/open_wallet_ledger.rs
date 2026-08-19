use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::state::*;

/// Opens an empty ledger for a wallet at a chosen size. A zero-amount `deposit` does the same
/// at the default size, and is how most ledgers come to exist; this is for sizing one up front.
#[derive(Accounts)]
#[instruction(slots: u16)]
pub struct OpenWalletLedger<'info> {
    /// Pays its own rent, which is what makes it the rent payer on record.
    #[account(mut)]
    pub owner: Signer<'info>,

    /// CHECK: created here and validated by hand — see `create_ledger_account`.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    /// CHECK: the ledger's permission, created here alongside it.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program, pinned to its known address.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<OpenWalletLedger>, slots: u16) -> Result<()> {
    require!(!is_pda(&ctx.accounts.owner.key()), VaultError::OwnerNotWallet);
    let ledger_info = ctx.accounts.ledger.to_account_info();
    require!(ledger_info.data_is_empty(), VaultError::LedgerExists);
    require!(slots > 0 && slots <= MAX_SLOTS, VaultError::BadSlotCount);

    let ledger = create_ledger_account_sized(
        &ledger_info,
        &ctx.accounts.owner,
        &ctx.accounts.owner,
        &ctx.accounts.system_program,
        ctx.bumps.ledger,
        slots as usize,
    )?;
    store_ledger(&ledger_info, &ledger)?;

    // One member: the owner — what lets a player read their own balance in a private rollup,
    // and nobody else.
    CreatePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .permissioned_account(&ledger_info)
        .permission(&ctx.accounts.permission.to_account_info())
        .payer(&ctx.accounts.owner.to_account_info())
        .system_program(&ctx.accounts.system_program.to_account_info())
        .args(MembersArgs {
            members: Some(vec![Member { flags: 0, pubkey: ctx.accounts.owner.key() }]),
        })
        .invoke_signed(&[&[b"ledger", ctx.accounts.owner.key().as_ref(), &[ctx.bumps.ledger]]])
        .map_err(|_| error!(VaultError::PermissionFailed))
}
