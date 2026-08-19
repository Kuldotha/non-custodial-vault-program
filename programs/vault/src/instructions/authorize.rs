use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::error::VaultError;
use crate::state::Ledger;
use crate::utils::pda::{self, is_pda, verify_pda_owner};

#[derive(BorshDeserialize)]
struct AssignArgs {
    authorized: Pubkey,
}

/// Grants or revokes (all-zero key) a wallet ledger's session key. basenet only; the owner signs.
/// Accounts: [ledger, owner]
pub fn assign_handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let AssignArgs { authorized } =
        AssignArgs::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [ledger_ai, owner, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;

    let mut l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    if l.pda_auth {
        return Err(VaultError::CannotAuthorizePdaLedger.into());
    }
    if authorized != Pubkey::default() && is_pda(&authorized) {
        return Err(VaultError::BadAuthorizedKey.into());
    }
    l.authorized = authorized;
    l.store(ledger_ai)
}

#[derive(BorshDeserialize)]
struct AuthorizePdaArgs {
    member_program: Pubkey,
    owner_seeds: Vec<Vec<u8>>,
}

/// Backfills a PDA ledger's stored member program. The owner PDA signs and its seeds prove it.
/// Accounts: [owner, ledger]
pub fn authorize_pda_handler(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let AuthorizePdaArgs { member_program, owner_seeds } =
        AuthorizePdaArgs::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [owner, ledger_ai, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    verify_pda_owner(owner.key, &member_program, &owner_seeds)?;

    let mut l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    if !l.pda_auth {
        return Err(VaultError::OwnerNotPda.into());
    }
    l.authorized = member_program;
    l.store(ledger_ai)
}
