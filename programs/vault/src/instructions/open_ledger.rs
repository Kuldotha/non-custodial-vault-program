use anchor_lang::prelude::*;

use crate::state::*;

/// Opens an empty ledger, funding nothing.
///
/// `deposit` opens a ledger as a side effect of putting value in, which serves a wallet well —
/// it has lamports and can pay its own way. A **program's** PDA has neither: it cannot sign a
/// System transfer and holds nothing to transfer. Its ledger is filled by `settle` from a human
/// who already has a balance, and settle can only move value between ledgers that exist. So the
/// ledger has to be creatable on its own.
///
/// That is the whole reason this instruction exists, and it is why a program needs no special
/// path in and out of the vault: open the ledger once, then settle. Nothing about a program's
/// treasury is a different shape from anyone else's.
///
/// No permission is created here. A ledger that stays on basenet, or is delegated to an ordinary
/// rollup, needs none — and paying rent for one is pure waste in those cases. Privacy is opted
/// into with `open_permission`, which `delegate_ledger` then insists on.
///
/// `slots` sizes it up front. A game paying out fourteen tokens wants room for fourteen at
/// creation rather than growing a slot at a time, and unlike `deposit` there is no later call to
/// grow it — the settles that fill it move value, not rent.
///
/// A **wallet pays its own rent**; only a PDA may be sponsored, because a PDA claimed by its
/// program can neither hold lamports usefully nor source a System transfer. The sponsorship
/// moves nothing: the ledger records who paid, and that is who the rent returns to on close and
/// the only key that may grow it. Without that, paying somebody's rent would be a way to hand
/// them value — slowly, but genuinely.
///
/// Because the PDA no longer has to pay, it can stay **program-owned**, which is what lets
/// `open_permission` read the program off it afterwards.
#[derive(Accounts)]
#[instruction(slots: u16)]
pub struct OpenLedger<'info> {
    /// CHECK: the ledger's owner — a wallet, or a program's PDA signing via invoke_signed.
    /// Signing is what makes this the owner's own decision rather than a stranger's.
    pub owner: Signer<'info>,

    /// Pays the rent, and becomes the recorded rent payer when the owner is a PDA — so this is
    /// who it comes back to, and the only account that may later grow or close the ledger. Must
    /// be the owner itself when the owner is a wallet.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: created here and validated by hand — see `create_ledger_account`.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<OpenLedger>, slots: u16) -> Result<()> {
    let ledger_info = ctx.accounts.ledger.to_account_info();
    require!(ledger_info.data_is_empty(), VaultError::LedgerExists);
    require!(slots > 0, VaultError::LedgerFull);

    let ledger = create_ledger_account_sized(
        &ledger_info,
        &ctx.accounts.payer,
        &ctx.accounts.owner,
        &ctx.accounts.system_program,
        ctx.bumps.ledger,
        slots as usize,
    )?;
    store_ledger(&ledger_info, &ledger)
}
