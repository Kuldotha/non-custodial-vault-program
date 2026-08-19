use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::state::*;

/// Opens an empty ledger for a program's PDA, sponsored by whoever pays. A PDA can neither pay
/// rent nor sign a System transfer, and `deposit` refuses off-curve owners, so its ledger has
/// to be creatable on its own.
///
/// Members: `[member_program, payer]` — the program because the rollup's filter checks an
/// instruction's top-level program, the sponsor because it is the only member able to sign an
/// RPC challenge. The PDA can do neither; naming it would be decoration.
#[derive(Accounts)]
#[instruction(slots: u16, member_program: Pubkey, owner_seeds: Vec<Vec<u8>>)]
pub struct OpenPdaLedger<'info> {
    /// CHECK: the program's PDA, signing via invoke_signed — see the derivation check.
    pub owner: Signer<'info>,

    /// Pays the rent and becomes the recorded rent payer: who it returns to on close, and
    /// the only account that may later grow the ledger.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: created here and validated by hand — see `create_ledger_account`.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    /// CHECK: the ledger's permission, created here alongside it.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program, pinned to its known address.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<OpenPdaLedger>,
    slots: u16,
    member_program: Pubkey,
    owner_seeds: Vec<Vec<u8>>,
) -> Result<()> {
    verify_pda_owner(&ctx.accounts.owner, &member_program, &owner_seeds)?;

    let ledger_info = ctx.accounts.ledger.to_account_info();
    require!(ledger_info.data_is_empty(), VaultError::LedgerExists);
    require!(slots > 0 && slots <= MAX_SLOTS, VaultError::BadSlotCount);

    let mut ledger = create_ledger_account_sized(
        &ledger_info,
        &ctx.accounts.payer,
        &ctx.accounts.owner,
        &ctx.accounts.system_program,
        ctx.bumps.ledger,
        slots as usize,
    )?;
    ledger.authorized = member_program;
    store_ledger(&ledger_info, &ledger)?;

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
        .invoke_signed(&[&[b"ledger", ctx.accounts.owner.key().as_ref(), &[ctx.bumps.ledger]]])
        .map_err(|_| error!(VaultError::PermissionFailed))
}

/// The proof that `member_program` is the program behind `owner`: its seeds must derive the
/// owner's address under that program, and address spaces cannot collide across programs — so
/// the owner's signature, which callers require, can only have come from it.
///
/// Do not replace this by reading the owner account's `owner` field: delegation and
/// reassignment rewrite it.
pub fn verify_pda_owner(
    owner: &impl Key,
    member_program: &Pubkey,
    owner_seeds: &[Vec<u8>],
) -> Result<()> {
    let seeds: Vec<&[u8]> = owner_seeds.iter().map(|s| s.as_slice()).collect();
    let derived = Pubkey::create_program_address(&seeds, member_program)
        .map_err(|_| error!(VaultError::MemberProgramMismatch))?;
    require_keys_eq!(derived, owner.key(), VaultError::MemberProgramMismatch);
    Ok(())
}
