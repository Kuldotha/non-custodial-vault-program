use solana_program::{account_info::AccountInfo, program_error::ProgramError};

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
