use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::{commit, delegate};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::commit_and_undelegate_accounts;

/// The reserves (`["vault"]` and the vault's ATA per mint) are never delegated — only
/// ledgers are.
/// That is exactly why `settle` can be pure bookkeeping inside the rollup.
#[delegate]
#[derive(Accounts)]
pub struct DelegateLedger<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: the ledger owner; must sign to hand their own ledger to the rollup.
    pub owner: Signer<'info>,

    /// CHECK: delegated by the SDK, which reassigns ownership to the delegation program.
    #[account(mut, del)]
    pub ledger: AccountInfo<'info>,

    /// CHECK: the ledger's basenet permission account. Must already exist and be owned by the
    /// permission program, or the ledger would be readable in the rollup until one lands.
    pub permission: UncheckedAccount<'info>,
}

pub fn delegate_handler(ctx: Context<DelegateLedger>, validator: Option<Pubkey>) -> Result<()> {
    // Refuse to delegate an unprotected ledger: the permission has to be on basenet *first*.
    let perm = &ctx.accounts.permission;
    require!(
        perm.owner == &ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID
            && !perm.data_is_empty(),
        crate::state::VaultError::MissingPermission
    );

    ctx.accounts.delegate_ledger(
        &ctx.accounts.payer,
        &[b"ledger", ctx.accounts.owner.key().as_ref()],
        DelegateConfig {
            commit_frequency_ms: u32::MAX,
            validator,
        },
    )?;
    Ok(())
}

/// Ends a session. The commit is implicit — state is pushed back to basenet as part of
/// undelegating, so there is no separate commit instruction to forget.
#[commit]
#[derive(Accounts)]
pub struct Undelegate<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: the delegated ledger being pushed back to basenet.
    #[account(mut)]
    pub ledger: AccountInfo<'info>,
}

pub fn undelegate_handler(ctx: Context<Undelegate>) -> Result<()> {
    commit_and_undelegate_accounts(
        &ctx.accounts.payer,
        vec![&ctx.accounts.ledger.to_account_info()],
        &ctx.accounts.magic_context,
        &ctx.accounts.magic_program,
        None,
    )?;
    Ok(())
}
