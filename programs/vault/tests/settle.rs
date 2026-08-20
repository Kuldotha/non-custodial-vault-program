//! `settle` — value between two ledgers, pure bookkeeping. No CPI, so it runs end to end in mollusk.

mod common;

use common::*;
use mollusk_svm::result::{InstructionResult, ProgramResult};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program_error::ProgramError,
    pubkey::Pubkey,
};
use vault::constants::SOL_MINT;
use vault::error::VaultError;

fn settle_ix(
    src: Pubkey,
    dst: Pubkey,
    src_authority: (Pubkey, bool),
    dst_consenter: (Pubkey, bool),
    mint: Pubkey,
    amount: u64,
) -> Instruction {
    let mut data = SETTLE_DISC.to_vec();
    data.extend_from_slice(mint.as_ref());
    data.extend_from_slice(&amount.to_le_bytes());
    Instruction {
        program_id: vault::ID,
        accounts: vec![
            AccountMeta::new(src, false),
            AccountMeta::new(dst, false),
            AccountMeta::new_readonly(src_authority.0, src_authority.1),
            AccountMeta::new_readonly(dst_consenter.0, dst_consenter.1),
        ],
        data,
    }
}

fn err_code(r: &InstructionResult) -> u32 {
    match &r.program_result {
        ProgramResult::Failure(ProgramError::Custom(c)) => *c,
        other => panic!("expected a custom error, got {other:?}"),
    }
}

fn code(e: VaultError) -> u32 {
    e as u32
}

#[test]
fn program_pays_wallet_an_existing_mint() {
    let m = token_mint(7);
    let prog = Pubkey::new_unique();
    let wallet = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&prog, true, &Pubkey::new_unique(), Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 1_000);
    });
    let (dst, dst_a) = make_ledger(&wallet, false, &wallet, Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 5);
    });
    let consenter = Pubkey::new_unique();

    let ix = settle_ix(src, dst, (prog, true), (consenter, false), m, 100);
    let accounts = vec![
        (src, src_a),
        (dst, dst_a),
        (prog, system_account()),
        (consenter, system_account()),
    ];

    let r = mollusk().process_instruction(&ix, &accounts);
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    assert_eq!(read_ledger(r.get_account(&src).unwrap()).entries[1].amount, 900);
    assert_eq!(read_ledger(r.get_account(&dst).unwrap()).entries[1].amount, 105);
}

#[test]
fn program_pays_wallet_a_new_mint_with_consent() {
    let m = token_mint(9);
    let prog = Pubkey::new_unique();
    let wallet = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&prog, true, &Pubkey::new_unique(), Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 500);
    });
    let (dst, dst_a) = make_ledger(&wallet, false, &wallet, Pubkey::default(), 4, |_| {});

    // The wallet owner consents to the new slot by signing as dst_consenter.
    let ix = settle_ix(src, dst, (prog, true), (wallet, true), m, 200);
    let accounts = vec![
        (src, src_a),
        (dst, dst_a),
        (prog, system_account()),
        (wallet, system_account()),
    ];

    let r = mollusk().process_instruction(&ix, &accounts);
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    let d = read_ledger(r.get_account(&dst).unwrap());
    assert_eq!(d.entries[d.index_of(&m).expect("credited")].amount, 200);
}

#[test]
fn a_new_mint_without_consent_is_rejected() {
    let m = token_mint(9);
    let prog = Pubkey::new_unique();
    let wallet = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&prog, true, &Pubkey::new_unique(), Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 500);
    });
    let (dst, dst_a) = make_ledger(&wallet, false, &wallet, Pubkey::default(), 4, |_| {});

    // dst_consenter does not sign.
    let ix = settle_ix(src, dst, (prog, true), (wallet, false), m, 200);
    let accounts = vec![
        (src, src_a),
        (dst, dst_a),
        (prog, system_account()),
        (wallet, system_account()),
    ];

    let r = mollusk().process_instruction(&ix, &accounts);
    assert_eq!(err_code(&r), code(VaultError::MissingUserSignature));
}

#[test]
fn two_wallets_is_not_a_payment_rail() {
    let m = token_mint(3);
    let alice = Pubkey::new_unique();
    let bob = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&alice, false, &alice, Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 1_000);
    });
    let (dst, dst_a) = make_ledger(&bob, false, &bob, Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 0);
    });

    let ix = settle_ix(src, dst, (alice, true), (bob, true), m, 100);
    let accounts = vec![
        (src, src_a),
        (dst, dst_a),
        (alice, system_account()),
        (bob, system_account()),
    ];

    let r = mollusk().process_instruction(&ix, &accounts);
    assert_eq!(err_code(&r), code(VaultError::NotProgramMediated));
}

#[test]
fn the_debited_program_side_must_sign() {
    let m = token_mint(7);
    let prog = Pubkey::new_unique();
    let wallet = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&prog, true, &Pubkey::new_unique(), Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 1_000);
    });
    let (dst, dst_a) = make_ledger(&wallet, false, &wallet, Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 5);
    });

    // src_authority (the program side) is not a signer.
    let ix = settle_ix(src, dst, (prog, false), (wallet, false), m, 100);
    let accounts = vec![
        (src, src_a),
        (dst, dst_a),
        (prog, system_account()),
        (wallet, system_account()),
    ];

    let r = mollusk().process_instruction(&ix, &accounts);
    assert_eq!(err_code(&r), code(VaultError::MissingProgramSignature));
}

#[test]
fn src_authority_must_be_the_owner() {
    let m = token_mint(7);
    let prog = Pubkey::new_unique();
    let wallet = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&prog, true, &Pubkey::new_unique(), Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 1_000);
    });
    let (dst, dst_a) = make_ledger(&wallet, false, &wallet, Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 5);
    });
    let impostor = Pubkey::new_unique();

    let ix = settle_ix(src, dst, (impostor, true), (wallet, false), m, 100);
    let accounts = vec![
        (src, src_a),
        (dst, dst_a),
        (impostor, system_account()),
        (wallet, system_account()),
    ];

    let r = mollusk().process_instruction(&ix, &accounts);
    assert_eq!(err_code(&r), code(VaultError::BadAuthority));
}

#[test]
fn program_to_program_sol_moves() {
    let house = Pubkey::new_unique();
    let jackpot = Pubkey::new_unique();
    let (src, src_a) = make_ledger(&house, true, &Pubkey::new_unique(), Pubkey::default(), 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (dst, dst_a) = make_ledger(&jackpot, true, &Pubkey::new_unique(), Pubkey::default(), 4, |_| {});
    let consenter = Pubkey::new_unique();

    let ix = settle_ix(src, dst, (house, true), (consenter, false), SOL_MINT, 250);
    let accounts = vec![
        (src, src_a),
        (dst, dst_a),
        (house, system_account()),
        (consenter, system_account()),
    ];

    let r = mollusk().process_instruction(&ix, &accounts);
    assert_eq!(r.program_result, ProgramResult::Success, "{:?}", r.program_result);
    assert_eq!(read_ledger(r.get_account(&src).unwrap()).entries[0].amount, 750);
    assert_eq!(read_ledger(r.get_account(&dst).unwrap()).entries[0].amount, 250);
}
