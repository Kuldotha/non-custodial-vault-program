mod common;

use common::*;
use borsh::{BorshDeserialize, BorshSerialize};
use ephemeral_rollups_sdk::access_control::structs::{Member, Permission};
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;
use mollusk_svm::program::loader_keys::LOADER_V3;
use solana_account::Account;
use solana_program::{instruction::{AccountMeta, Instruction}, pubkey::Pubkey};

#[test]
#[ignore = "requires ACL_ELF pointing to the permission program binary"]
fn caller_registration_keeps_members_flags_and_balances() {
    let mut svm = mollusk();
    svm.add_program_with_loader_and_elf(&PERMISSION_PROGRAM_ID, &LOADER_V3,
        &std::fs::read(std::env::var("ACL_ELF").unwrap()).unwrap());
    let game = Pubkey::new_unique();
    let caller = Pubkey::new_unique();
    let reader = Pubkey::new_unique();
    let (owner, bump) = Pubkey::find_program_address(&[b"house"], &game);
    let (ledger, mut ledger_account) = make_ledger(&owner, true, &reader, game, 1, |_| {});
    ledger_account.owner = ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID;
    let (permission, permission_bump) = Permission::find_pda(&ledger);
    let mut bytes = borsh::to_vec(&Permission {
        discriminator: 0, bump: permission_bump, permissioned_account: ledger,
        members: Some(vec![Member {flags: 0, pubkey: vault::ID}, Member {flags: 3, pubkey: reader}]),
    }).unwrap();
    bytes.resize(567, 0);
    let permission_account = Account {lamports: 5_000_000, data: bytes,
        owner: PERMISSION_PROGRAM_ID, executable: false, rent_epoch: 0};
    let mut data = vec![179, 6, 243, 124, 245, 19, 225, 185];
    data.extend_from_slice(game.as_ref());
    vec![b"house".to_vec(), vec![bump]].serialize(&mut data).unwrap();
    data.extend_from_slice(caller.as_ref());
    let ix = Instruction {program_id: vault::ID, data, accounts: vec![
        AccountMeta::new_readonly(owner, true), AccountMeta::new_readonly(ledger, false),
        AccountMeta::new(permission, false), AccountMeta::new_readonly(PERMISSION_PROGRAM_ID, false)]};
    let accounts = vec![(owner, system_account()), (ledger, ledger_account.clone()), (permission, permission_account),
        (PERMISSION_PROGRAM_ID, Account {lamports: 1, data: vec![], owner: LOADER_V3, executable: true, rent_epoch: 0})];
    let result = svm.process_instruction(&ix, &accounts);
    assert!(result.program_result.is_ok(), "{:?}", result.program_result);
    let updated = result.resulting_accounts.iter().find(|(key,_)| *key == permission).unwrap();
    let parsed = Permission::deserialize(&mut &updated.1.data[..]).unwrap();
    let members = parsed.members.unwrap();
    assert_eq!(members.len(), 4);
    assert!(members.iter().any(|m| m.pubkey == ephemeral_rollups_sdk::consts::DELEGATION_PROGRAM_ID));
    assert!(members.iter().any(|m| m.pubkey == reader && m.flags == 3));
    assert!(members.iter().any(|m| m.pubkey == vault::ID));
    assert!(members.iter().any(|m| m.pubkey == caller && m.flags == 0));
    let unchanged = result.resulting_accounts.iter().find(|(key,_)| *key == ledger).unwrap();
    assert_eq!(unchanged.1, ledger_account);
    let again = svm.process_instruction(&ix, &result.resulting_accounts);
    assert!(again.program_result.is_ok());
    assert_eq!(again.resulting_accounts, result.resulting_accounts);
}
