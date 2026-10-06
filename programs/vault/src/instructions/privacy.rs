use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use ephemeral_rollups_sdk::access_control::structs::Member;
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::error::VaultError;
use crate::state::Ledger;
use crate::utils::pda::{self, is_pda, verify_pda_owner};
use crate::utils::permission;

/// Deletes a ledger's permission — the owner giving privacy up. The only way it becomes public.
/// Accounts: [owner, ledger, permission, payer, permission_program]
pub fn make_public_handler(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let [owner, ledger_ai, permission, payer, permission_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer || !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if *permission_program.key != PERMISSION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    let bump = pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    let l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    if *payer.key != l.rent_payer {
        return Err(VaultError::NotRentPayer.into());
    }

    permission::close(
        permission_program,
        ledger_ai,
        permission,
        payer,
        &[b"ledger", owner.key.as_ref(), &[bump]],
    )
}

/// Takes privacy back for a wallet: the permission returns, naming the owner.
/// Accounts: [owner, ledger, permission, permission_program, system_program]
pub fn make_wallet_ledger_private_handler(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> ProgramResult {
    let [owner, ledger_ai, permission, permission_program, system_program, ..] = accounts else {
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
    let l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    // The owner is the rent payer for a wallet ledger, and must sign as such.
    if *owner.key != l.rent_payer {
        return Err(VaultError::NotRentPayer.into());
    }
    if !permission.data_is_empty() {
        return Err(VaultError::PermissionExists.into());
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

#[derive(BorshDeserialize)]
struct PdaArgs {
    member_program: Pubkey,
    owner_seeds: Vec<Vec<u8>>,
}

/// Takes privacy back for a program's PDA, with the same proof `open_pda_ledger` demands.
/// Accounts: [owner, payer, ledger, permission, permission_program, system_program]
pub fn make_pda_ledger_private_handler(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let PdaArgs { member_program, owner_seeds } =
        PdaArgs::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
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
    if !permission.data_is_empty() {
        return Err(VaultError::PermissionExists.into());
    }
    verify_pda_owner(owner.key, &member_program, &owner_seeds)?;
    let bump = pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    let l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    if *payer.key != l.rent_payer {
        return Err(VaultError::NotRentPayer.into());
    }

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

#[derive(BorshDeserialize)]
struct AddPdaCaller {
    member_program: Pubkey,
    owner_seeds: Vec<Vec<u8>>,
    caller: Pubkey,
}

/// Adds a caller without changing balances, ledger authority, or existing permission flags.
/// Accounts: [owner (signer), ledger, permission, permission_program]
pub fn add_pda_caller_handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let args = AddPdaCaller::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [owner, ledger, permission_account, permission_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer { return Err(ProgramError::MissingRequiredSignature); }
    verify_pda_owner(owner.key, &args.member_program, &args.owner_seeds)?;
    let bump = pda::validate(program_id, ledger, &[b"ledger", owner.key.as_ref()])?;
    if ledger.owner != program_id && *ledger.owner != ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID {
        return Err(ProgramError::IllegalOwner);
    }
    let state = Ledger::read_from(&ledger.try_borrow_data()?)?;
    if state.owner != *owner.key { return Err(VaultError::BadLedgerOwner.into()); }
    permission::add_caller(permission_program, ledger, permission_account, args.caller,
        &[b"ledger", owner.key.as_ref(), &[bump]])
}

#[cfg(test)]
mod caller_tests {
    use super::*;
    use borsh::BorshSerialize;

    fn request(program: Pubkey, caller: Pubkey, seeds: Vec<Vec<u8>>) -> Vec<u8> {
        let mut data = program.to_bytes().to_vec();
        seeds.serialize(&mut data).unwrap();
        data.extend_from_slice(caller.as_ref());
        data
    }

    #[test]
    fn adding_a_caller_requires_the_owner_signature() {
        let key = Pubkey::new_unique();
        let mut lamports = 0;
        let mut data = [];
        let account = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &crate::ID, false);
        let accounts = vec![account; 4];
        assert_eq!(add_pda_caller_handler(&crate::ID, &accounts, &request(Pubkey::new_unique(), key, vec![])), Err(ProgramError::MissingRequiredSignature));
    }

    #[test]
    fn a_signed_owner_cannot_claim_another_programs_pda() {
        let game = Pubkey::new_unique();
        let (owner, bump) = Pubkey::find_program_address(&[b"house"], &game);
        let mut lamports = 0;
        let mut data = [];
        let account = AccountInfo::new(&owner, true, false, &mut lamports, &mut data, &crate::ID, false);
        let accounts = vec![account; 4];
        assert!(add_pda_caller_handler(&crate::ID, &accounts,
            &request(Pubkey::new_unique(), owner, vec![b"house".to_vec(), vec![bump]])).is_err());
    }
}
