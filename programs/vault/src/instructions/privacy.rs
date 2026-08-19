use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::{
    ClosePermissionCpiBuilder, CreatePermissionCpiBuilder,
};
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::instructions::open_pda_ledger::verify_pda_owner;
use crate::state::*;

/// Privacy's verbs — and privacy's only verbs.
///
/// Every ledger is born private, and `close_ledger` buries the permission with it. Some are
/// meant to be watched though — a progressive pot is worthless as a secret — so the opt-out is
/// explicit: `make_public` deletes the permission, and the `make_*_ledger_private` pair puts
/// it back, split like the open instructions because recreating one must derive its members.
///
/// All of them are basenet-only for free: `Account<Ledger>` fails on a delegated ledger, so
/// privacy cannot be flipped under a live rollup session.

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

    /// CHECK: the MagicBlock permission program, pinned to its known address.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,
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

    /// CHECK: the MagicBlock permission program, pinned to its known address.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn make_wallet_ledger_private_handler(ctx: Context<MakeWalletLedgerPrivate>) -> Result<()> {
    require!(!is_pda(&ctx.accounts.owner.key()), VaultError::OwnerNotWallet);
    require!(
        ctx.accounts.permission.data_is_empty(),
        VaultError::PermissionExists
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

    /// CHECK: the MagicBlock permission program, pinned to its known address.
    #[account(address = PERMISSION_PROGRAM_ID)]
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
        VaultError::PermissionExists
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
