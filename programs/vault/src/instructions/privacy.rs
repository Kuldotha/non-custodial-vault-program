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
