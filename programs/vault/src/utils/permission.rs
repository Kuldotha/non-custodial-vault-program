use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};

use ephemeral_rollups_sdk::access_control::instructions::{
    ClosePermissionCpiBuilder, CreatePermissionCpiBuilder,
};
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};

use crate::error::VaultError;

/// Creates a ledger's basenet permission naming `members`, signed by the ledger's own seeds.
pub fn create<'a>(
    permission_program: &AccountInfo<'a>,
    permissioned_account: &AccountInfo<'a>,
    permission: &AccountInfo<'a>,
    payer: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    members: Vec<Member>,
    signer_seeds: &[&[u8]],
) -> Result<(), ProgramError> {
    CreatePermissionCpiBuilder::new(permission_program)
        .permissioned_account(permissioned_account)
        .permission(permission)
        .payer(payer)
        .system_program(system_program)
        .args(MembersArgs { members: Some(members) })
        .invoke_signed(&[signer_seeds])
        .map_err(|_| ProgramError::from(VaultError::PermissionFailed))
}

/// Closes a ledger's permission; the ledger authorises its own closure by signing with its seeds.
pub fn close<'a>(
    permission_program: &AccountInfo<'a>,
    permissioned_account: &AccountInfo<'a>,
    permission: &AccountInfo<'a>,
    payer: &AccountInfo<'a>,
    signer_seeds: &[&[u8]],
) -> Result<(), ProgramError> {
    ClosePermissionCpiBuilder::new(permission_program)
        .payer(payer)
        .authority(permissioned_account, false)
        .permissioned_account(permissioned_account, true)
        .permission(permission)
        .invoke_signed(&[signer_seeds])
        .map_err(|_| ProgramError::from(VaultError::PermissionFailed))
}

/// Keeps private membership and flags intact, including the vault's implicit caller access.
pub fn add_caller<'a>(
    permission_program: &AccountInfo<'a>, account: &AccountInfo<'a>, permission: &AccountInfo<'a>,
    caller: Pubkey, seeds: &[&[u8]],
) -> Result<(), ProgramError> {
    use borsh::BorshDeserialize;
    use ephemeral_rollups_sdk::access_control::{instructions::UpdatePermissionCpiBuilder, structs::Permission};
    use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;
    if *permission_program.key != PERMISSION_PROGRAM_ID || *permission.owner != PERMISSION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    if Permission::find_pda(account.key).0 != *permission.key { return Err(ProgramError::InvalidSeeds); }
    let data = permission.try_borrow_data()?;
    let parsed = Permission::deserialize(&mut &data[..]).map_err(|_| ProgramError::InvalidAccountData)?;
    if parsed.discriminator != 0 || parsed.permissioned_account != *account.key { return Err(ProgramError::InvalidAccountData); }
    let Some(mut members) = parsed.members else { return Ok(()); };
    let mut changed = false;
    for key in [*account.owner, caller] {
        if !members.iter().any(|member| member.pubkey == key) {
            members.push(Member { flags: 0, pubkey: key });
            changed = true;
        }
    }
    if !changed { return Ok(()); }
    if members.len() > 16 { return Err(ProgramError::InvalidArgument); }
    drop(data);
    UpdatePermissionCpiBuilder::new(permission_program)
        .authority(account, false).permissioned_account(account, true).permission(permission)
        .args(MembersArgs { members: Some(members) }).invoke_signed(&[seeds])
}
