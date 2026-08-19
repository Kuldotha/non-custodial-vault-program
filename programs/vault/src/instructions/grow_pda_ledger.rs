use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::error::VaultError;
use crate::state::Ledger;
use crate::utils::account::ensure_headroom;
use crate::utils::pda::{self, is_pda};

#[derive(BorshDeserialize)]
struct Args {
    min_free: u16,
    slot_increase: u16,
}

/// Adds slots to a program ledger, funded by its recorded rent payer. basenet only.
/// Accounts: [owner, payer, ledger, system_program]
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let Args { min_free, slot_increase } =
        Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [owner, payer, ledger_ai, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer || !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !is_pda(owner.key) {
        return Err(VaultError::OwnerNotPda.into());
    }
    pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;

    let mut l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    ensure_headroom(ledger_ai, &mut l, payer, system_program, min_free, slot_increase)?;
    l.store(ledger_ai)
}
