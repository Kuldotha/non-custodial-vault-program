use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

use ephemeral_rollups_sdk::consts::EPHEMERAL_VAULT_ID;
use ephemeral_rollups_sdk::ephemeral_accounts::EphemeralAccount;

use crate::error::VaultError;
use crate::state::receipt::{self, RECEIPT_DISCRIMINATOR, RECEIPT_HEADER};
use crate::state::Ledger;

/// Closes an abandoned receipt (created, never settled), returning rent to the sponsor ledger.
/// Fired once by the reap crank. Idempotent — a settled receipt is already gone, so this no-ops.
/// Accounts: [receipt, authority_ledger, ephemeral_vault, magic_program]
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [receipt_ai, authority_ledger, ephemeral_vault, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if *ephemeral_vault.key != EPHEMERAL_VAULT_ID {
        return Err(ProgramError::InvalidArgument);
    }
    if receipt_ai.owner != program_id || receipt_ai.data_len() < RECEIPT_HEADER {
        return Ok(());
    }

    let authority = {
        let data = receipt_ai.try_borrow_data()?;
        if data[..8] != RECEIPT_DISCRIMINATOR {
            return Ok(());
        }
        // Only reap once the same-slot settle window has passed.
        if receipt::slot_of(&data) >= Clock::get()?.slot {
            return Ok(());
        }
        receipt::authority_of(&data)
    };

    let l = Ledger::load_checked(authority_ledger, program_id)?;
    if l.owner != authority {
        return Err(VaultError::BadAuthority.into());
    }
    let ledger_bump = [l.bump];
    let ledger_seeds: [&[u8]; 3] = [b"ledger", authority.as_ref(), &ledger_bump];
    EphemeralAccount::new(authority_ledger, receipt_ai, ephemeral_vault)
        .with_signer_seeds(&[&ledger_seeds])
        .close()
}
