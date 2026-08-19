//! `settle` — the pure-bookkeeping move between two ledgers, and the slot discipline it relies
//! on (folded here from the old `ledger.rs` unit tests, now exercised through real instructions).
//!
//! Only the debited side must sign, so the directly-callable direction is wallet -> program
//! (the wallet signs). program -> wallet needs the PDA to sign via CPI, which is what
//! `settle_receipt` does with consent gathered up front — covered in `receipt.rs`.

mod common;

use anchor_lang::solana_program::pubkey::Pubkey;
use anchor_lang::InstructionData;
use common::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
};
use vault::state::{VaultError, SOL_MINT};

/// Builds a `settle` instruction with explicit signer flags for the two authority accounts —
/// `Settle` types them as `UncheckedAccount`, so the signer bit has to be set by hand.
fn settle_ix(
    src: Pubkey,
    dst: Pubkey,
    src_authority: Pubkey,
    src_signs: bool,
    dst_consenter: Pubkey,
    dst_signs: bool,
    mint: Pubkey,
    amount: u64,
) -> Instruction {
    let metas = vec![
        AccountMeta::new(src, false),
        AccountMeta::new(dst, false),
        AccountMeta::new_readonly(src_authority, src_signs),
        AccountMeta::new_readonly(dst_consenter, dst_signs),
    ];
    Instruction {
        program_id: vault::ID,
        accounts: metas,
        data: vault::instruction::Settle { mint, amount }.data(),
    }
}

/// An off-curve owner for a program ledger.
fn program_owner(seed: &[u8]) -> Pubkey {
    Pubkey::find_program_address(&[seed], &MEMBER_ID).0
}

#[tokio::test]
async fn wallet_to_program_moves_sol() {
    let wallet = Keypair::new();
    let prog = program_owner(b"house");

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |_| {});
    pt.add_account(wl, wa);
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    let ix = settle_ix(wl, pl, wallet.pubkey(), true, wallet.pubkey(), false, SOL_MINT, 400);
    send(&mut ctx, ix, &[&wallet]).await.unwrap();

    // slot 0 is always SOL, positionally; the arithmetic is a plain debit/credit.
    let w = read_ledger(&mut ctx, wl).await;
    let p = read_ledger(&mut ctx, pl).await;
    assert_eq!(w.entries[0].mint, SOL_MINT);
    assert_eq!(w.entries[0].amount, 600);
    assert_eq!(p.entries[0].mint, SOL_MINT);
    assert_eq!(p.entries[0].amount, 400);
}

#[tokio::test]
async fn token_debit_releases_its_slot_and_credit_claims_one() {
    let wallet = Keypair::new();
    let prog = program_owner(b"house");
    let m = token_mint(7);

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 1, m, 1_000);
    });
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |_| {});
    pt.add_account(wl, wa);
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    let free_before = read_ledger(&mut ctx, wl).await.free_slots();
    let ix = settle_ix(wl, pl, wallet.pubkey(), true, wallet.pubkey(), false, m, 1_000);
    send(&mut ctx, ix, &[&wallet]).await.unwrap();

    let w = read_ledger(&mut ctx, wl).await;
    let p = read_ledger(&mut ctx, pl).await;
    // debited to zero -> the slot is released back to the free band (mint reset to SOL_MINT)…
    assert_eq!(w.index_of(&m), None);
    assert_eq!(w.free_slots(), free_before + 1);
    // …and never mistaken for SOL, which stays positional at slot 0.
    assert_eq!(w.index_of(&SOL_MINT), Some(0));
    // the credit claimed a fresh non-zero slot on the program ledger.
    let i = p.index_of(&m).expect("program ledger claimed a slot for the mint");
    assert_ne!(i, 0);
    assert_eq!(p.entries[i].amount, 1_000);
}

#[tokio::test]
async fn two_wallets_is_not_a_payment_rail() {
    let alice = Keypair::new();
    let bob = Keypair::new();

    let mut pt = program_test();
    let (al, aa) = make_ledger(&alice.pubkey(), false, &alice.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (bl, ba) = make_ledger(&bob.pubkey(), false, &bob.pubkey(), Pubkey::default(), 4, |_| {});
    pt.add_account(al, aa);
    pt.add_account(bl, ba);
    let mut ctx = pt.start_with_context().await;

    let ix = settle_ix(al, bl, alice.pubkey(), true, bob.pubkey(), true, SOL_MINT, 100);
    let err = send(&mut ctx, ix, &[&alice, &bob]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NotProgramMediated));
}

#[tokio::test]
async fn the_debited_owner_must_sign() {
    let wallet = Keypair::new();
    let prog = program_owner(b"house");

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |_| {});
    pt.add_account(wl, wa);
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    // src_authority names the wallet but does not sign.
    let ix = settle_ix(wl, pl, wallet.pubkey(), false, wallet.pubkey(), false, SOL_MINT, 100);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::MissingUserSignature));
}

/// The reserve-drain guard: aliasing src and dst onto one ledger is rejected before the handler
/// runs. Uses a program ledger — the population that could otherwise satisfy the one-human check.
#[tokio::test]
async fn self_settle_is_rejected() {
    let prog = program_owner(b"house");

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    let ix = settle_ix(pl, pl, prog, false, prog, false, SOL_MINT, 1);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::DuplicateLedger));
}

#[tokio::test]
async fn a_full_ledger_refuses_a_new_mint() {
    let wallet = Keypair::new();
    let prog = program_owner(b"house");
    let n = token_mint(9);
    let x = token_mint(3);

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 1, n, 500);
    });
    // capacity 2: slot 0 SOL, slot 1 already taken by x -> no free slot to claim.
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 2, |l| {
        set_entry(l, 1, x, 1);
    });
    pt.add_account(wl, wa);
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    let ix = settle_ix(wl, pl, wallet.pubkey(), true, wallet.pubkey(), false, n, 500);
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::LedgerFull));
}
