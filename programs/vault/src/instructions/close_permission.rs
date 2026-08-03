use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::ClosePermissionCpiBuilder;

use crate::state::*;

/// Closes a ledger's permission, leaving the ledger itself alone.
///
/// **This makes the ledger readable in a private rollup.** Membership is otherwise fixed at
/// creation and no instruction can widen it — removing the permission entirely is the one way
/// past that, which is why it takes the owner's signature and nothing weaker. A ledger's privacy
/// is the owner's to give up.
///
/// The permission program requires the permissioned account to sign, and only this program can
/// sign for a ledger PDA, so this cannot be done from outside the vault.
#[derive(Accounts)]
pub struct ClosePermission<'info> {
    /// CHECK: the ledger's owner — a wallet, or a program's PDA signing via invoke_signed.
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,

    /// CHECK: closed here.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// Receives the rent and must sign, as the permission program requires.
    #[account(mut, address = ledger.rent_payer @ VaultError::OffCurveOwnerNotAllowed)]
    pub payer: Signer<'info>,

    /// CHECK: the MagicBlock permission program.
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<ClosePermission>) -> Result<()> {
    let ledger_info = ctx.accounts.ledger.to_account_info();
    ClosePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .payer(&ctx.accounts.payer.to_account_info())
        .authority(&ledger_info, false)
        .permissioned_account(&ledger_info, true)
        .permission(&ctx.accounts.permission.to_account_info())
        .invoke_signed(&[&[
            b"ledger",
            ctx.accounts.owner.key().as_ref(),
            &[ctx.accounts.ledger.bump],
        ]])
        .map_err(|_| error!(VaultError::PermissionFailed))?;
    Ok(())
}
