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
/// Most slots a single open instruction may allocate up front (rent paid at open). Not a ceiling
/// on a ledger's capacity — `grow_pda_ledger` can extend it further, one realloc at a time.
pub const MAX_GROWTH: u16 = 256;

pub const ENTRY_SIZE: usize = 32 + 8;

/// Anchor account discriminator for `Ledger` — `sha256("account:Ledger")[..8]`, so a ledger
/// reads as an Anchor account to any Anchor-style client and to member programs that index
/// into the raw bytes.
pub const LEDGER_DISCRIMINATOR: [u8; 8] = [43, 41, 21, 213, 180, 176, 95, 32];
