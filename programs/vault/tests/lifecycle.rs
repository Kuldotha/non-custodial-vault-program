//! Lifecycle and consent instructions with a wallet (or no) signer: initialize, authorize,
//! open/privacy validation, close, and the undelegate authority check (folded from the old
//! `may_end_session` unit test). Permission/ephemeral-CPI happy paths are devnet-only; their
//! pre-CPI validation is covered here.

mod common;

use anchor_lang::solana_program::pubkey::Pubkey;
use common::*;
use solana_sdk::signature::{Keypair, Signer};
use vault::state::{VaultError, SOL_MINT};

fn floor() -> u64 {
    solana_sdk::rent::Rent::default().minimum_balance(0)
}

#[tokio::test]
async fn initialize_vault_funds_the_reserve_to_its_floor() {
    let pt = program_test();
    let (vault, _) = vault_pda();
    let mut ctx = pt.start_with_context().await;

    let payer = ctx.payer.pubkey();
    let ix = vault_ix(
        vault::accounts::InitializeVault { payer, vault, system_program: system_program_id() },
        vault::instruction::InitializeVault {},
        &[],
    );
    send(&mut ctx, ix, &[]).await.unwrap();
    assert_eq!(lamports_of(&mut ctx, vault).await, floor());

    // idempotent: a second call is a no-op, not a double-fund.
    let ix = vault_ix(
        vault::accounts::InitializeVault { payer, vault, system_program: system_program_id() },
        vault::instruction::InitializeVault {},
        &[],
    );
    send(&mut ctx, ix, &[]).await.unwrap();
    assert_eq!(lamports_of(&mut ctx, vault).await, floor());
}

#[tokio::test]
async fn assign_sets_a_session_key() {
    let wallet = Keypair::new();
    let session = Keypair::new().pubkey();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    pt.add_account(wl, wa);
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::AssignLedgerAuthorization { ledger: wl, owner: wallet.pubkey() },
        vault::instruction::AssignLedgerAuthorization { authorized: session },
        &[],
    );
    send(&mut ctx, ix, &[&wallet]).await.unwrap();
    assert_eq!(read_ledger(&mut ctx, wl).await.authorized, session);
}

#[tokio::test]
async fn a_session_key_must_be_on_curve() {
    let wallet = Keypair::new();
    let off_curve = vault_pda().0; // a PDA — off-curve

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    pt.add_account(wl, wa);
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::AssignLedgerAuthorization { ledger: wl, owner: wallet.pubkey() },
        vault::instruction::AssignLedgerAuthorization { authorized: off_curve },
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::BadAuthorizedKey));
}

#[tokio::test]
async fn a_program_ledger_cannot_carry_a_session_key() {
    let (prog, _) = proxy_pda(b"house");

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), PROXY_ID, 4, |_| {});
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    let inner = vault_ix(
        vault::accounts::AssignLedgerAuthorization { ledger: pl, owner: prog },
        vault::instruction::AssignLedgerAuthorization { authorized: Keypair::new().pubkey() },
        &[],
    );
    let err = send(&mut ctx, via_proxy(b"house", inner), &[]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::CannotAuthorizePdaLedger));
}

#[tokio::test]
async fn making_a_wallet_private_again_needs_the_permission_gone() {
    let wallet = Keypair::new();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    let permission = Keypair::new().pubkey();
    pt.add_account(wl, wa);
    pt.add_account(permission, opaque_account(permission_program(), 8)); // already present
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::MakeWalletLedgerPrivate {
            owner: wallet.pubkey(),
            ledger: wl,
            permission,
            permission_program: permission_program(),
            system_program: system_program_id(),
        },
        vault::instruction::MakeWalletLedgerPrivate {},
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::PermissionExists));
}

#[tokio::test]
async fn only_the_rent_payer_may_make_a_ledger_public() {
    let wallet = Keypair::new();
    let stranger = Keypair::new();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    let permission = Keypair::new().pubkey();
    pt.add_account(wl, wa);
    pt.add_account(permission, opaque_account(permission_program(), 8));
    pt.add_account(stranger.pubkey(), system_account(1_000_000_000));
    let mut ctx = pt.start_with_context().await;

    // payer is not the recorded rent payer -> the address constraint rejects it.
    let ix = vault_ix(
        vault::accounts::MakePublic {
            owner: wallet.pubkey(),
            ledger: wl,
            permission,
            payer: stranger.pubkey(),
            permission_program: permission_program(),
        },
        vault::instruction::MakePublic {},
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet, &stranger]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NotRentPayer));
}

#[tokio::test]
async fn opening_a_wallet_ledger_bounds_the_slot_count() {
    let wallet = Keypair::new();
    let (wl, _) = ledger_pda(&wallet.pubkey());

    let mut pt = program_test();
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::OpenWalletLedger {
            owner: wallet.pubkey(),
            ledger: wl,
            permission: Keypair::new().pubkey(),
            permission_program: permission_program(),
            system_program: system_program_id(),
        },
        vault::instruction::OpenWalletLedger { slots: 0 },
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::BadSlotCount));
}

#[tokio::test]
async fn opening_a_ledger_that_exists_is_refused() {
    let wallet = Keypair::new();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    pt.add_account(wl, wa); // already exists
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::OpenWalletLedger {
            owner: wallet.pubkey(),
            ledger: wl,
            permission: Keypair::new().pubkey(),
            permission_program: permission_program(),
            system_program: system_program_id(),
        },
        vault::instruction::OpenWalletLedger { slots: 4 },
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::LedgerExists));
}

#[tokio::test]
async fn close_sweeps_sol_and_closes_the_ledger() {
    let wallet = Keypair::new();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 0, SOL_MINT, 300);
    });
    let (vault, _) = vault_pda();
    pt.add_account(wl, wa);
    pt.add_account(vault, system_account(floor() + 300));
    let mut ctx = pt.start_with_context().await;

    // owner == rent_payer == wallet; a fresh (empty) permission makes the close skip its CPI.
    let ix = vault_ix(
        vault::accounts::CloseLedger {
            owner: wallet.pubkey(),
            rent_payer: wallet.pubkey(),
            ledger: wl,
            vault,
            permission: Keypair::new().pubkey(),
            permission_program: permission_program(),
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::CloseLedger {},
        &[],
    );
    send(&mut ctx, ix, &[&wallet]).await.unwrap();

    assert!(
        ctx.banks_client.get_account(wl).await.unwrap().is_none(),
        "ledger should be closed"
    );
    assert!(lamports_of(&mut ctx, wallet.pubkey()).await >= 300, "swept SOL not received");
}

#[tokio::test]
async fn close_requires_the_recorded_rent_payer() {
    let wallet = Keypair::new();
    let stranger = Keypair::new();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    let (vault, _) = vault_pda();
    pt.add_account(wl, wa);
    pt.add_account(vault, system_account(floor()));
    pt.add_account(stranger.pubkey(), system_account(1_000_000_000));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::CloseLedger {
            owner: wallet.pubkey(),
            rent_payer: stranger.pubkey(),
            ledger: wl,
            vault,
            permission: Keypair::new().pubkey(),
            permission_program: permission_program(),
            token_program: spl_token_id(),
            system_program: system_program_id(),
        },
        vault::instruction::CloseLedger {},
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet, &stranger]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NotRentPayer));
}

/// Undelegation once had no authority check — any key could push any ledger home. A stranger is
/// now refused before the (validator-native) commit CPI is ever reached.
#[tokio::test]
async fn undelegate_refuses_a_stranger() {
    let owner = Keypair::new();
    let stranger = Keypair::new();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&owner.pubkey(), false, &owner.pubkey(), Pubkey::default(), 4, |_| {});
    pt.add_account(wl, wa);
    pt.add_account(stranger.pubkey(), system_account(1_000_000_000));
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::Undelegate {
            payer: stranger.pubkey(),
            ledger: wl,
            magic_context: magic_context(),
            magic_program: magic_program(),
        },
        vault::instruction::Undelegate {},
        &[],
    );
    let err = send(&mut ctx, ix, &[&stranger]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NotAuthorizedToConsent));
}
