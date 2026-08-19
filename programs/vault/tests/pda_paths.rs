//! Instructions whose owner is a program PDA — reachable only when that program signs by CPI.
//! A generic `invoke_signed` proxy in the harness stands in for the member program.
//!
//! This covers the natural "game pays player" direction of `settle` (program -> wallet), plus
//! `grow_pda_ledger` and `authorize_pda_ledger`. The permission/ephemeral-CPI PDA instructions
//! (`open_pda_ledger`, `make_pda_ledger_private`, `delegate`) still can't complete locally.

mod common;

use anchor_lang::solana_program::pubkey::Pubkey;
use anchor_lang::InstructionData;
use common::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    signature::{Keypair, Signer},
};
use vault::state::{VaultError, SOL_MINT};

const HOUSE: &[u8] = b"house";

#[tokio::test]
async fn program_pays_a_wallet_through_settle() {
    let (prog, _) = proxy_pda(HOUSE);
    let wallet = Keypair::new();

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), PROXY_ID, 4, |l| {
        set_entry(l, 0, SOL_MINT, 1_000);
    });
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |l| {
        set_entry(l, 0, SOL_MINT, 0);
    });
    pt.add_account(pl, pa);
    pt.add_account(wl, wa);
    let mut ctx = pt.start_with_context().await;

    // src_authority is the program PDA (elevated by the proxy); dst already holds SOL at slot 0.
    let inner = Instruction {
        program_id: vault::ID,
        accounts: vec![
            AccountMeta::new(pl, false),
            AccountMeta::new(wl, false),
            AccountMeta::new_readonly(prog, true),
            AccountMeta::new_readonly(prog, false),
        ],
        data: vault::instruction::Settle { mint: SOL_MINT, amount: 400 }.data(),
    };
    send(&mut ctx, via_proxy(HOUSE, inner), &[]).await.unwrap();

    assert_eq!(read_ledger(&mut ctx, pl).await.entries[0].amount, 600);
    assert_eq!(read_ledger(&mut ctx, wl).await.entries[0].amount, 400);
}

#[tokio::test]
async fn grow_adds_headroom_to_a_program_ledger() {
    let (prog, _) = proxy_pda(HOUSE);
    let payer = Keypair::new();

    let mut pt = program_test();
    // capacity 2, so only one free slot — below the min_free we ask for.
    let (pl, pa) = make_ledger(&prog, true, &payer.pubkey(), PROXY_ID, 2, |_| {});
    pt.add_account(pl, pa);
    pt.add_account(payer.pubkey(), system_account(1_000_000_000));
    let mut ctx = pt.start_with_context().await;

    let inner = vault_ix(
        vault::accounts::GrowPdaLedger {
            owner: prog,
            payer: payer.pubkey(),
            ledger: pl,
            system_program: system_program_id(),
        },
        vault::instruction::GrowPdaLedger { min_free: 8, slot_increase: 8 },
        &[],
    );
    send(&mut ctx, via_proxy(HOUSE, inner), &[&payer]).await.unwrap();

    let l = read_ledger(&mut ctx, pl).await;
    assert_eq!(l.capacity(), 10);
    assert!(l.free_slots() >= 8);
}

#[tokio::test]
async fn only_the_rent_payer_may_grow() {
    let (prog, _) = proxy_pda(HOUSE);
    let rent_payer = Keypair::new();
    let stranger = Keypair::new();

    let mut pt = program_test();
    let (pl, pa) = make_ledger(&prog, true, &rent_payer.pubkey(), PROXY_ID, 2, |_| {});
    pt.add_account(stranger.pubkey(), system_account(1_000_000_000));
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    let inner = vault_ix(
        vault::accounts::GrowPdaLedger {
            owner: prog,
            payer: stranger.pubkey(),
            ledger: pl,
            system_program: system_program_id(),
        },
        vault::instruction::GrowPdaLedger { min_free: 8, slot_increase: 8 },
        &[],
    );
    let err = send(&mut ctx, via_proxy(HOUSE, inner), &[&stranger]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::NotRentPayer));
}

#[tokio::test]
async fn authorize_pda_backfills_the_member_program() {
    let (prog, bump) = proxy_pda(HOUSE);

    let mut pt = program_test();
    // authorized starts empty (an old ledger, before the field was written).
    let (pl, pa) = make_ledger(&prog, true, &Keypair::new().pubkey(), Pubkey::default(), 4, |_| {});
    pt.add_account(pl, pa);
    let mut ctx = pt.start_with_context().await;

    let inner = vault_ix(
        vault::accounts::AuthorizePdaLedger { owner: prog, ledger: pl },
        vault::instruction::AuthorizePdaLedger {
            member_program: PROXY_ID,
            owner_seeds: vec![HOUSE.to_vec(), vec![bump]],
        },
        &[],
    );
    send(&mut ctx, via_proxy(HOUSE, inner), &[]).await.unwrap();

    assert_eq!(read_ledger(&mut ctx, pl).await.authorized, PROXY_ID);
}

/// `grow_pda_ledger` is for PDAs only — a wallet (on-curve, signing directly) is refused.
#[tokio::test]
async fn grow_refuses_a_wallet() {
    let wallet = Keypair::new();

    let mut pt = program_test();
    let (wl, wa) = make_ledger(&wallet.pubkey(), false, &wallet.pubkey(), Pubkey::default(), 4, |_| {});
    pt.add_account(wallet.pubkey(), system_account(1_000_000_000));
    pt.add_account(wl, wa);
    let mut ctx = pt.start_with_context().await;

    let ix = vault_ix(
        vault::accounts::GrowPdaLedger {
            owner: wallet.pubkey(),
            payer: wallet.pubkey(),
            ledger: wl,
            system_program: system_program_id(),
        },
        vault::instruction::GrowPdaLedger { min_free: 8, slot_increase: 8 },
        &[],
    );
    let err = send(&mut ctx, ix, &[&wallet]).await.unwrap_err();
    assert_eq!(err_code(err), vault_err(VaultError::OwnerNotPda));
}
