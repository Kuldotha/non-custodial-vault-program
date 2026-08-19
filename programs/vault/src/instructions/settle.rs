use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::error::VaultError;
use crate::state::Ledger;

#[derive(BorshDeserialize)]
struct Args {
    mint: Pubkey,
    amount: u64,
}

/// Moves value between two ledgers — pure bookkeeping, the reserves are untouched.
/// Accounts: [src, dst, src_authority, dst_consenter]
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let Args { mint, amount } =
        Args::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [src, dst, src_authority, dst_consenter, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    let mut src_l = Ledger::load_checked(src, program_id)?;
    let mut dst_l = Ledger::load_checked(dst, program_id)?;
    // Aliasing src and dst would write the credit back last, minting balance from nothing.
    if src.key == dst.key {
        return Err(VaultError::DuplicateLedger.into());
    }

    if *src_authority.key != src_l.owner {
        return Err(VaultError::BadAuthority.into());
    }

    // At most one human, so Alice-pays-Bob is unrepresentable.
    if !(src_l.pda_auth || dst_l.pda_auth) {
        return Err(VaultError::NotProgramMediated.into());
    }

    // The debited side authorises: a human by signing, a program by invoke_signed over its seeds.
    if !src_authority.is_signer {
        return Err(if src_l.pda_auth {
            VaultError::MissingProgramSignature
        } else {
            VaultError::MissingUserSignature
        }
        .into());
    }

    let may_claim = if dst_l.pda_auth || dst_l.index_of(&mint).is_some() {
        true
    } else {
        if !dst_consenter.is_signer {
            return Err(VaultError::MissingUserSignature.into());
        }
        let allowed = *dst_consenter.key == dst_l.owner
            || (dst_l.authorized != Pubkey::default() && dst_l.authorized == *dst_consenter.key);
        if !allowed {
            return Err(VaultError::NotAuthorizedToConsent.into());
        }
        true
    };

    let si = src_l.index_of(&mint).ok_or(VaultError::NoBalance)?;
    src_l.debit(si, amount)?;
    let di = dst_l.index_for_credit(&mint, may_claim)?;
    dst_l.credit(di, amount)?;

    src_l.store(src)?;
    dst_l.store(dst)?;
    Ok(())
}
