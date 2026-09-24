//! Closing a wallet ledger: the store's rent goes to the owner, the ledger's to the rent payer,
//! and for a wallet those are the same key passed twice.

mod common;

use common::*;
use mollusk_svm::result::ProgramResult;
use solana_account::Account;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

const CLOSE_LEDGER_DISC: [u8; 8] = [236, 179, 19, 235, 59, 77, 121, 118];
const SYSTEM: Pubkey = Pubkey::new_from_array([0; 32]);
const TOKEN: Pubkey = solana_program::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const PERMISSION_PROGRAM: Pubkey = solana_program::pubkey!("ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1");

fn close_ix(owner: Pubkey, rent_payer: Pubkey, ledger: Pubkey, vault: Pubkey, permission: Pubkey, session: Pubkey) -> Instruction {
    Instruction {
        program_id: vault::ID,
        accounts: vec![
            AccountMeta::new(owner, true),
            AccountMeta::new(rent_payer, true),
            AccountMeta::new(ledger, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(permission, false),
            AccountMeta::new_readonly(PERMISSION_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN, false),
            AccountMeta::new_readonly(SYSTEM, false),
            AccountMeta::new(session, false),
        ],
        data: CLOSE_LEDGER_DISC.to_vec(),
    }
}

fn empty() -> Account {
    Account { lamports: 0, data: vec![], owner: Pubkey::default(), executable: false, rent_epoch: 0 }
}

/// A wallet ledger with nothing on it, no permission (a pre-permission ledger), and a store.
fn close_with_store(store: bool) -> ProgramResult {
    let owner = wallet_key(7);
    let (ledger, ledger_account) = make_ledger(&owner, false, &owner, Pubkey::default(), 1, |_| {});
    let (vault, _) = Pubkey::find_program_address(&[b"vault"], &vault::ID);
    let (permission, _) = Pubkey::find_program_address(&[b"permission:", ledger.as_ref()], &PERMISSION_PROGRAM);
    let (session, session_account) = make_session(&owner, |_| {});
    let result = mollusk().process_instruction(
        &close_ix(owner, owner, ledger, vault, permission, session),
        &[
            (owner, system_account()),
            (ledger, ledger_account),
            (vault, system_account()),
            (permission, empty()),
            (PERMISSION_PROGRAM, empty()),
            (TOKEN, empty()),
            (SYSTEM, empty()),
            (session, if store { session_account } else { empty() }),
        ],
    );
    result.program_result
}

#[test]
fn a_wallet_ledger_without_a_store_closes() {
    assert_eq!(close_with_store(false), ProgramResult::Success);
}

#[test]
fn a_wallet_ledger_with_a_store_closes() {
    assert_eq!(close_with_store(true), ProgramResult::Success);
}
