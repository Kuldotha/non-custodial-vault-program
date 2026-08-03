use anchor_lang::prelude::*;

use crate::state::*;

/// Adds slots to an existing ledger.
///
/// `deposit` grows a wallet's ledger as it goes, but it refuses an off-curve owner — so without
/// this a program's ledger is stuck at whatever `open_ledger` gave it, and a game that adds a
/// fifteenth payout token has nowhere to put it.
///
/// Nothing about the rent model changes. The rent payer recorded at creation is the only account
/// that may fund the increase, and the whole amount still comes back to them when the ledger is
/// closed — so a sponsor who opened a program's ledger keeps paying for it and keeps getting it
/// back, and nothing crosses between parties. `rent_payer` stays immutable.
///
/// A sponsor who has gone away leaves the ledger at its current size. Nothing is lost: closing it
/// still returns every balance to the owner and the rent to the payer. Handling that case would
/// mean letting `rent_payer` change, which is the thing that keeps rent uninteresting.
#[derive(Accounts)]
pub struct GrowLedger<'info> {
    /// CHECK: the ledger's owner — a wallet, or a program's PDA signing via invoke_signed.
    pub owner: Signer<'info>,

    /// Funds the extra slots. `ensure_headroom` requires it to be the recorded rent payer.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<GrowLedger>, min_free: u16, step: u16) -> Result<()> {
    let info = ctx.accounts.ledger.to_account_info();
    let mut ledger = ctx.accounts.ledger.clone().into_inner();

    ensure_headroom(
        &info,
        &mut ledger,
        &ctx.accounts.payer,
        &ctx.accounts.system_program,
        min_free,
        step,
    )?;

    // The account grew underneath Anchor, so write the longer ledger out by hand — its own
    // serialisation on exit would still be sized for the shorter one.
    store_ledger(&info, &ledger)
}
