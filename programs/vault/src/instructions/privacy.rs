use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::{
    ClosePermissionCpiBuilder, CreatePermissionCpiBuilder,
};
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};

use crate::instructions::open_pda_ledger::verify_pda_owner;
use crate::state::*;

/// Privacy's verbs — and privacy's only verbs.
///
/// Every ledger is born private: the open instructions create the permission with it, and
/// `close_ledger` buries them together. Some ledgers are meant to be watched, though — a
/// progressive pot is worthless as a secret, and a copy of the figure in a public account
/// would be a second number free to drift. So the opt-out is explicit: `make_public` deletes
/// the permission, and the two `make_*_ledger_private` variants put it back.
///
/// `make_public` serves both kinds of owner in one instruction because deleting has no
/// members to derive — nothing about it depends on what the owner is. Recreating does, which
/// is why the private side is split like the open side and proves its member program the same
/// way.
///
/// Everything here takes the **owner's signature and nothing weaker** — privacy is the
/// owner's to give up — plus the recorded rent payer, so the permission's rent keeps coming
/// from and returning to one account. And everything insists the ledger is at home:
/// `Account<Ledger>` checks the account's owner, which a delegated ledger fails — flipping
/// privacy under a live rollup session would race whatever the validator has admitted.

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
pub struct MakeWalletLedgerPrivate<'info> {
    /// Also the rent payer: a wallet funds its own ledger, and both rents return to it.
    #[account(mut, address = ledger.rent_payer @ VaultError::NotRentPayer)]
    pub owner: Signer<'info>,

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

pub fn make_wallet_ledger_private_handler(ctx: Context<MakeWalletLedgerPrivate>) -> Result<()> {
    require!(!is_pda(&ctx.accounts.owner.key()), VaultError::OwnerNotWallet);
    require!(
        ctx.accounts.permission.data_is_empty(),
        VaultError::LedgerExists
    );

    let ledger_info = ctx.accounts.ledger.to_account_info();
    CreatePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .permissioned_account(&ledger_info)
        .permission(&ctx.accounts.permission.to_account_info())
        .payer(&ctx.accounts.owner.to_account_info())
        .system_program(&ctx.accounts.system_program.to_account_info())
        .args(MembersArgs {
            members: Some(vec![Member { flags: 0, pubkey: ctx.accounts.owner.key() }]),
        })
        .invoke_signed(&[&[
            b"ledger",
            ctx.accounts.owner.key().as_ref(),
            &[ctx.accounts.ledger.bump],
        ]])
        .map_err(|_| error!(VaultError::PermissionFailed))
}

#[derive(Accounts)]
#[instruction(member_program: Pubkey, owner_seeds: Vec<Vec<u8>>)]
pub struct MakePdaLedgerPrivate<'info> {
    /// CHECK: the program's PDA, signing via invoke_signed — see the derivation check.
    pub owner: Signer<'info>,

    /// Pays the permission's rent. Must be the recorded rent payer, so both rents come from
    /// and return to one account — buying no membership beyond what the sponsor already has.
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

pub fn make_pda_ledger_private_handler(
    ctx: Context<MakePdaLedgerPrivate>,
    member_program: Pubkey,
    owner_seeds: Vec<Vec<u8>>,
) -> Result<()> {
    require!(
        ctx.accounts.permission.data_is_empty(),
        VaultError::LedgerExists
    );
    verify_pda_owner(&ctx.accounts.owner, &member_program, &owner_seeds)?;

    let ledger_info = ctx.accounts.ledger.to_account_info();
    CreatePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .permissioned_account(&ledger_info)
        .permission(&ctx.accounts.permission.to_account_info())
        .payer(&ctx.accounts.payer.to_account_info())
        .system_program(&ctx.accounts.system_program.to_account_info())
        .args(MembersArgs {
            members: Some(vec![
                Member { flags: 0, pubkey: member_program },
                Member { flags: 0, pubkey: ctx.accounts.payer.key() },
            ]),
        })
        .invoke_signed(&[&[
            b"ledger",
            ctx.accounts.owner.key().as_ref(),
            &[ctx.accounts.ledger.bump],
        ]])
        .map_err(|_| error!(VaultError::PermissionFailed))
}
