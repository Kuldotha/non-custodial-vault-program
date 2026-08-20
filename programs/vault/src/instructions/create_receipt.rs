use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use ephemeral_rollups_sdk::consts::EPHEMERAL_VAULT_ID;
use ephemeral_rollups_sdk::ephemeral_accounts::EphemeralAccount;

use crate::error::VaultError;
use crate::state::receipt::{self, Movement, RECEIPT_HEADER};
use crate::utils::pda::verify_pda_owner;

#[derive(BorshDeserialize)]
struct Args {
    movements: Vec<Movement>,
    member_program: Pubkey,
    authority_seeds: Vec<Vec<u8>>,
    owners: Vec<Pubkey>,
    callback_disc: [u8; 8],
    args: Vec<u8>,
}

/// An ephemeral vault-owned account holding the terms. It touches no ledger, which is what lets a
/// game reach it by CPI inside a private rollup (the ACL would refuse a game transaction that
/// touched the player's ledger); consent is checked at settle instead. The receipt is seeded by and
/// signed by its consenter (the session key), so nobody can mint one for someone else — one that
/// debits or seeds a human it has no right to simply dies at settle.
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let args = Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [authority, consenter, receipt, ephemeral_vault, _magic_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !authority.is_signer {
        return Err(VaultError::MissingProgramSignature.into());
    }
    if !consenter.is_signer {
        return Err(VaultError::NotAuthorizedToConsent.into());
    }
    if args.movements.len() > u8::MAX as usize || args.args.len() > u8::MAX as usize {
        return Err(VaultError::NoAuthorization.into());
    }
    if args.owners.is_empty() || args.owners.len() > u8::MAX as usize {
        return Err(VaultError::NoAuthorization.into());
    }
    if *ephemeral_vault.key != EPHEMERAL_VAULT_ID {
        return Err(ProgramError::InvalidArgument);
    }

    // Not `authority.owner` — delegation rewrites that field.
    verify_pda_owner(authority.key, &args.member_program, &args.authority_seeds)?;

    for (i, o) in args.owners.iter().enumerate() {
        if args.owners[..i].contains(o) {
            return Err(VaultError::DuplicateLedger.into());
        }
    }
    for m in &args.movements {
        if (m.from as usize) >= args.owners.len() || (m.to as usize) >= args.owners.len() {
            return Err(VaultError::NoAuthorization.into());
        }
        if m.from == m.to {
            return Err(VaultError::NoAuthorization.into());
        }
    }

    let slot = Clock::get()?.slot;

    let (receipt_pda, bump) = Pubkey::find_program_address(
        &[b"receipt", authority.key.as_ref(), consenter.key.as_ref()],
        program_id,
    );
    if *receipt.key != receipt_pda {
        return Err(VaultError::BadAuthority.into());
    }

    // A settled receipt the member program never closed still belongs to it.
    if !receipt.data_is_empty() && receipt.owner != program_id {
        return Err(VaultError::ReceiptNotConsumed.into());
    }
    // A live same-slot receipt may not be overwritten; an earlier slot's is debris.
    if receipt.data_len() >= RECEIPT_HEADER {
        let created = receipt::slot_of(&receipt.try_borrow_data()?);
        if created == slot {
            return Err(VaultError::ReceiptLive.into());
        }
    }

    let len = receipt::len_for(args.owners.len(), args.movements.len(), args.args.len());
    let bump_arr = [bump];
    let seeds: [&[u8]; 4] = [b"receipt", authority.key.as_ref(), consenter.key.as_ref(), &bump_arr];
    let signer_seeds: [&[&[u8]]; 1] = [&seeds];
    let account = EphemeralAccount::new(authority, receipt, ephemeral_vault)
        .with_signer_seeds(&signer_seeds);
    if receipt.data_len() == 0 {
        account.create(len as u32)?;
    } else if receipt.data_len() != len {
        account.resize(len as u32)?;
    }

    receipt::write(
        &mut receipt.try_borrow_mut_data()?,
        consenter.key,
        &args.owners,
        authority.key,
        &args.member_program,
        &args.callback_disc,
        slot,
        &args.movements,
        &args.args,
    );
    Ok(())
}
