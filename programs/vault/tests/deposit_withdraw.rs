//! `deposit` and `withdraw` — the wallet-only value paths, SOL and SPL.
//!
//! First-ever deposit creates the ledger's permission (a CPI to the MagicBlock permission
//! program) — not reachable locally, so these tests seed an existing ledger and a non-empty
//! permission so the create is skipped. The off-curve-owner rejection isn't reachable either: it
//! needs an off-curve *signer*, which no keypair can produce; it's a CPI-only guard.

mod common;

use anchor_lang::solana_program::pubkey::Pubkey;
use common::*;
use solana_sdk::signature::{Keypair, Signer};
use vault::state::{VaultError, SOL_MINT};

fn vault_floor() -> u64 {
    solana_sdk::rent::Rent::default().minimum_balance(0)
}

/// A wallet ledger with a permission already present, so deposit skips the permission CPI.
fn seeded_wallet(
    pt: &mut solana_program_test::ProgramTest,
    wallet: &Pubkey,
    slots: usize,
    set: impl FnOnce(&mut vault::state::Ledger),
) -> (Pubkey, Pubkey) {
    let (lpda, lacct) = make_ledger(wallet, false, wallet, Pubkey::default(), slots, set);
    pt.add_account(lpda, lacct);
    // permission account: any non-empty account makes `data_is_empty()` false.
    let perm = Keypair::new().pubkey();
    pt.add_account(perm, opaque_account(vault::ID, 8));
    (lpda, perm)
}

#[tokio::test]
async fn deposit_sol_credits_the_ledger_and_funds_the_reserve() {
    let wallet = Keypair::new();
    let mut pt = program_test();
    let (lpda, perm) = seeded_wallet(&mut pt, &wallet.pubkey(), 32, |l| set_entry(l, 0, SOL_MINT, 0));
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let (vault, _) = vault_pda();
    pt.add_account(vault, system_account(vault_floor()));
    let mut ctx = pt.start_with_context().await;

    let vault_before = lamports_of(&mut ctx, vault).await;
    let ix = vault_ix(
        vault::accounts::Deposit {
            owner: wallet.pubkey(),
            ledger: lpda,
            permission: perm,
            permission_program: permission_program(),
            vault,
            vault_token: wallet.pubkey(),
            owner_token: wallet.pubkey(),
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::Deposit {
            mint: SOL_MINT,
            amount: 500,
            min_free: Some(0),
            slot_increase: Some(0),
        },
        &[],
    );
    send(&mut ctx, ix, &[&wallet]).await.unwrap();

    assert_eq!(read_ledger(&mut ctx, lpda).await.entries[0].amount, 500);
    assert_eq!(lamports_of(&mut ctx, vault).await, vault_before + 500);
}

#[tokio::test]
async fn deposit_sol_needs_the_vault_initialized() {
    let wallet = Keypair::new();
    let mut pt = program_test();
    let (lpda, perm) = seeded_wallet(&mut pt, &wallet.pubkey(), 32, |l| set_entry(l, 0, SOL_MINT, 0));
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let (vault, _) = vault_pda();
    // vault below its rent floor.
    pt.add_account(vault, system_account(1));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::Deposit {
            owner: wallet.pubkey(),
            ledger: lpda,
            permission: perm,
            permission_program: permission_program(),
            vault,
            vault_token: wallet.pubkey(),
            owner_token: wallet.pubkey(),
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::Deposit {
            mint: SOL_MINT,
            amount: 500,
            min_free: Some(0),
            slot_increase: Some(0),
        },
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::VaultNotInitialized));
}

#[tokio::test]
async fn deposit_spl_moves_tokens_into_the_reserve() {
    let wallet = Keypair::new();
    let m = token_mint(21);
    let (vault, _) = vault_pda();
    let reserve = vault_reserve(&m);
    let owner_token = Keypair::new().pubkey();

    let mut pt = program_test();
    let (lpda, perm) = seeded_wallet(&mut pt, &wallet.pubkey(), 32, |l| set_entry(l, 0, SOL_MINT, 0));
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    pt.add_account(vault, system_account(vault_floor()));
    pt.add_account(reserve, spl_token_account(m, vault, 0));
    pt.add_account(owner_token, spl_token_account(m, wallet.pubkey(), 1_000));
    let mut ctx = pt.start_with_context().await;

    let mut ix = vault_ix(
        vault::accounts::Deposit {
            owner: wallet.pubkey(),
            ledger: lpda,
            permission: perm,
            permission_program: permission_program(),
            vault,
            vault_token: reserve,
            owner_token,
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::Deposit {
            mint: m,
            amount: 700,
            min_free: Some(0),
            slot_increase: Some(0),
        },
        &[],
    );
    mark_writable(&mut ix, &[reserve, owner_token]);
    send(&mut ctx, ix, &[&wallet]).await.unwrap();

    let l = read_ledger(&mut ctx, lpda).await;
    assert_eq!(l.entries[l.index_of(&m).unwrap()].amount, 700);
    assert_eq!(token_balance(&mut ctx, reserve).await, 700);
    assert_eq!(token_balance(&mut ctx, owner_token).await, 300);
}

#[tokio::test]
async fn withdraw_sol_pays_the_owner() {
    let wallet = Keypair::new();
    let mut pt = program_test();
    let (lpda, _) = seeded_wallet(&mut pt, &wallet.pubkey(), 32, |l| set_entry(l, 0, SOL_MINT, 500));
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let (vault, _) = vault_pda();
    pt.add_account(vault, system_account(vault_floor() + 500));
    let mut ctx = pt.start_with_context().await;

    let owner_before = lamports_of(&mut ctx, wallet.pubkey()).await;
    let ix = vault_ix(
        vault::accounts::Withdraw {
            owner: wallet.pubkey(),
            ledger: lpda,
            vault,
            vault_token: wallet.pubkey(),
            owner_token: wallet.pubkey(),
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::Withdraw { mint: SOL_MINT, amount: 500 },
        &[],
    );
    send(&mut ctx, ix, &[&wallet]).await.unwrap();

    assert_eq!(read_ledger(&mut ctx, lpda).await.entries[0].amount, 0);
    assert_eq!(lamports_of(&mut ctx, wallet.pubkey()).await, owner_before + 500);
}

#[tokio::test]
async fn withdraw_sol_cannot_exceed_the_reserve() {
    let wallet = Keypair::new();
    let mut pt = program_test();
    let (lpda, _) = seeded_wallet(&mut pt, &wallet.pubkey(), 32, |l| set_entry(l, 0, SOL_MINT, 1_000));
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let (vault, _) = vault_pda();
    // ledger claims 1000 but the reserve can only spend 100 above its floor.
    pt.add_account(vault, system_account(vault_floor() + 100));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::Withdraw {
            owner: wallet.pubkey(),
            ledger: lpda,
            vault,
            vault_token: wallet.pubkey(),
            owner_token: wallet.pubkey(),
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::Withdraw { mint: SOL_MINT, amount: 1_000 },
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::InsufficientReserve));
}

#[tokio::test]
async fn withdraw_of_an_unheld_mint_has_no_balance() {
    let wallet = Keypair::new();
    let m = token_mint(21);
    let mut pt = program_test();
    let (lpda, _) = seeded_wallet(&mut pt, &wallet.pubkey(), 32, |l| set_entry(l, 0, SOL_MINT, 0));
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let (vault, _) = vault_pda();
    pt.add_account(vault, system_account(vault_floor()));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::Withdraw {
            owner: wallet.pubkey(),
            ledger: lpda,
            vault,
            vault_token: wallet.pubkey(),
            owner_token: wallet.pubkey(),
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::Withdraw { mint: m, amount: 1 },
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NoBalance));
}

#[tokio::test]
async fn withdraw_spl_pays_the_owner_token_account() {
    let wallet = Keypair::new();
    let m = token_mint(21);
    let (vault, _) = vault_pda();
    let reserve = vault_reserve(&m);
    let owner_token = Keypair::new().pubkey();

    let mut pt = program_test();
    let (lpda, _) = seeded_wallet(&mut pt, &wallet.pubkey(), 32, |l| set_entry(l, 1, m, 400));
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    pt.add_account(vault, system_account(vault_floor()));
    pt.add_account(reserve, spl_token_account(m, vault, 400));
    pt.add_account(owner_token, spl_token_account(m, wallet.pubkey(), 0));
    let mut ctx = pt.start_with_context().await;

    let mut ix = vault_ix(
        vault::accounts::Withdraw {
            owner: wallet.pubkey(),
            ledger: lpda,
            vault,
            vault_token: reserve,
            owner_token,
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::Withdraw { mint: m, amount: 400 },
        &[],
    );
    mark_writable(&mut ix, &[reserve, owner_token]);
    send(&mut ctx, ix, &[&wallet]).await.unwrap();

    assert_eq!(read_ledger(&mut ctx, lpda).await.index_of(&m), None);
    assert_eq!(token_balance(&mut ctx, reserve).await, 0);
    assert_eq!(token_balance(&mut ctx, owner_token).await, 400);
}

