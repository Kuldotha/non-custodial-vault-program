//! Shared mollusk-svm harness for the native vault's tests.
//!
//! Mollusk runs the compiled `target/deploy/vault.so` in its SVM, so tests exercise the real
//! program. Ledger accounts are fabricated with the byte-exact layout (`Ledger::write_to`), which
//! lets a test reach states a live flow would otherwise gate. The permission/ephemeral/magic CPIs
//! remain out of reach — as they were under the old harness — so `settle` (pure bookkeeping) and
//! the raw `settle_receipt` movement/consent logic are what run here end to end.

#![allow(dead_code)]

use mollusk_svm::Mollusk;
use solana_account::Account;
use solana_program::{pubkey::Pubkey, rent::Rent};

use vault::state::{Entry, Ledger};

// Anchor `sha256("global:<name>")[..8]` — the wire discriminators approach A preserves.
pub const SETTLE_DISC: [u8; 8] = [175, 42, 185, 87, 144, 131, 102, 212];

pub fn mollusk() -> Mollusk {
    Mollusk::new(&vault::ID, "vault")
}

pub fn ledger_pda(owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"ledger", owner.as_ref()], &vault::ID)
}

/// A program-owned ledger at its canonical PDA, with whatever balances `set` writes. `pda_auth`
/// is stored, not derived — settle reads it from the account — so a test picks wallet vs program.
pub fn make_ledger(
    owner: &Pubkey,
    pda_auth: bool,
    rent_payer: &Pubkey,
    authorized: Pubkey,
    slots: usize,
    set: impl FnOnce(&mut Ledger),
) -> (Pubkey, Account) {
    let (pda, bump) = ledger_pda(owner);
    let mut l = Ledger::new(*owner, pda_auth, bump, *rent_payer, slots);
    l.authorized = authorized;
    set(&mut l);

    let mut data = vec![0u8; Ledger::space(slots)];
    l.write_to(&mut data).unwrap();
    let account = Account {
        lamports: Rent::default().minimum_balance(data.len()),
        data,
        owner: vault::ID,
        executable: false,
        rent_epoch: 0,
    };
    (pda, account)
}

pub fn read_ledger(account: &Account) -> Ledger {
    Ledger::read_from(&account.data).unwrap()
}

/// A funded System-owned account — a signer stand-in or a placeholder.
pub fn system_account() -> Account {
    Account {
        lamports: 1_000_000_000,
        data: vec![],
        owner: Pubkey::default(), // the System Program id is the all-zeros key
        executable: false,
        rent_epoch: 0,
    }
}

pub fn set_entry(l: &mut Ledger, index: usize, mint: Pubkey, amount: u64) {
    l.entries[index] = Entry { mint, amount };
}

pub fn token_mint(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}
