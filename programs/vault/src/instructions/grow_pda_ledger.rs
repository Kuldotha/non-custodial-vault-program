use anchor_lang::prelude::*;

use crate::state::*;

/// Adds slots to an existing **program** ledger.
///
/// `deposit` grows a wallet's ledger as it goes, but it refuses an off-curve owner — so without
/// this a program's ledger is stuck at whatever `open_pda_ledger` gave it, and a game that adds
/// a fifteenth payout token has nowhere to put it. Wallets have no business here, and the
/// off-curve assertion says so instead of leaving it implied.
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
pub struct GrowPdaLedger<'info> {
    /// CHECK: the program's PDA, signing via invoke_signed.
    pub owner: Signer<'info>,

    /// Funds the extra slots. `ensure_headroom` requires it to be the recorded rent payer.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: loaded and written by hand. As `Account<Ledger>` this would silently do nothing:
    /// Anchor serialises its own copy on exit, after the handler, so the grown `entries` vector
    /// would be overwritten by the original's shorter one — the account keeps the extra bytes and
    /// the rent, and `capacity()` still reports the old count.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<GrowPdaLedger>, min_free: u16, step: u16) -> Result<()> {
    require!(is_pda(&ctx.accounts.owner.key()), VaultError::OwnerNotPda);
    let info = ctx.accounts.ledger.to_account_info();
    let mut ledger = load_ledger(&info)?;
    // The seeds constraint re-derives the address; this is what `has_one = owner` was doing.
    require_keys_eq!(ledger.owner, ctx.accounts.owner.key(), VaultError::BadLedgerOwner);

    ensure_headroom(
        &info,
        &mut ledger,
        &ctx.accounts.payer,
        &ctx.accounts.system_program,
        min_free,
        step,
    )?;

    store_ledger(&info, &ledger)
}
