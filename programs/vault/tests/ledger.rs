//! Unit tests for the ledger's own logic — slot discipline, the curve discriminator,
//! and the arithmetic that `settle` depends on. These need no runtime.

use anchor_lang::prelude::Pubkey;
use vault::state::*;

fn mint(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

fn fresh() -> Ledger {
    let mut l = Ledger {
        owner: Pubkey::new_unique(),
        pda_auth: false,
        bump: 0,
        _pad: [0; 6],
        entries: vec![],
    };
    l.init(Pubkey::new_unique(), false, 255, DEFAULT_SLOTS as usize);
    l
}

#[test]
fn slot_zero_is_always_sol() {
    let l = fresh();
    assert_eq!(l.entries[0].mint, SOL_MINT);
    assert_eq!(l.index_of(&SOL_MINT), Some(0));
    assert_eq!(l.capacity(), DEFAULT_SLOTS as usize);
}

#[test]
fn sol_lookup_is_positional_not_a_scan() {
    let mut l = fresh();
    // a freshly claimed slot must never be mistaken for SOL, and vice versa
    let i = l.index_or_claim(&mint(7)).unwrap();
    assert_ne!(i, 0);
    assert_eq!(l.index_of(&SOL_MINT), Some(0));
    assert_eq!(l.index_of(&mint(7)), Some(i));
}

#[test]
fn free_slots_excludes_slot_zero() {
    let l = fresh();
    // 32 slots, slot 0 taken by SOL
    assert_eq!(l.free_slots(), DEFAULT_SLOTS as usize - 1);
}

#[test]
fn claiming_is_idempotent() {
    let mut l = fresh();
    let a = l.index_or_claim(&mint(3)).unwrap();
    let b = l.index_or_claim(&mint(3)).unwrap();
    assert_eq!(a, b);
    assert_eq!(l.free_slots(), DEFAULT_SLOTS as usize - 2);
}

#[test]
fn ledger_fills_and_then_refuses() {
    let mut l = fresh();
    for i in 1..DEFAULT_SLOTS as usize {
        l.index_or_claim(&mint(i as u8)).unwrap();
    }
    assert_eq!(l.free_slots(), 0);
    // settle relies on this failing rather than silently growing
    assert!(l.index_or_claim(&mint(200)).is_err());
}

/// The discriminator the whole security model rests on: a human's key is on-curve and a
/// program's PDA is not, so the two populations can never overlap.
#[test]
fn curve_discriminates_humans_from_pdas() {
    use std::str::FromStr;
    use vault::state::is_pda;

    // a real wallet — someone holds the private key, so it is on-curve
    let wallet = Pubkey::from_str("691aFvKMnHXrMSgqk6G8izoCbVZTmkrRcu8xCeMKfPh1").unwrap();
    assert!(!is_pda(&wallet), "a wallet must be on-curve");

    // a PDA is off-curve by construction, and no private key for it can exist
    let (pda, _) = Pubkey::find_program_address(&[b"vault_authority"], &vault::ID);
    assert!(is_pda(&pda), "a PDA must be off-curve");

    // the vault's own reserve PDA, likewise
    let (vault_pda, _) = Pubkey::find_program_address(&[b"vault"], &vault::ID);
    assert!(is_pda(&vault_pda));
}

#[test]
fn space_matches_the_layout() {
    // header + 40 bytes per slot; the rent numbers in the spec depend on this
    assert_eq!(Ledger::space(32), Ledger::HEADER + 32 * 40);
    assert_eq!(ENTRY_SIZE, 40);
}
