use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke_signed,
    program_error::ProgramError, pubkey::Pubkey,
};
use solana_system_interface::instruction as system_instruction;

use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::constants::{SOL_MINT, TOKEN_PROGRAM_ID};
use crate::error::VaultError;
use crate::state::Ledger;
use crate::utils::pda;
use crate::utils::permission;
use crate::utils::reserve::{require_reserve, token_fields, vault_floor};
use crate::utils::spl;

/// Sweeps every balance back to the owner, then closes the ledger and its permission. basenet only.
/// The rent goes to whoever put it up (the recorded rent payer), not the owner.
/// Accounts: [owner, rent_payer, ledger, vault, permission, permission_program, token_program,
///            system_program] + (vault_token, owner_token) pairs in entry order.
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let [owner, rent_payer, ledger_ai, vault, permission, permission_program, token_program, system_program, remaining @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer || !rent_payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if *token_program.key != TOKEN_PROGRAM_ID || *permission_program.key != PERMISSION_PROGRAM_ID {
        return Err(ProgramError::IncorrectProgramId);
    }
    pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    let vault_bump = pda::validate(program_id, vault, &[b"vault"])?;

    let mut l = Ledger::load_checked(ledger_ai, program_id)?;
    if l.owner != *owner.key {
        return Err(VaultError::BadLedgerOwner.into());
    }
    if l.rent_payer != *rent_payer.key {
        return Err(VaultError::NotRentPayer.into());
    }
    let ledger_bump = l.bump;

    // ── sweep SOL (entry 0) ──────────────────────────────────────────────────
    let sol = l.entries[0].amount;
    if sol > 0 {
        let spendable = vault.lamports().saturating_sub(vault_floor()?);
        if spendable < sol {
            return Err(VaultError::InsufficientReserve.into());
        }
        invoke_signed(
            &system_instruction::transfer(vault.key, owner.key, sol),
            &[vault.clone(), owner.clone(), system_program.clone()],
            &[&[b"vault", &[vault_bump]]],
        )?;
        l.entries[0].amount = 0;
    }

    // ── sweep every non-zero token entry, in entry order ─────────────────────
    let mut pair = 0usize;
    for i in 1..l.capacity() {
        let (mint, amount) = (l.entries[i].mint, l.entries[i].amount);
        if mint == SOL_MINT || amount == 0 {
            continue;
        }
        let vault_token = remaining.get(pair * 2).ok_or(VaultError::MissingTokenAccounts)?;
        let owner_token = remaining.get(pair * 2 + 1).ok_or(VaultError::MissingTokenAccounts)?;
        pair += 1;

        let reserve = require_reserve(vault_token, vault.key, &mint)?;
        if reserve < amount {
            return Err(VaultError::InsufficientReserve.into());
        }
        let (d_mint, d_owner, _) = token_fields(owner_token)?;
        if d_mint != mint || d_owner != *owner.key {
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
        l.entries[i].amount = 0;
    }

    // Nothing may be left behind: an incomplete account list would strand value.
    if !l.entries.iter().all(|e| e.amount == 0) {
        return Err(VaultError::MissingTokenAccounts.into());
    }

    // Close the permission (skip if a pre-permission ledger has none), then the ledger account.
    if !permission.data_is_empty() {
        permission::close(
            permission_program,
            ledger_ai,
            permission,
            rent_payer,
            &[b"ledger", owner.key.as_ref(), &[ledger_bump]],
        )?;
    }
    pda::close(rent_payer, ledger_ai)
}
