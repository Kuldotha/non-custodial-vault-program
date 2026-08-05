use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};

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
/// `slots` sizes it up front, capped at [`MAX_SLOTS`]. A game paying out fourteen tokens wants
/// room for fourteen at creation rather than growing a slot at a time, and unlike `deposit` there
/// is no later call to grow it — the settles that fill it move value, not rent. The cap is there
/// because rent scales with the number and is paid on the spot: a zero too many is an expensive
/// mistake, and nothing realistic comes near it.
///
/// A **wallet pays its own rent**; only a PDA may be sponsored, because a PDA claimed by its
/// program can neither hold lamports usefully nor source a System transfer. The sponsorship
/// moves nothing: the ledger records who paid, and that is who the rent returns to on close and
/// the only key that may grow it. Without that, paying somebody's rent would be a way to hand
/// them value — slowly, but genuinely.
///
/// Because the PDA no longer has to pay, it can stay **program-owned**, which is what lets the
/// permission name the program that owns it.
///
/// The permission is created here, with the ledger, and dies with it in `close_ledger`. Privacy
/// is the vault's to enforce rather than each program's to negotiate: a ledger that could be made
/// readable by whoever delegated it would leave programs opening and closing permissions against
/// each other, never settling. One lifecycle, no verbs of its own.
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

    /// CHECK: the ledger's permission, created here alongside it.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program.
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<OpenLedger>, slots: u16) -> Result<()> {
    let ledger_info = ctx.accounts.ledger.to_account_info();
    require!(ledger_info.data_is_empty(), VaultError::LedgerExists);
    require!(
        slots > 0 && slots <= MAX_SLOTS,
        VaultError::BadSlotCount
    );

    let ledger = create_ledger_account_sized(
        &ledger_info,
        &ctx.accounts.payer,
        &ctx.accounts.owner,
        &ctx.accounts.system_program,
        ctx.bumps.ledger,
        slots as usize,
    )?;
    store_ledger(&ledger_info, &ledger)?;

    // Named members: the owner, plus the program that owns it when the owner is a PDA — which is
    // how a game reaches the ledgers it owns inside a private rollup.
    let owner_info = ctx.accounts.owner.to_account_info();
    let mut members = vec![Member { flags: 0, pubkey: ctx.accounts.owner.key() }];
    if *owner_info.owner != anchor_lang::system_program::ID {
        members.push(Member { flags: 0, pubkey: *owner_info.owner });
    }

    CreatePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .permissioned_account(&ledger_info)
        .permission(&ctx.accounts.permission.to_account_info())
        .payer(&ctx.accounts.payer.to_account_info())
        .system_program(&ctx.accounts.system_program.to_account_info())
        .args(MembersArgs { members: Some(members) })
        .invoke_signed(&[&[b"ledger", ctx.accounts.owner.key().as_ref(), &[ctx.bumps.ledger]]])
        .map_err(|_| error!(VaultError::PermissionFailed))
}
