use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};

use crate::state::*;

/// Opens an empty ledger for a wallet, funding nothing.
///
/// `deposit` opens a wallet's ledger as a side effect of putting value in, which is how one
/// normally comes to exist — this is for opening one deliberately, sized up front. `amount`
/// zero on a deposit does the same at the default size.
///
/// Wallets only, asserted rather than assumed: its PDA counterpart differs in who pays, who
/// the permission names, and what has to be proven — too much to share an instruction with.
/// The owner pays its own rent, because rent returns on close and paying somebody else's
/// would be a way to hand them money.
///
/// `slots` sizes it up front, capped at [`MAX_SLOTS`]: rent scales with the count and is paid
/// on the spot, so a zero too many is an expensive mistake.
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

    /// CHECK: the MagicBlock permission program.
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

    // One member: the owner. That is what lets a player read their own balance inside a
    // private rollup, and nobody else read it.
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
