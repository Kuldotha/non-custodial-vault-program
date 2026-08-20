use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};

use crate::error::VaultError;

/// A human can never sign for an off-curve address and `find_program_address` never returns an
/// on-curve one, so the curve of the key is a sound wallet-vs-PDA discriminator. Uses the
/// `sol_curve_validate_point` syscall — `solana_pubkey`'s host check is `unimplemented!()` on SBF.
pub fn is_pda(key: &Pubkey) -> bool {
    let point = solana_curve25519::edwards::PodEdwardsPoint(key.to_bytes());
    !solana_curve25519::edwards::validate_edwards(&point)
}

/// Confirms `account` is the PDA of `seeds` under `program_id`, returning the bump.
pub fn validate(
    program_id: &Pubkey,
    account: &AccountInfo,
    seeds: &[&[u8]],
) -> Result<u8, ProgramError> {
    let (pda, bump) = Pubkey::find_program_address(seeds, program_id);
    if &pda != account.key {
        return Err(ProgramError::InvalidSeeds);
    }
    Ok(bump)
}

/// Proves `member_program` is the program behind `owner`: its seeds must derive the owner's
/// address under that program. Address spaces cannot collide across programs, so the owner's
/// signature — which callers require — can only have come from it. Do not replace with the
/// account's `owner` field: delegation rewrites it.
pub fn verify_pda_owner(
    owner: &Pubkey,
    member_program: &Pubkey,
    owner_seeds: &[Vec<u8>],
) -> Result<(), ProgramError> {
    let seeds: Vec<&[u8]> = owner_seeds.iter().map(|s| s.as_slice()).collect();
    let derived = Pubkey::create_program_address(&seeds, member_program)
        .map_err(|_| ProgramError::from(VaultError::MemberProgramMismatch))?;
    if derived != *owner {
        return Err(VaultError::MemberProgramMismatch.into());
    }
    Ok(())
}

/// Closes a program-owned account: sweep its lamports to `receiver` and zero its data.
pub fn close(receiver: &AccountInfo, account: &AccountInfo) -> Result<(), ProgramError> {
    **receiver.try_borrow_mut_lamports()? += account.lamports();
    **account.try_borrow_mut_lamports()? = 0;
    account.try_borrow_mut_data()?.fill(0);
    Ok(())
}
