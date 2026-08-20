use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::instructions;

// Approach A: the wire discriminators are Anchor's `sha256("global:<name>")[..8]`, so existing
// callers (scratch-cards' utils/vault.rs, any client, the IDL) keep working unchanged.
const INITIALIZE_VAULT: [u8; 8] = [48, 191, 163, 44, 71, 129, 63, 164];
const OPEN_WALLET_LEDGER: [u8; 8] = [219, 161, 148, 96, 48, 166, 60, 149];
const OPEN_PDA_LEDGER: [u8; 8] = [129, 231, 253, 170, 87, 172, 11, 29];
const GROW_PDA_LEDGER: [u8; 8] = [197, 145, 30, 32, 242, 38, 205, 203];
const MAKE_PUBLIC: [u8; 8] = [41, 76, 102, 98, 184, 102, 132, 29];
const MAKE_WALLET_LEDGER_PRIVATE: [u8; 8] = [176, 216, 128, 156, 39, 62, 187, 89];
const MAKE_PDA_LEDGER_PRIVATE: [u8; 8] = [200, 46, 191, 221, 30, 12, 10, 96];
const DEPOSIT: [u8; 8] = [242, 35, 198, 137, 82, 225, 242, 182];
const WITHDRAW: [u8; 8] = [183, 18, 70, 156, 148, 109, 161, 34];
const SETTLE: [u8; 8] = [175, 42, 185, 87, 144, 131, 102, 212];
const CREATE_RECEIPT: [u8; 8] = [187, 57, 104, 13, 15, 1, 219, 99];
const SETTLE_RECEIPT: [u8; 8] = [216, 17, 200, 111, 7, 3, 233, 182];
const ASSIGN_LEDGER_AUTHORIZATION: [u8; 8] = [116, 150, 47, 4, 211, 225, 133, 201];
const CLOSE_LEDGER: [u8; 8] = [236, 179, 19, 235, 59, 77, 121, 118];
const DELEGATE_LEDGER: [u8; 8] = [159, 3, 197, 64, 7, 12, 101, 66];
const UNDELEGATE: [u8; 8] = [131, 148, 180, 198, 91, 104, 42, 238];
/// The delegation program's fixed callback discriminator — matched first, exactly as the
/// `#[ephemeral]` macro used to inject a handler for it.
const PROCESS_UNDELEGATION: [u8; 8] = [196, 28, 41, 206, 48, 37, 51, 167];

pub fn dispatch(program_id: &Pubkey, accounts: &[AccountInfo], input: &[u8]) -> ProgramResult {
    if input.len() < 8 {
        return Err(ProgramError::InvalidInstructionData);
    }
    let disc: [u8; 8] = input[..8].try_into().unwrap();
    let data = &input[8..];

    match disc {
        INITIALIZE_VAULT => instructions::initialize_vault::handler(program_id, accounts, data),
        SETTLE => instructions::settle::handler(program_id, accounts, data),
        GROW_PDA_LEDGER => instructions::grow_pda_ledger::handler(program_id, accounts, data),
        ASSIGN_LEDGER_AUTHORIZATION => {
            instructions::authorize::assign_handler(program_id, accounts, data)
        }
        OPEN_WALLET_LEDGER => instructions::open_wallet_ledger::handler(program_id, accounts, data),
        OPEN_PDA_LEDGER => instructions::open_pda_ledger::handler(program_id, accounts, data),
        MAKE_PUBLIC => instructions::privacy::make_public_handler(program_id, accounts),
        MAKE_WALLET_LEDGER_PRIVATE => {
            instructions::privacy::make_wallet_ledger_private_handler(program_id, accounts)
        }
        MAKE_PDA_LEDGER_PRIVATE => {
            instructions::privacy::make_pda_ledger_private_handler(program_id, accounts, data)
        }
        DEPOSIT => instructions::deposit::handler(program_id, accounts, data),
        WITHDRAW => instructions::withdraw::handler(program_id, accounts, data),
        CREATE_RECEIPT => instructions::create_receipt::handler(program_id, accounts, data),
        SETTLE_RECEIPT => instructions::settle_receipt::handler(program_id, accounts, data),
        CLOSE_LEDGER => instructions::close_ledger::handler(program_id, accounts),
        DELEGATE_LEDGER => instructions::delegation::delegate_handler(program_id, accounts, data),
        UNDELEGATE => instructions::delegation::undelegate_handler(program_id, accounts),
        PROCESS_UNDELEGATION => {
            instructions::delegation::process_undelegation_handler(program_id, accounts, data)
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
