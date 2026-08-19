use solana_program::{pubkey::Pubkey, pubkey};

/// The SPL Token program.
pub const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

/// The SPL Associated Token Account program — reserves are the vault's ATA per mint.
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

/// The SOL entry's mint key: the System Program's zero key. Slot 0 of every ledger is SOL from
/// creation, so a zero mint at index 0 means SOL and a zero mint anywhere else means a free slot.
pub const SOL_MINT: Pubkey = Pubkey::new_from_array([0u8; 32]);

pub const DEFAULT_SLOTS: u16 = 32;
pub const DEFAULT_MIN_FREE: u16 = 16;
/// Solana caps a single realloc at 10 KiB, which is 256 entries.
pub const MAX_SLOT_INCREASE: u16 = 256;
/// Largest capacity a ledger may be opened with (rent is paid up front).
pub const MAX_SLOTS: u16 = 256;

pub const ENTRY_SIZE: usize = 32 + 8;

/// Anchor account discriminator for `Ledger` — `sha256("account:Ledger")[..8]`. Kept byte-exact
/// so native-written ledgers stay readable by the deployed Anchor state and by member programs
/// that index into the raw bytes (approach A, wire-compatible).
pub const LEDGER_DISCRIMINATOR: [u8; 8] = [43, 41, 21, 213, 180, 176, 95, 32];
