use anchor_lang::prelude::*;

use crate::state::*;

/// Adds slots to an existing **program** ledger. Wallets grow through `deposit`, which refuses
/// an off-curve owner, so this is the only way a program's ledger gets bigger.
///
/// Only the recorded `rent_payer` may fund the increase and it never changes, so a sponsor who
/// has gone away leaves the ledger at its current size — the price of never letting rent cross
/// between parties.
#[derive(Accounts)]
pub struct GrowPdaLedger<'info> {
    pub owner: Signer<'info>,

    /// Funds the extra slots. `ensure_headroom` requires it to be the recorded rent payer.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: loaded and written by hand — see `ensure_headroom` for why `Account<Ledger>`
    /// cannot be used here.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<GrowPdaLedger>, min_free: u16, slot_increase: u16) -> Result<()> {
    require!(is_pda(&ctx.accounts.owner.key()), VaultError::OwnerNotPda);
    let info = ctx.accounts.ledger.to_account_info();
    let mut ledger = load_ledger(&info)?;
    require_keys_eq!(ledger.owner, ctx.accounts.owner.key(), VaultError::BadLedgerOwner);

    ensure_headroom(
        &info,
        &mut ledger,
        &ctx.accounts.payer,
        &ctx.accounts.system_program,
        min_free,
        slot_increase,
    )?;

    store_ledger(&info, &ledger)
}
