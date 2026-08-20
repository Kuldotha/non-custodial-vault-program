use solana_program::{
    account_info::AccountInfo, declare_id, entrypoint, entrypoint::ProgramResult, pubkey::Pubkey,
};

use crate::instruction;

declare_id!("9vDAQgdHWCPQZabumgcuwoSLzWnRyQkSM1EHQnW8YXjs");

#[cfg(not(feature = "no-entrypoint"))]
entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    instruction::dispatch(program_id, accounts, instruction_data)
}
