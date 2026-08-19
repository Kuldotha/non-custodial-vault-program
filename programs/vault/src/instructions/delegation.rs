use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::{commit, delegate};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::commit_and_undelegate_accounts;

use crate::state::{load_ledger, VaultError};

/// Only ledgers are delegated — the reserves stay on basenet, which is what keeps `settle`
/// pure bookkeeping inside the rollup.
#[delegate]
#[derive(Accounts)]
pub struct DelegateLedger<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    pub owner: Signer<'info>,

    /// CHECK: reassigned to the delegation program by the SDK.
    #[account(mut, del)]
    pub ledger: AccountInfo<'info>,
}

pub fn delegate_handler(ctx: Context<DelegateLedger>, validator: Option<Pubkey>) -> Result<()> {
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

/// Ends a session; the commit is implicit. Rollup-only, and unlike delegating it accepts the
/// ledger's session key as well as its owner.
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
    // Nothing downstream checks who is asking — the delegation program only asks whether the
    // account is delegated.
    let ledger = load_ledger(&ctx.accounts.ledger)?;
    require!(
        ledger.may_end_session(&ctx.accounts.payer.key()),
        VaultError::NotAuthorizedToConsent
    );

    commit_and_undelegate_accounts(
        &ctx.accounts.payer,
        vec![&ctx.accounts.ledger.to_account_info()],
        &ctx.accounts.magic_context,
        &ctx.accounts.magic_program,
        None,
    )?;
    Ok(())
}
