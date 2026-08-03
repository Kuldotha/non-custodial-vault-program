use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};

use crate::state::*;

/// Makes a ledger private, once and irreversibly.
///
/// Separate from `open_ledger` because privacy is not always wanted: a ledger that lives on
/// basenet, or is delegated to an ordinary rollup, is readable there regardless and a permission
/// buys it nothing but rent. `delegate_ledger` is what insists on one, so the cost falls on the
/// ledgers that actually need it.
///
/// Being separate is also what lets a **program's** ledger name its program. The two facts
/// cannot both be true in one instruction: paying rent through the System program requires the
/// account to be System-owned, while learning which program stands behind a PDA requires it to
/// be program-owned. So the order is: open the ledger while the PDA still belongs to System,
/// let the program claim it, then come here — by which point `owner.owner` names the program.
///
/// Members, both derived and neither passed:
///
/// - the **owner**, so a player can read their own balance inside a private rollup;
/// - the **program behind it**, when the owner is a claimed PDA. A program has to reach its own
///   ledgers to settle them, and a rollup's filter only ever sees an instruction's *top-level*
///   program — a PDA in the member list would not admit it.
///
/// Reading the program off the account instead of taking it as an argument is what stops this
/// being a way to smuggle a third party into somebody else's ACL. Membership is fixed here for
/// the life of the account: there is no instruction that widens it later.
#[derive(Accounts)]
pub struct OpenPermission<'info> {
    /// CHECK: the ledger's owner — a wallet, or a program's PDA signing via invoke_signed.
    /// Signing is what makes this the owner's decision and nobody else's.
    pub owner: Signer<'info>,

    /// Pays the permission's rent. Must be the ledger's recorded rent payer, so that both
    /// rents come from one account and both return to it — buying no membership and no access.
    #[account(mut)]
    pub payer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
        constraint = ledger.rent_payer == payer.key() @ VaultError::OffCurveOwnerNotAllowed,
    )]
    pub ledger: Account<'info, Ledger>,

    /// CHECK: created here, by the permission program.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program.
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<OpenPermission>) -> Result<()> {
    require!(
        ctx.accounts.permission.data_is_empty(),
        VaultError::LedgerExists
    );

    let owner_info = ctx.accounts.owner.to_account_info();
    let mut members = vec![Member { flags: 0, pubkey: ctx.accounts.owner.key() }];
    if *owner_info.owner != anchor_lang::system_program::ID {
        members.push(Member { flags: 0, pubkey: *owner_info.owner });
    }

    let ledger_info = ctx.accounts.ledger.to_account_info();
    CreatePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .permissioned_account(&ledger_info)
        .permission(&ctx.accounts.permission.to_account_info())
        .payer(&ctx.accounts.payer.to_account_info())
        .system_program(&ctx.accounts.system_program.to_account_info())
        .args(MembersArgs { members: Some(members) })
        .invoke_signed(&[&[
            b"ledger",
            ctx.accounts.owner.key().as_ref(),
            &[ctx.accounts.ledger.bump],
        ]])
        .map_err(|e| {
            error!(VaultError::PermissionFailed)
                .with_source(source!())
                .with_values(("cpi", e.to_string()))
        })
}
