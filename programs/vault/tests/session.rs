//! The session store: per-program keys granted and revoked by the owner on basenet, and
//! consenting in `settle` for the one program each was minted for. `settle_receipt` consents
//! through the same function, past CPIs mollusk cannot make.

mod common;

use common::*;
use mollusk_svm::result::{InstructionResult, ProgramResult};
use solana_account::Account;
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_error::ProgramError,
    pubkey::Pubkey,
    rent::Rent,
};
use vault::error::VaultError;
use vault::state::{Session, RING};

const SYSTEM: Pubkey = Pubkey::new_from_array([0; 32]);

fn authorize_ix(session: Pubkey, owner: (Pubkey, bool), program: Pubkey, key: Pubkey, expires_at: i64) -> Instruction {
    let mut data = AUTHORIZE_SESSION_DISC.to_vec();
    data.extend_from_slice(program.as_ref());
    data.extend_from_slice(key.as_ref());
    data.extend_from_slice(&expires_at.to_le_bytes());
    Instruction {
        program_id: vault::ID,
        accounts: vec![
            AccountMeta::new(session, false),
            AccountMeta::new(owner.0, owner.1),
            AccountMeta::new_readonly(SYSTEM, false),
        ],
        data,
    }
}

fn revoke_ix(session: Pubkey, owner: Pubkey, program: Pubkey, key: Pubkey) -> Instruction {
    let mut data = REVOKE_SESSION_DISC.to_vec();
    data.extend_from_slice(program.as_ref());
    data.extend_from_slice(key.as_ref());
    Instruction {
        program_id: vault::ID,
        accounts: vec![AccountMeta::new(session, false), AccountMeta::new(owner, true)],
        data,
    }
}

/// A program paying a wallet a mint the wallet has no slot for yet, the session key consenting,
/// with the wallet's store passed.
fn settle_new_slot_ix(src: Pubkey, dst: Pubkey, prog: Pubkey, consenter: Pubkey, session: Option<Pubkey>, mint: Pubkey) -> Instruction {
    let mut data = SETTLE_DISC.to_vec();
    data.extend_from_slice(mint.as_ref());
    data.extend_from_slice(&100u64.to_le_bytes());
    let mut accounts = vec![
        AccountMeta::new(src, false),
        AccountMeta::new(dst, false),
        AccountMeta::new_readonly(prog, true),
        AccountMeta::new_readonly(consenter, true),
    ];
    if let Some(session) = session {
        accounts.push(AccountMeta::new_readonly(session, false));
    }
    Instruction { program_id: vault::ID, accounts, data }
}

fn err_code(r: &InstructionResult) -> u32 {
    match &r.program_result {
        ProgramResult::Failure(ProgramError::Custom(c)) => *c,
        other => panic!("expected a custom error, got {other:?}"),
    }
}

/// The store, and the owner and system program beside it, as an instruction wants them.
fn with_owner(session: Pubkey, store: Account, owner: Pubkey) -> Vec<(Pubkey, Account)> {
    let (system, system_account) = mollusk_svm::program::keyed_account_for_system_program();
    vec![(session, store), (owner, system_account_holding(10_000_000_000)), (system, system_account)]
}

fn system_account_holding(lamports: u64) -> Account {
    let mut a = system_account();
    a.lamports = lamports;
    a
}

/// The world a paying program and a wallet share: the program's ledger holding `mint`, the
/// wallet's ledger without a slot for it, and the wallet's store as `set` leaves it.
fn payout_world(mint: Pubkey, set: impl FnOnce(&mut Session)) -> (Pubkey, Pubkey, Pubkey, Vec<(Pubkey, Account)>) {
    let prog = Pubkey::new_unique();
    let wallet = Pubkey::new_unique();
    let member = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&prog, true, &Pubkey::new_unique(), member, 4, |l| set_entry(l, 1, mint, 500));
    let (dst, dst_a) = make_ledger(&wallet, false, &wallet, Pubkey::default(), 4, |_| {});
    let (session, session_a) = make_session(&wallet, set);
    (prog, member, session, vec![(src, src_a), (dst, dst_a), (prog, system_account()), (session, session_a)])
}

#[test]
fn the_first_grant_for_a_program_adds_its_entry_and_the_owner_pays_its_rent() {
    let owner = wallet_key(0x10);
    let game = Pubkey::new_unique();
    let key = wallet_key(0x20);
    let (session, store) = make_session(&owner, |_| {});
    let owner_before = 10_000_000_000u64;
    let r = mollusk().process_instruction(&authorize_ix(session, (owner, true), game, key, 0), &with_owner(session, store, owner));
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let after = r.get_account(&session).unwrap();
    assert_eq!(after.data.len(), Session::space(1), "one entry's worth of bytes");
    assert_eq!(after.lamports, Rent::default().minimum_balance(Session::space(1)), "rent-exempt at its new size");
    let paid = owner_before - r.get_account(&owner).unwrap().lamports;
    assert_eq!(paid, after.lamports - Rent::default().minimum_balance(Session::space(0)));
    let s = read_session(after);
    assert_eq!(s.entries.len(), 1);
    assert_eq!(s.entries[0].program, game);
    assert_eq!(s.entries[0].ring[0], key);
    assert!(s.allows(&key, &game, 0));
}

#[test]
fn a_wallet_without_a_store_yet_gets_one_on_its_first_grant() {
    let owner = wallet_key(0x10);
    let game = Pubkey::new_unique();
    let key = wallet_key(0x20);
    let (session, _) = session_pda(&owner);
    let accounts = with_owner(session, Account::default(), owner);
    let r = mollusk().process_instruction(&authorize_ix(session, (owner, true), game, key, 0), &accounts);
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let after = r.get_account(&session).unwrap();
    assert_eq!(after.owner, vault::ID);
    assert_eq!(after.data.len(), Session::space(1));
    assert_eq!(after.lamports, Rent::default().minimum_balance(Session::space(1)));
    let s = read_session(after);
    assert_eq!(s.owner, owner);
    assert!(s.allows(&key, &game, 0));
}

#[test]
fn a_second_program_gets_its_own_entry_and_ring() {
    let owner = wallet_key(0x10);
    let (game, other) = (Pubkey::new_unique(), Pubkey::new_unique());
    let (session, store) = make_session(&owner, |s| s.entry_mut(game).0.grant(wallet_key(0x20)));
    let r = mollusk().process_instruction(&authorize_ix(session, (owner, true), other, wallet_key(0x21), 0), &with_owner(session, store, owner));
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let s = read_session(r.get_account(&session).unwrap());
    assert_eq!(s.entries.len(), 2);
    assert_eq!(r.get_account(&session).unwrap().data.len(), 512);
    assert!(s.allows(&wallet_key(0x20), &game, 0));
    assert!(s.allows(&wallet_key(0x21), &other, 0));
    assert!(!s.allows(&wallet_key(0x21), &game, 0));
}

#[test]
fn a_grant_needs_the_owner_and_a_real_key() {
    let owner = wallet_key(0x10);
    let game = Pubkey::new_unique();
    let key = wallet_key(0x20);
    let (session, store) = make_session(&owner, |_| {});
    let unsigned = mollusk().process_instruction(&authorize_ix(session, (owner, false), game, key, 0), &with_owner(session, store.clone(), owner));
    assert_eq!(unsigned.program_result, ProgramResult::Failure(ProgramError::MissingRequiredSignature));
    let stranger = Pubkey::new_unique();
    let by_stranger = mollusk().process_instruction(&authorize_ix(session, (stranger, true), game, key, 0), &with_owner(session, store.clone(), stranger));
    assert_eq!(by_stranger.program_result, ProgramResult::Failure(ProgramError::InvalidSeeds));
    let zero = mollusk().process_instruction(&authorize_ix(session, (owner, true), game, Pubkey::default(), 0), &with_owner(session, store, owner));
    assert_eq!(err_code(&zero), VaultError::BadAuthorizedKey as u32);
}

#[test]
fn a_temporary_key_takes_the_programs_one_slot_and_must_expire_in_the_future() {
    let owner = wallet_key(0x10);
    let game = Pubkey::new_unique();
    let (session, store) = make_session(&owner, |s| s.entry_mut(game).0.grant(wallet_key(0x20)));
    let mut mollusk = mollusk();
    mollusk.sysvars.clock.unix_timestamp = 1_800_000_000;
    let now = mollusk.sysvars.clock.unix_timestamp;
    let first = wallet_key(0x60);
    let r = mollusk.process_instruction(&authorize_ix(session, (owner, true), game, first, now + 3_600), &with_owner(session, store, owner));
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let after_first = r.get_account(&session).unwrap().clone();
    let e = read_session(&after_first).entries[0];
    assert_eq!(e.temporary, first);
    assert_eq!(e.ring[0], wallet_key(0x20), "the ring is untouched");
    assert_eq!(e.ring[1], Pubkey::default());

    let second = wallet_key(0x61);
    let r = mollusk.process_instruction(&authorize_ix(session, (owner, true), game, second, now + 60), &with_owner(session, after_first.clone(), owner));
    let s = read_session(r.get_account(&session).unwrap());
    assert_eq!(s.entries[0].temporary, second, "the next temporary session overwrites the last");
    assert!(!s.allows(&first, &game, now));
    assert!(s.allows(&second, &game, now));
    assert!(!s.allows(&second, &game, now + 60));

    let past = mollusk.process_instruction(&authorize_ix(session, (owner, true), game, first, now), &with_owner(session, after_first, owner));
    assert_eq!(err_code(&past), VaultError::BadExpiry as u32);
}

#[test]
fn a_revoke_forgets_one_key_and_the_zero_key_drops_the_program_with_its_rent_refunded() {
    let owner = wallet_key(0x10);
    let (game, other) = (Pubkey::new_unique(), Pubkey::new_unique());
    let keys: Vec<Pubkey> = (0..3u8).map(|n| wallet_key(0x40 + n)).collect();
    let (session, store) = make_session(&owner, |s| {
        let (e, _) = s.entry_mut(game);
        for k in &keys {
            e.grant(*k);
        }
        e.grant_temporary(wallet_key(0x70), i64::MAX);
        s.entry_mut(other).0.grant(wallet_key(0x71));
    });
    let r = mollusk().process_instruction(&revoke_ix(session, owner, game, keys[1]), &with_owner(session, store.clone(), owner));
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let s = read_session(r.get_account(&session).unwrap());
    assert!(s.allows(&keys[0], &game, 0));
    assert!(!s.allows(&keys[1], &game, 0));
    assert!(s.allows(&keys[2], &game, 0));
    assert_eq!(s.entries[0].ring[1], keys[2], "the ring closed up");

    let owner_before = 10_000_000_000u64;
    let r = mollusk().process_instruction(&revoke_ix(session, owner, game, Pubkey::default()), &with_owner(session, store, owner));
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let after = r.get_account(&session).unwrap();
    let s = read_session(after);
    assert_eq!(s.entries.len(), 1, "the game's entry went");
    assert_eq!(s.entries[0].program, other);
    assert_eq!(after.data.len(), Session::space(1));
    assert_eq!(after.lamports, Rent::default().minimum_balance(Session::space(1)));
    let refunded = r.get_account(&owner).unwrap().lamports - owner_before;
    assert_eq!(refunded, Rent::default().minimum_balance(Session::space(2)) - after.lamports);
}

#[test]
fn the_ring_holds_five_and_the_sixth_evicts_the_oldest() {
    let owner = wallet_key(0x10);
    let game = Pubkey::new_unique();
    let keys: Vec<Pubkey> = (0..RING as u8 + 1).map(|n| wallet_key(0x50 + n)).collect();
    let (session, mut store) = make_session(&owner, |_| {});
    for k in &keys {
        let r = mollusk().process_instruction(&authorize_ix(session, (owner, true), game, *k, 0), &with_owner(session, store.clone(), owner));
        assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
        store = r.get_account(&session).unwrap().clone();
    }
    let s = read_session(&store);
    assert!(!s.allows(&keys[0], &game, 0), "the first key is gone");
    assert!(keys[1..].iter().all(|k| s.allows(k, &game, 0)));
    assert_eq!(store.data.len(), Session::space(1), "one program, one entry, however many keys");
}

#[test]
fn a_session_key_consents_to_a_new_slot_for_its_own_program() {
    let m = token_mint(9);
    let consenter = wallet_key(0x30);
    let (prog, member, session, accounts) = payout_world(m, |_| {});
    let mut accounts = accounts;
    let (src, dst) = (accounts[0].0, accounts[1].0);
    // Granted for the paying program's member: the ledger's `authorized` names it.
    let (_, store) = accounts.iter_mut().find(|(k, _)| *k == session).unwrap();
    let mut s = read_session(store);
    s.entry_mut(member).0.grant(consenter);
    store.data = vec![0u8; Session::space(s.entries.len())];
    s.write_to(&mut store.data).unwrap();
    accounts.push((consenter, system_account()));

    let r = mollusk().process_instruction(&settle_new_slot_ix(src, dst, prog, consenter, Some(session), m), &accounts);
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let d = read_ledger(r.get_account(&dst).unwrap());
    assert_eq!(d.entries[d.index_of(&m).expect("credited")].amount, 100);
}

#[test]
fn a_key_for_another_program_an_expired_key_or_no_store_cannot_consent() {
    let m = token_mint(9);
    let consenter = wallet_key(0x30);
    let other_game = Pubkey::new_unique();
    let (prog, member, session, mut accounts) = payout_world(m, |s| s.entry_mut(other_game).0.grant(consenter));
    let (src, dst) = (accounts[0].0, accounts[1].0);
    accounts.push((consenter, system_account()));

    let wrong_program = mollusk().process_instruction(&settle_new_slot_ix(src, dst, prog, consenter, Some(session), m), &accounts);
    assert_eq!(err_code(&wrong_program), VaultError::NotAuthorizedToConsent as u32);

    let without_store = mollusk().process_instruction(&settle_new_slot_ix(src, dst, prog, consenter, None, m), &accounts);
    assert_eq!(err_code(&without_store), VaultError::NotAuthorizedToConsent as u32);

    let mut mollusk = mollusk();
    mollusk.sysvars.clock.unix_timestamp = 1_800_000_000;
    let now = mollusk.sysvars.clock.unix_timestamp;
    let rewrite = |accounts: &mut Vec<(Pubkey, Account)>, expires_at: i64| {
        let (_, store) = accounts.iter_mut().find(|(k, _)| *k == session).unwrap();
        let mut s = read_session(store);
        s.entry_mut(member).0.grant_temporary(consenter, expires_at);
        store.data = vec![0u8; Session::space(s.entries.len())];
        s.write_to(&mut store.data).unwrap();
    };
    rewrite(&mut accounts, now);
    let expired = mollusk.process_instruction(&settle_new_slot_ix(src, dst, prog, consenter, Some(session), m), &accounts);
    assert_eq!(err_code(&expired), VaultError::NotAuthorizedToConsent as u32);

    rewrite(&mut accounts, now + 1);
    let live = mollusk.process_instruction(&settle_new_slot_ix(src, dst, prog, consenter, Some(session), m), &accounts);
    assert_eq!(live.program_result, ProgramResult::Success, "{:?}", live.program_result);
}

#[test]
fn someone_elses_store_is_not_the_wallets() {
    let m = token_mint(9);
    let consenter = wallet_key(0x30);
    let (prog, member, _session, mut accounts) = payout_world(m, |_| {});
    let (src, dst) = (accounts[0].0, accounts[1].0);
    let (impostor_store, impostor_account) = make_session(&Pubkey::new_unique(), |s| s.entry_mut(member).0.grant(consenter));
    accounts.push((impostor_store, impostor_account));
    accounts.push((consenter, system_account()));
    let r = mollusk().process_instruction(&settle_new_slot_ix(src, dst, prog, consenter, Some(impostor_store), m), &accounts);
    assert_eq!(r.program_result, ProgramResult::Failure(ProgramError::InvalidSeeds));
}
