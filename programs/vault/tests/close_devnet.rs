//! Replays a devnet wallet-ledger close in Mollusk with the real permission program loaded and
//! the real accounts, dumped from devnet, so the runtime's verdict comes with its logs.
//! Run with DEVNET_STATE=<dir holding acl.so, ledger.bin/.meta, permission.bin/.meta,
//! session.bin/.meta>.

mod common;

use common::*;
use mollusk_svm::program::loader_keys::LOADER_V3;
use solana_account::Account;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};
use solana_svm_log_collector::LogCollector;
use std::rc::Rc;

const CLOSE_LEDGER_DISC: [u8; 8] = [236, 179, 19, 235, 59, 77, 121, 118];
const SYSTEM: Pubkey = Pubkey::new_from_array([0; 32]);
const TOKEN: Pubkey = solana_program::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
const PERMISSION_PROGRAM: Pubkey = solana_program::pubkey!("ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1");

fn dumped(dir: &str, name: &str) -> (Pubkey, Account) {
    let meta = std::fs::read_to_string(format!("{dir}/{name}.meta")).unwrap();
    let mut parts = meta.split_whitespace();
    let pubkey: Pubkey = parts.next().unwrap().parse().unwrap();
    let owner: Pubkey = parts.next().unwrap().parse().unwrap();
    let lamports: u64 = parts.next().unwrap().parse().unwrap();
    let data = std::fs::read(format!("{dir}/{name}.bin")).unwrap();
    (pubkey, Account { lamports, data, owner, executable: false, rent_epoch: 0 })
}

#[test]
#[ignore = "needs DEVNET_STATE with dumps from devnet"]
fn replay_a_devnet_close() {
    let dir = std::env::var("DEVNET_STATE").expect("DEVNET_STATE");
    let mut mollusk = mollusk();
    let acl = std::fs::read(format!("{dir}/acl.so")).unwrap();
    mollusk.add_program_with_loader_and_elf(&PERMISSION_PROGRAM, &LOADER_V3, &acl);
    let logger = LogCollector::new_ref_with_limit(None);
    mollusk.logger = Some(Rc::clone(&logger));

    let (ledger, ledger_account) = dumped(&dir, "ledger");
    let (permission, permission_account) = dumped(&dir, "permission");
    let (session, session_account) = dumped(&dir, "session");
    let owner = Pubkey::new_from_array(ledger_account.data[8..40].try_into().unwrap());
    let (vault, _) = Pubkey::find_program_address(&[b"vault"], &vault::ID);

    let ix = Instruction {
        program_id: vault::ID,
        accounts: vec![
            AccountMeta::new(owner, true),
            AccountMeta::new(owner, true),
            AccountMeta::new(ledger, false),
            AccountMeta::new(vault, false),
            AccountMeta::new(permission, false),
            AccountMeta::new_readonly(PERMISSION_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN, false),
            AccountMeta::new_readonly(SYSTEM, false),
            AccountMeta::new(session, false),
        ],
        data: CLOSE_LEDGER_DISC.to_vec(),
    };
    let empty = || Account { lamports: 0, data: vec![], owner: Pubkey::default(), executable: false, rent_epoch: 0 };
    let program = |loader: Pubkey| Account { lamports: 1, data: vec![], owner: loader, executable: true, rent_epoch: 0 };
    for (label, permission_account, session_account) in [
        ("real permission, real store", permission_account.clone(), session_account.clone()),
        ("empty permission, real store", empty(), session_account.clone()),
        ("real permission, empty store", permission_account.clone(), empty()),
        ("empty permission, empty store", empty(), empty()),
    ] {
        let mut wallet_account = system_account();
        wallet_account.lamports = 31_913_400;
        logger.borrow_mut().get_recorded_content().len();
        let result = mollusk.process_instruction(
            &ix,
            &[
                (owner, wallet_account),
                (ledger, ledger_account.clone()),
                (vault, system_account()),
                (permission, permission_account),
                (PERMISSION_PROGRAM, program(LOADER_V3)),
                (TOKEN, program(Pubkey::default())),
                (SYSTEM, program(Pubkey::default())),
                (session, session_account),
            ],
        );
        let logs: Vec<String> = logger.borrow().get_recorded_content().iter().filter(|l| l.contains("invoke") || l.contains("failed")).cloned().collect();
        println!("{label}: {:?}\n    {}", result.raw_result, logs.join("\n    "));
        *logger.borrow_mut() = LogCollector::default();
    }
}
