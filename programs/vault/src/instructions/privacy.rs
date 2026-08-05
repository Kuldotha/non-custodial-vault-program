use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::{
    ClosePermissionCpiBuilder, CreatePermissionCpiBuilder,
};
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};

use crate::state::*;

/// Privacy's two verbs — and privacy's only verbs.
///
/// Every ledger is born private: `deposit` and `open_ledger` create the permission with it,
/// and `close_ledger` buries them together. Some ledgers are meant to be watched, though — a
/// progressive pot is worthless as a secret — and a copy of the figure in a public account
/// would be a second number free to drift from the first. So the opt-out is explicit instead:
/// `make_public` deletes the permission, `make_private` puts it back.
///
/// Both take the **owner's signature and nothing weaker** — privacy is the owner's to give up,
/// and only the owning program can sign for a PDA's ledger. Both also take the recorded rent
/// payer, so the permission's rent keeps coming from and returning to one account. And both
/// insist the ledger is at home: `Account<Ledger>` checks the account's owner, which a
/// delegated ledger fails — flipping privacy under a live rollup session would race whatever
/// the validator has already admitted.

#[derive(Accounts)]
pub struct MakePublic<'info> {
    /// CHECK: the ledger's owner — a wallet, or a program's PDA signing via invoke_signed.
    pub owner: Signer<'info>,

    #[account(
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,

    /// CHECK: closed here.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// Receives the rent and must sign, as the permission program requires.
    #[account(mut, address = ledger.rent_payer @ VaultError::NotRentPayer)]
    pub payer: Signer<'info>,

    /// CHECK: the MagicBlock permission program.
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn make_public_handler(ctx: Context<MakePublic>) -> Result<()> {
    let ledger_info = ctx.accounts.ledger.to_account_info();
    ClosePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .payer(&ctx.accounts.payer.to_account_info())
        .authority(&ledger_info, false)
        .permissioned_account(&ledger_info, true)
        .permission(&ctx.accounts.permission.to_account_info())
        .invoke_signed(&[&[
            b"ledger",
            ctx.accounts.owner.key().as_ref(),
            &[ctx.accounts.ledger.bump],
        ]])
        .map_err(|_| error!(VaultError::PermissionFailed))?;
    Ok(())
}

#[derive(Accounts)]
pub struct MakePrivate<'info> {
    /// CHECK: the ledger's owner — a wallet, or a program's PDA signing via invoke_signed.
    pub owner: Signer<'info>,

    /// Pays the permission's rent. Must be the ledger's recorded rent payer, so both rents
    /// come from one account and both return to it — buying no membership and no access.
    #[account(mut, address = ledger.rent_payer @ VaultError::NotRentPayer)]
    pub payer: Signer<'info>,

    #[account(
        mut,
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,

    /// CHECK: created here, by the permission program.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program.
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn make_private_handler(ctx: Context<MakePrivate>) -> Result<()> {
    require!(
        ctx.accounts.permission.data_is_empty(),
        VaultError::LedgerExists
    );

    // The standard member set, identical to what open_ledger grants at creation: the owner;
    // for a PDA also its program and its sponsor. Derived, never passed — this must not be a
    // way to smuggle a third party into an ACL.
    let owner_info = ctx.accounts.owner.to_account_info();
    let mut members = vec![Member { flags: 0, pubkey: ctx.accounts.owner.key() }];
    if *owner_info.owner != anchor_lang::system_program::ID {
        members.push(Member { flags: 0, pubkey: *owner_info.owner });
        members.push(Member { flags: 0, pubkey: ctx.accounts.payer.key() });
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
        .map_err(|_| error!(VaultError::PermissionFailed))
}
