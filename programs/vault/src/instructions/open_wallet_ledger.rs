use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use ephemeral_rollups_sdk::access_control::structs::Member;
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::constants::MAX_GROWTH;
use crate::error::VaultError;
use crate::utils::account::{create_ledger_account_sized, create_session_account};
use crate::utils::pda::{self, is_pda};
use crate::utils::permission;

#[derive(BorshDeserialize)]
struct Args {
    slots: u16,
}

/// Opens an empty wallet ledger at a chosen size, owner-funded, with its session store beside it.
/// Accounts: [owner, ledger, permission, permission_program, system_program, session]
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let Args { slots } =
        Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [owner, ledger_ai, permission, permission_program, system_program, session, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if is_pda(owner.key) {
        return Err(VaultError::OwnerNotWallet.into());
    }
    if *permission_program.key != PERMISSION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let bump = pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    if !ledger_ai.data_is_empty() {
        return Err(VaultError::LedgerExists.into());
    }
    if slots == 0 || slots > MAX_GROWTH {
        return Err(VaultError::BadSlotCount.into());
    }

    let l = create_ledger_account_sized(ledger_ai, owner, owner, system_program, bump, slots as usize)?;
    l.store(ledger_ai)?;

    let session_bump = pda::validate(program_id, session, &[b"session", owner.key.as_ref()])?;
    if session.data_is_empty() {
        create_session_account(session, owner, system_program, session_bump)?;
    }

    permission::create(
        permission_program,
        ledger_ai,
        permission,
        owner,
        system_program,
        vec![Member { flags: 0, pubkey: *owner.key }],
        &[b"ledger", owner.key.as_ref(), &[bump]],
    )
}
