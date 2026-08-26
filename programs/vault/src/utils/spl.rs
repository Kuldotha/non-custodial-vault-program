use solana_program::{
    pubkey::Pubkey,
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::{invoke, invoke_signed},
};

use solana_program::program_error::ProgramError;

use crate::constants::TOKEN_2022_PROGRAM_ID;
use crate::error::VaultError;

/// An SPL token `Transfer` (instruction tag 3, then the amount little-endian), built by hand so
/// the vault needs no `spl-token` dependency — it pulls a conflicting `solana-program` and the
/// vault already reads token accounts as raw bytes. `signer_seeds` present when the authority is
/// one of our PDAs (the vault), absent when a wallet signs for itself. The program id comes off
/// the passed account, so the same call serves classic Token and Token-2022.
pub fn transfer<'a>(
    token_program: &AccountInfo<'a>,
    from: &AccountInfo<'a>,
    to: &AccountInfo<'a>,
    authority: &AccountInfo<'a>,
    amount: u64,
    signer_seeds: Option<&[&[u8]]>,
) -> ProgramResult {
    let mut data = Vec::with_capacity(9);
    data.push(3u8);
    data.extend_from_slice(&amount.to_le_bytes());

    let ix = Instruction {
        program_id: *token_program.key,
        accounts: vec![
            AccountMeta::new(*from.key, false),
            AccountMeta::new(*to.key, false),
            AccountMeta::new_readonly(*authority.key, true),
        ],
        data,
    };

    let infos = [from.clone(), to.clone(), authority.clone(), token_program.clone()];
    match signer_seeds {
        Some(seeds) => invoke_signed(&ix, &infos, &[seeds]),
        None => invoke(&ix, &infos),
    }
}

/// `TransferChecked` (tag 12: amount, then decimals) — the form Token-2022 requires for mints
/// carrying extensions. The mint rides along and its decimals are read off its own bytes, so
/// no caller has to know them. Works identically on the classic token program.
pub fn transfer_checked<'a>(
    token_program: &AccountInfo<'a>,
    from: &AccountInfo<'a>,
    mint: &AccountInfo<'a>,
    to: &AccountInfo<'a>,
    authority: &AccountInfo<'a>,
    amount: u64,
    signer_seeds: Option<&[&[u8]]>,
) -> ProgramResult {
    let decimals = mint_decimals(mint)?;
    let mut data = Vec::with_capacity(10);
    data.push(12u8);
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(decimals);

    let ix = Instruction {
        program_id: *token_program.key,
        accounts: vec![
            AccountMeta::new(*from.key, false),
            AccountMeta::new_readonly(*mint.key, false),
            AccountMeta::new(*to.key, false),
            AccountMeta::new_readonly(*authority.key, true),
        ],
        data,
    };

    let infos = [from.clone(), mint.clone(), to.clone(), authority.clone(), token_program.clone()];
    match signer_seeds {
        Some(seeds) => invoke_signed(&ix, &infos, &[seeds]),
        None => invoke(&ix, &infos),
    }
}

/// Byte 44 of any mint, either token program — the layout is shared up to the base fields.
fn mint_decimals(mint: &AccountInfo) -> Result<u8, ProgramError> {
    let data = mint.try_borrow_data()?;
    if data.len() < 45 {
        return Err(VaultError::MintMismatch.into());
    }
    Ok(data[44])
}

/// The optional mint account a token-program-aware instruction may append. Token-2022 REQUIRES
/// it (its transfers are checked, and checked transfers carry the mint); the classic program
/// may omit it and take the unchecked path, which is what every pre-2022 client sends.
pub fn trailing_mint<'a, 'b>(
    accounts: &'b [AccountInfo<'a>],
    index: usize,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Result<Option<&'b AccountInfo<'a>>, ProgramError> {
    match accounts.get(index) {
        Some(ai) if ai.key == mint && ai.owner == token_program => Ok(Some(ai)),
        _ if *token_program == TOKEN_2022_PROGRAM_ID => Err(VaultError::MintMismatch.into()),
        _ => Ok(None),
    }
}
