use solana_program::{
    account_info::AccountInfo,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::{rent::Rent, Sysvar},
};

use crate::constants::{is_token_program, ASSOCIATED_TOKEN_PROGRAM_ID};
use crate::error::VaultError;

/// The vault's own rent-exempt minimum. Not part of any ledger's balance, so every SOL path
/// subtracts it before deciding what is spendable.
pub fn vault_floor() -> Result<u64, ProgramError> {
    Ok(Rent::get()?.minimum_balance(0))
}

/// The associated token address for `(wallet, mint)` under the SPL token program. Derived here
/// rather than via `spl-associated-token-account` to avoid dragging in the Token-2022 tree.
pub fn associated_token_address(wallet: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[wallet.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

/// Reads `(mint, owner, amount)` from a raw SPL token account. By hand rather than a typed account
/// because these slots carry a placeholder on the SOL path.
pub fn token_fields(info: &AccountInfo) -> Result<(Pubkey, Pubkey, u64), ProgramError> {
    if !is_token_program(info.owner) {
        return Err(VaultError::MintMismatch.into());
    }
    let data = info.try_borrow_data()?;
    if data.len() < 165 {
        return Err(VaultError::MintMismatch.into());
    }
    let mint = Pubkey::new_from_array(data[0..32].try_into().unwrap());
    let owner = Pubkey::new_from_array(data[32..64].try_into().unwrap());
    let amount = u64::from_le_bytes(data[64..72].try_into().unwrap());
    Ok((mint, owner, amount))
}

/// Asserts `info` is the vault's canonical ATA for `mint` and returns its balance. The token
/// account's own `owner` field is what gives authority; the derivation is asserted so a mint has
/// exactly one pool.
pub fn require_reserve(
    info: &AccountInfo, vault: &Pubkey, mint: &Pubkey, token_program: &Pubkey,
) -> Result<u64, ProgramError> {
    if *info.key != associated_token_address(vault, mint, token_program) {
        return Err(VaultError::NotCanonicalReserve.into());
    }
    let (m, o, amount) = token_fields(info)?;
    if m != *mint || o != *vault {
        return Err(VaultError::MintMismatch.into());
    }
    Ok(amount)
}
