use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey,
};
use solana_system_interface::instruction as system_instruction;

use ephemeral_rollups_sdk::access_control::structs::Member;
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::constants::{is_token_program, DEFAULT_MIN_FREE, DEFAULT_SLOTS, SOL_MINT};
use crate::error::VaultError;
use crate::state::Ledger;
use crate::utils::account::{create_ledger_account, ensure_headroom};
use crate::utils::pda::{self, is_pda};
use crate::utils::permission;
use crate::utils::reserve::{require_reserve, token_fields, vault_floor};
use crate::utils::spl::{self, trailing_mint};

#[derive(BorshDeserialize)]
struct Args {
    mint: Pubkey,
    amount: u64,
    min_free: Option<u16>,
    slot_increase: Option<u16>,
}

/// Wallet → vault. `mint` selects the asset; `SOL_MINT` moves lamports. Creates the ledger and
/// its permission on first use, and tops up the free-slot band.
/// Accounts: [owner, ledger, permission, permission_program, vault, vault_token, owner_token,
///            token_program, system_program] (+ the mint, appended, for a Token-2022 asset —
///            its transfer must be checked, and checked transfers carry the mint)
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let Args { mint, amount, min_free, slot_increase } =
        Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [owner, ledger_ai, permission, permission_program, vault, vault_token, owner_token, token_program, system_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // Off-curve owners are refused — a program's ledger moves value only through `settle`.
    if is_pda(owner.key) {
        return Err(VaultError::OffCurveOwnerNotAllowed.into());
    }
    if *permission_program.key != PERMISSION_PROGRAM_ID || !is_token_program(token_program.key) {
        return Err(ProgramError::IncorrectProgramId);
    }

    let slot_increase = slot_increase.unwrap_or(DEFAULT_SLOTS);
    let min_free = min_free.unwrap_or(DEFAULT_MIN_FREE);
    let bump = pda::validate(program_id, ledger_ai, &[b"ledger", owner.key.as_ref()])?;
    pda::validate(program_id, vault, &[b"vault"])?;

    let mut l = if ledger_ai.data_is_empty() {
        create_ledger_account(ledger_ai, owner, system_program, bump)?
    } else {
        if ledger_ai.owner != program_id {
            return Err(VaultError::BadLedgerOwner.into());
        }
        Ledger::read_from(&ledger_ai.try_borrow_data()?)?
    };

    // Created here on first use, unconditionally, rather than left to whoever later delegates.
    if permission.data_is_empty() {
        l.store(ledger_ai)?;
        permission::create(
            permission_program,
            ledger_ai,
            permission,
            owner,
            system_program,
            vec![Member { flags: 0, pubkey: *owner.key }],
            &[b"ledger", owner.key.as_ref(), &[bump]],
        )?;
    }

    // Before the claim, not after: a deposit of a new mint into a full ledger would otherwise fail
    // on the very growth this call is about to perform.
    ensure_headroom(ledger_ai, &mut l, owner, system_program, min_free, slot_increase)?;
    let index = l.index_or_claim(&mint)?;

    if mint == SOL_MINT {
        // Rent must already be in place — see initialize_vault.
        if vault.lamports() < vault_floor()? {
            return Err(VaultError::VaultNotInitialized.into());
        }
        invoke(
            &system_instruction::transfer(owner.key, vault.key, amount),
            &[owner.clone(), vault.clone(), system_program.clone()],
        )?;
    } else {
        require_reserve(vault_token, vault.key, &mint, token_program.key)?;
        let (src_mint, _, _) = token_fields(owner_token)?;
        if src_mint != mint || owner_token.owner != token_program.key {
            return Err(VaultError::MintMismatch.into());
        }
        match trailing_mint(accounts, 9, &mint, token_program.key)? {
            Some(mint_ai) => spl::transfer_checked(
                token_program, owner_token, mint_ai, vault_token, owner, amount, None)?,
            None => spl::transfer(token_program, owner_token, vault_token, owner, amount, None)?,
        }
    }

    l.credit(index, amount)?;
    l.store(ledger_ai)
}
