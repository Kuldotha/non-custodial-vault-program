//! `settle_receipt` — settles a fabricated receipt and calls back into the member program.
//!
//! `create_receipt` cannot run locally (it CPIs the ephemeral vault to mint the account), so the
//! receipt bytes are fabricated directly — exactly the layout `create_receipt` would write — and
//! the settle path is exercised end-to-end, callback included, via a mock member program.

mod common;

use anchor_lang::solana_program::pubkey::Pubkey;
use common::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
};
use vault::state::{VaultError, SOL_MINT};

const DISC: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
const SLOT: u64 = 10;

fn program_owner(seed: &[u8]) -> Pubkey {
    Pubkey::find_program_address(&[seed], &MEMBER_ID).0
}

fn settle_receipt_ix(
    receipt: Pubkey,
    authority: Pubkey,
    callback_program: Pubkey,
    ledgers: &[Pubkey],
    forwarded: &[Pubkey],
) -> Instruction {
    let (va, _) = vault_authority();
    let mut extra: Vec<AccountMeta> = ledgers.iter().map(|k| AccountMeta::new(*k, false)).collect();
    extra.extend(forwarded.iter().map(|k| AccountMeta::new(*k, false)));
    vault_ix(
        vault::accounts::SettleReceipt {
            receipt,
            authority,
            callback_program,
            vault_authority: va,
        },
        vault::instruction::SettleReceipt {},
        &extra,
    )
}

#[tokio::test]
async fn program_pays_wallet_and_the_callback_fires() {
    let prog = program_owner(b"house");
    let wallet = Keypair::new();
    let authority = program_owner(b"authority");
    let m = token_mint(7);

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |l| {
        set_entry(l, 1, m, 1_000);
    });
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    let (rpda, _) = Pubkey::find_program_address(
        &[b"receipt", authority.as_ref(), prog.as_ref()],
        &vault::ID,
    );
    let flag = Keypair::new().pubkey();

    pt.add_account(pl, pa);
    pt.add_account(wl, wa);
    pt.add_account(flag, opaque_account(MEMBER_ID, 1));
    let mut ctx = pt.start_with_context().await;
    ctx.warp_to_slot(SLOT).unwrap();

    let bytes = receipt_bytes(
        &[prog, wallet.pubkey()],
        &authority,
        &MEMBER_ID,
        DISC,
        SLOT,
        &[Mv { mint: m, amount: 1_000, from: 0, to: 1 }],
        &[],
    );
    ctx.set_account(&rpda, &receipt_account(bytes).into());

    let ix = settle_receipt_ix(rpda, authority, MEMBER_ID, &[pl, wl], &[flag]);
    send(&mut ctx, ix, &[]).await.unwrap();

    // value moved: the program's slot released, the wallet claimed one.
    let p = read_ledger(&mut ctx, pl).await;
    let w = read_ledger(&mut ctx, wl).await;
    assert_eq!(p.index_of(&m), None);
    assert_eq!(w.entries[w.index_of(&m).expect("wallet credited")].amount, 1_000);

    // the callback ran (it flipped the flag) and the receipt was consumed and handed over.
    let flag_acct = ctx.banks_client.get_account(flag).await.unwrap().unwrap();
    assert_eq!(flag_acct.data[0], 1, "callback did not run");
    let receipt_acct = ctx.banks_client.get_account(rpda).await.unwrap().unwrap();
    assert_eq!(receipt_acct.owner, MEMBER_ID, "receipt not handed to member program");
    assert!(receipt_acct.data.iter().all(|b| *b == 0), "receipt not zeroed");
}

/// A `Ledger` is program-owned too, so the `owner = crate::ID` constraint alone would let one be
/// passed where a receipt belongs — and settle runs no consent. The 8-byte discriminator is what
/// rejects it: a `Ledger`'s discriminator is not the receipt's.
#[tokio::test]
async fn a_ledger_cannot_be_passed_as_a_receipt() {
    let prog = program_owner(b"house");
    let authority = program_owner(b"authority");

    let mut pt = program_test();
    // A real, program-owned ledger with a balance — exactly the account an attacker would try to
    // reinterpret as a receipt to move value with no approval.
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;
    ctx.warp_to_slot(SLOT).unwrap();

    // Pass the ledger itself in the receipt slot.
    let ix = settle_receipt_ix(pl, authority, MEMBER_ID, &[], &[]);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NotAReceipt));
}

#[tokio::test]
async fn a_receipt_from_another_slot_is_expired() {
    let prog = program_owner(b"house");
    let wallet = Keypair::new();
    let authority = program_owner(b"authority");

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    let (rpda, _) =
        Pubkey::find_program_address(&[b"receipt", authority.as_ref(), prog.as_ref()], &vault::ID);
    pt.add_account(pl, pa);
    pt.add_account(wl, wa);
    let mut ctx = pt.start_with_context().await;
    ctx.warp_to_slot(SLOT).unwrap();

    // created in a different slot than the current one.
    let bytes = receipt_bytes(
        &[prog, wallet.pubkey()],
        &authority,
        &MEMBER_ID,
        DISC,
        SLOT + 1,
        &[Mv { mint: SOL_MINT, amount: 1, from: 0, to: 1 }],
        &[],
    );
    ctx.set_account(&rpda, &receipt_account(bytes).into());

    let ix = settle_receipt_ix(rpda, authority, MEMBER_ID, &[pl, wl], &[]);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::ReceiptExpired));
}

#[tokio::test]
async fn the_callback_program_must_be_the_proven_member() {
    let prog = program_owner(b"house");
    let wallet = Keypair::new();
    let authority = program_owner(b"authority");

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    let (rpda, _) =
        Pubkey::find_program_address(&[b"receipt", authority.as_ref(), prog.as_ref()], &vault::ID);
    pt.add_account(pl, pa);
    pt.add_account(wl, wa);
    let mut ctx = pt.start_with_context().await;
    ctx.warp_to_slot(SLOT).unwrap();

    let bytes = receipt_bytes(
        &[prog, wallet.pubkey()],
        &authority,
        &MEMBER_ID,
        DISC,
        SLOT,
        &[Mv { mint: SOL_MINT, amount: 1, from: 0, to: 1 }],
        &[],
    );
    ctx.set_account(&rpda, &receipt_account(bytes).into());

    // pass some other program as the callback.
    let wrong = program_owner(b"not-the-member");
    let ix = settle_receipt_ix(rpda, authority, wrong, &[pl, wl], &[]);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::CallbackProgramMismatch));
}

#[tokio::test]
async fn the_authority_must_match_the_receipt() {
    let prog = program_owner(b"house");
    let wallet = Keypair::new();
    let authority = program_owner(b"authority");

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    let (rpda, _) =
        Pubkey::find_program_address(&[b"receipt", authority.as_ref(), prog.as_ref()], &vault::ID);
    pt.add_account(pl, pa);
    pt.add_account(wl, wa);
    let mut ctx = pt.start_with_context().await;
    ctx.warp_to_slot(SLOT).unwrap();

    let bytes = receipt_bytes(
        &[prog, wallet.pubkey()],
        &authority,
        &MEMBER_ID,
        DISC,
        SLOT,
        &[Mv { mint: SOL_MINT, amount: 1, from: 0, to: 1 }],
        &[],
    );
    ctx.set_account(&rpda, &receipt_account(bytes).into());

    let wrong_authority = program_owner(b"someone-else");
    let ix = settle_receipt_ix(rpda, wrong_authority, MEMBER_ID, &[pl, wl], &[]);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::BadAuthority));
}

/// The receipt path's own duplicate-ledger guard (the second "settle"): a receipt naming one
/// owner twice, settled with the same account passed twice, is rejected. The code is off-enum:
/// 7000 + check(4)*100 + index(1).
#[tokio::test]
async fn a_receipt_naming_a_ledger_twice_is_rejected() {
    let prog = program_owner(b"house");
    let authority = program_owner(b"authority");

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), MEMBER_ID, 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (rpda, _) =
        Pubkey::find_program_address(&[b"receipt", authority.as_ref(), prog.as_ref()], &vault::ID);
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;
    ctx.warp_to_slot(SLOT).unwrap();

    // owners = [prog, prog] — a duplicate the real create_receipt would reject, fabricated here.
    let bytes = receipt_bytes(
        &[prog, prog],
        &authority,
        &MEMBER_ID,
        DISC,
        SLOT,
        &[Mv { mint: SOL_MINT, amount: 1, from: 0, to: 1 }],
        &[],
    );
    ctx.set_account(&rpda, &receipt_account(bytes).into());

    let ix = settle_receipt_ix(rpda, authority, MEMBER_ID, &[pl, pl], &[]);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), 7000 + 4 * 100 + 1);
}

#[tokio::test]
async fn a_receipt_between_two_wallets_is_not_a_payment_rail() {
    let alice = Keypair::new();
    let bob = Keypair::new();
    let authority = program_owner(b"authority");

    let mut pt = program_test();
    let (al, aa) = make_ledger(&alice.pubkey(), false, &alice.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (bl, ba) = make_ledger(&bob.pubkey(), false, &bob.pubkey(), Pubkey::default(), 4, |_| {});
    let (rpda, _) = Pubkey::find_program_address(
        &[b"receipt", authority.as_ref(), alice.pubkey().as_ref()],
        &vault::ID,
    );
    pt.add_account(al, aa);
    pt.add_account(bl, ba);
    let mut ctx = pt.start_with_context().await;
    ctx.warp_to_slot(SLOT).unwrap();

    let bytes = receipt_bytes(
        &[alice.pubkey(), bob.pubkey()],
        &authority,
        &MEMBER_ID,
        DISC,
        SLOT,
        &[Mv { mint: SOL_MINT, amount: 100, from: 0, to: 1 }],
        &[],
    );
    ctx.set_account(&rpda, &receipt_account(bytes).into());

    let ix = settle_receipt_ix(rpda, authority, MEMBER_ID, &[al, bl], &[]);
    let err = send(&mut ctx, ix, &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NotProgramMediated));
}
