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
/// `slots` sizes it up front. A game paying out fourteen tokens wants room for fourteen at
/// creation rather than growing a slot at a time, and unlike `deposit` there is no later call to
/// grow it — the settles that fill it move value, not rent.
///
/// The **payer** covers rent and is not the owner: a PDA cannot pay. That is not a way to move
/// value, since rent buys an empty account and nothing is credited to anyone.
#[derive(Accounts)]
#[instruction(slots: u16)]
pub struct OpenLedger<'info> {
    /// CHECK: the ledger's owner — a wallet, or a program's PDA signing via invoke_signed.
    /// Signing is what makes this the owner's own decision rather than a stranger's.
    pub owner: Signer<'info>,

    /// Pays the rent. Buys no access: it is not named in the permission.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: created here and validated by hand — see `create_ledger_account`.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    /// CHECK: the ledger's basenet permission, created here so the ledger is never readable
    /// in a rollup even for an instant.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program.
    pub permission_program: UncheckedAccount<'info>,

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
    store_ledger(&ledger_info, &ledger)?;

    // Members, all derived, never passed:
    //
    // - the **owner**, so a player can read their own balance inside a private rollup;
    // - the **program behind it**, when the owner is a PDA that program has claimed. A program
    //   has to reach its own ledgers to settle them, and the rollup's filter only ever sees an
    //   instruction's top-level program — a PDA in the list would not admit it.
    //
    // Reading the program off the account rather than accepting it as an argument is what keeps
    // this from being a way to smuggle a third party into somebody's ACL.
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
        .map_err(|e| {
            error!(VaultError::PermissionFailed)
                .with_source(source!())
                .with_values(("cpi", e.to_string()))
        })
}
