use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke_signed,
    program_error::ProgramError, pubkey::Pubkey,
};
use solana_system_interface::instruction as system_instruction;

use crate::constants::{SOL_MINT, TOKEN_PROGRAM_ID};
use crate::error::VaultError;
use crate::state::Ledger;
use crate::utils::pda::{self, is_pda};
use crate::utils::reserve::{require_reserve, token_fields, vault_floor};
use crate::utils::spl;

#[derive(BorshDeserialize)]
struct Args {
    mint: Pubkey,
    amount: u64,
}

/// Vault → wallet, and only ever the signer's own.
/// Accounts: [owner, ledger, vault, vault_token, owner_token, token_program, system_program]
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let Args { mint, amount } =
        Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [owner, ledger_ai, vault, vault_token, owner_token, token_program, system_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if is_pda(owner.key) {
        return Err(VaultError::OffCurveOwnerNotAllowed.into());
    }
    if *token_program.key != TOKEN_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    let vault_bump = pda::validate(program_id, vault, &[b"vault"])?;

    let mut l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    let index = l.index_of(&mint).ok_or(VaultError::NoBalance)?;
    l.debit(index, amount)?;

    if mint == SOL_MINT {
        // The vault's own rent is not part of anyone's balance.
        let spendable = vault.lamports().saturating_sub(vault_floor()?);
        if spendable < amount {
            return Err(VaultError::InsufficientReserve.into());
        }
        invoke_signed(
            &system_instruction::transfer(vault.key, owner.key, amount),
            &[vault.clone(), owner.clone(), system_program.clone()],
            &[&[b"vault", &[vault_bump]]],
        )?;
    } else {
        let reserve = require_reserve(vault_token, vault.key, &mint)?;
        if reserve < amount {
            return Err(VaultError::InsufficientReserve.into());
        }
        let (dst_mint, dst_owner, _) = token_fields(owner_token)?;
        if dst_mint != mint || dst_owner != *owner.key {
            return Err(VaultError::MintMismatch.into());
        }
        spl::transfer(
            token_program,
            vault_token,
            owner_token,
            vault,
            amount,
            Some(&[b"vault", &[vault_bump]]),
        )?;
    }

    l.store(ledger_ai)
}
