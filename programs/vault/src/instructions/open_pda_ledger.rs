use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use ephemeral_rollups_sdk::access_control::structs::Member;
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::constants::MAX_GROWTH;
use crate::error::VaultError;
use crate::utils::account::create_ledger_account_sized;
use crate::utils::pda::{self, verify_pda_owner};
use crate::utils::permission;

#[derive(BorshDeserialize)]
struct Args {
    slots: u16,
    member_program: Pubkey,
    owner_seeds: Vec<Vec<u8>>,
}

/// Opens an empty ledger for a program's PDA, sponsor-funded. The member program is proven by
/// deriving the owner from its seeds.
/// Accounts: [owner, payer, ledger, permission, permission_program, system_program]
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let Args { slots, member_program, owner_seeds } =
        Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [owner, payer, ledger_ai, permission, permission_program, system_program, ..] = accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer || !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if *permission_program.key != PERMISSION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    verify_pda_owner(owner.key, &member_program, &owner_seeds)?;
    let bump = pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    if !ledger_ai.data_is_empty() {
        return Err(VaultError::LedgerExists.into());
    }
    if slots == 0 || slots > MAX_GROWTH {
        return Err(VaultError::BadSlotCount.into());
    }

    let mut l =
        create_ledger_account_sized(ledger_ai, payer, owner, system_program, bump, slots as usize)?;
    l.authorized = member_program;
    l.store(ledger_ai)?;

    permission::create(
        permission_program,
        ledger_ai,
        permission,
        payer,
        system_program,
        vec![
            Member { flags: 0, pubkey: member_program },
            Member { flags: 0, pubkey: *payer.key },
        ],
        &[b"ledger", owner.key.as_ref(), &[bump]],
    )
}
