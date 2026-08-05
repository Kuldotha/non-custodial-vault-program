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

    /// CHECK: the ledger's permission, when it has one. Unread here — see the handler.
    pub permission: UncheckedAccount<'info>,
}

pub fn delegate_handler(ctx: Context<DelegateLedger>, validator: Option<Pubkey>) -> Result<()> {
    // No permission is required. This vault settles across rollup boundaries and gates who may
    // pay; it does not decide who may read. Whoever delegates a ledger chooses its destination,
    // so whoever delegates is the only party that can know whether privacy is needed — and on an
    // ordinary rollup a permission does nothing but cost rent.
    //
    // A ledger bound for a private validator must therefore have `open_permission` called first,
    // by the client. Requiring one here proved only that an account existed, not that anything
    // was protected.

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
