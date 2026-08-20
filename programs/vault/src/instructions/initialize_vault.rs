use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey,
};
use solana_system_interface::instruction as system_instruction;

use crate::utils::pda;
use crate::utils::reserve::vault_floor;

/// Funds `["vault"]` to its rent floor. Permissionless and idempotent.
/// Accounts: [payer, vault, system_program]
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [payer, vault, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    pda::validate(program_id, vault, &[b"vault"])?;

    let floor = vault_floor()?;
    let have = vault.lamports();
    if have >= floor {
        return Ok(());
    }
    invoke(
        &system_instruction::transfer(payer.key, vault.key, floor - have),
        &[payer.clone(), vault.clone(), system_program.clone()],
    )
}
