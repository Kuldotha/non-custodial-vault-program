use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use ephemeral_rollups_sdk::consts::EPHEMERAL_VAULT_ID;
use ephemeral_rollups_sdk::ephemeral_accounts::EphemeralAccount;

use crate::error::VaultError;
use crate::state::receipt::{self, Movement, RECEIPT_HEADER};
use crate::state::Ledger;
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

/// Creates a receipt — an ephemeral, vault-owned account holding the terms. This is the approval:
/// every owner a movement debits signs here, as does any wallet a credit would open a new slot for.
/// Accounts: [authority, receipt, ephemeral_vault, magic_program] + ledgers[owners] + signers
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let args = Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [authority, receipt, ephemeral_vault, _magic_program, remaining @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !authority.is_signer {
        return Err(VaultError::MissingProgramSignature.into());
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

    // Approval happens here, so settle needs no consent. Owners' ledgers come first in
    // remaining_accounts, in index order; the approver signers follow.
    let ledger_count = args.owners.len();
    if remaining.len() < ledger_count {
        return Err(VaultError::NoAuthorization.into());
    }
    let (ledger_infos, signers) = remaining.split_at(ledger_count);
    for (idx, info) in ledger_infos.iter().enumerate() {
        if info.owner != program_id {
            return Err(VaultError::BadLedgerOwner.into());
        }
        let ledger = Ledger::read_from(&info.try_borrow_data()?)?;
        if ledger.owner != args.owners[idx] {
            return Err(VaultError::BadLedgerOwner.into());
        }
        let derived = Pubkey::create_program_address(
            &[b"ledger", args.owners[idx].as_ref(), &[ledger.bump]],
            program_id,
        )
        .map_err(|_| ProgramError::from(VaultError::BadLedgerOwner))?;
        if derived != *info.key {
            return Err(VaultError::BadLedgerOwner.into());
        }

        let debited = args.movements.iter().any(|m| m.from as usize == idx);
        let new_slot_credit = !ledger.pda_auth
            && args
                .movements
                .iter()
                .any(|m| m.to as usize == idx && ledger.index_of(&m.mint).is_none());
        if debited || new_slot_credit {
            let approved = signers.iter().any(|s| {
                s.is_signer && (*s.key == args.owners[idx] || *s.key == ledger.authorized)
            });
            if !approved {
                return Err(VaultError::NotAuthorizedToConsent.into());
            }
        }
    }

    let slot = Clock::get()?.slot;

    let (receipt_pda, bump) = Pubkey::find_program_address(
        &[b"receipt", authority.key.as_ref(), args.owners[0].as_ref()],
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
    let seeds: [&[u8]; 4] = [b"receipt", authority.key.as_ref(), args.owners[0].as_ref(), &bump_arr];
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
