use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use ephemeral_rollups_sdk::cpi::{delegate_account, undelegate_account, DelegateAccounts, DelegateConfig};
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder, MagicIntentBundleBuilder};

use crate::state::Ledger;

#[derive(BorshDeserialize)]
struct DelegateArgs {
    validator: Option<Pubkey>,
}

/// Hands a ledger to the rollup. The owner signs, so only they can delegate their ledger.
/// Accounts: [payer, owner, buffer, delegation_record, delegation_metadata, ledger, owner_program,
///            delegation_program, system_program]
pub fn delegate_handler(_program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let DelegateArgs { validator } =
        DelegateArgs::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [payer, owner, buffer, delegation_record, delegation_metadata, ledger_ai, owner_program, delegation_program, system_program, ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !payer.is_signer || !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    delegate_account(
        DelegateAccounts {
            payer,
            pda: ledger_ai,
            owner_program,
            buffer,
            delegation_record,
            delegation_metadata,
            delegation_program,
            system_program,
        },
        &[b"ledger", owner.key.as_ref()],
        DelegateConfig { commit_frequency_ms: u32::MAX, validator },
    )
}

/// Ends the session and returns the ledger to basenet. Sent to the rollup; the commit is implicit.
///
/// `payer` funds the magic commit and must be the transaction fee payer — the rollup only lets an
/// account be written if it is delegated or is the fee payer, and the magic program locates the
/// payer by that identity. `authority` is who the ledger names as owner; no signature is required
/// of it. `fees_vault` is the magic program's ephemeral vault.
/// Accounts: [payer, authority, ledger, magic_program, magic_context, fees_vault]
pub fn undelegate_handler(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let [payer, _authority, ledger_ai, magic_program, magic_context, fees_vault, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !payer.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    // Permissionless by design: undelegate only commits the ledger home (no value moves), so anyone
    // may rescue one whose authorized key is lost. Still must be a real vault ledger; the caller pays.
    Ledger::load_checked(ledger_ai, program_id)?;
    MagicIntentBundleBuilder::new(payer.clone(), magic_context.clone(), magic_program.clone())
        .magic_fee_vault(fees_vault.clone())
        .commit_and_undelegate(&[ledger_ai.clone()])
        .build_and_invoke()
}

#[derive(BorshDeserialize)]
struct ProcessArgs {
    pda_seeds: Vec<Vec<u8>>,
}

/// The delegation program's callback that finalises undelegation.
/// Accounts: [delegated_pda, buffer, payer, system_program]
pub fn process_undelegation_handler(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    let ProcessArgs { pda_seeds } =
        ProcessArgs::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [delegated_pda, buffer, payer, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    undelegate_account(delegated_pda, program_id, buffer, payer, system_program, pda_seeds)
}
