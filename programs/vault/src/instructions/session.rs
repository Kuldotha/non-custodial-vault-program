use borsh::BorshDeserialize;
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, program::invoke,
    program_error::ProgramError, pubkey::Pubkey, rent::Rent, sysvar::Sysvar,
};
use solana_system_interface::instruction as system_instruction;

use crate::error::VaultError;
use crate::state::Session;
use crate::utils::account::create_session_account;
use crate::utils::pda::{self, is_pda};

#[derive(BorshDeserialize)]
struct AuthorizeArgs {
    program: Pubkey,
    key: Pubkey,
    /// Zero grants a persisted key, into the program's ring; anything else a temporary one,
    /// dead at that unix second.
    expires_at: i64,
}

/// Lets `key` consent for `program` on the owner's ledger. basenet only; the owner signs. The
/// first grant for a program adds its entry to the store, the owner paying the rent — and a
/// wallet whose ledger predates the store gets the store itself here, so re-authorising is
/// this one signature for everyone.
/// Accounts: [session, owner, system_program]
pub fn authorize_handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let AuthorizeArgs { program, key, expires_at } =
        AuthorizeArgs::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [session_ai, owner, system_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if key == Pubkey::default() || is_pda(&key) {
        return Err(VaultError::BadAuthorizedKey.into());
    }
    if is_pda(owner.key) {
        return Err(VaultError::OwnerNotWallet.into());
    }
    let mut s = if session_ai.data_is_empty() {
        let bump = pda::validate(program_id, session_ai, &[b"session", owner.key.as_ref()])?;
        create_session_account(session_ai, owner, system_program, bump)?
    } else {
        load(program_id, session_ai, owner.key)?
    };
    let (entry, added) = s.entry_mut(program);
    if expires_at == 0 {
        entry.grant(key);
    } else {
        if expires_at <= Clock::get()?.unix_timestamp {
            return Err(VaultError::BadExpiry.into());
        }
        entry.grant_temporary(key, expires_at);
    }
    if added {
        let space = Session::space(s.entries.len());
        let needed = Rent::get()?.minimum_balance(space);
        let have = session_ai.lamports();
        if needed > have {
            invoke(
                &system_instruction::transfer(owner.key, session_ai.key, needed - have),
                &[owner.clone(), session_ai.clone(), system_program.clone()],
            )?;
        }
        session_ai.resize(space)?;
    }
    s.write_to(&mut session_ai.try_borrow_mut_data()?)
}

#[derive(BorshDeserialize)]
struct RevokeArgs {
    program: Pubkey,
    /// The key to forget within the program's entry; the zero key drops the entry whole, the
    /// program's access with it, and refunds its rent.
    key: Pubkey,
}

/// basenet only; the owner signs.
/// Accounts: [session, owner]
pub fn revoke_handler(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let RevokeArgs { program, key } = RevokeArgs::try_from_slice(data).map_err(|_| ProgramError::InvalidInstructionData)?;
    let [session_ai, owner, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if !owner.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    let mut s = load(program_id, session_ai, owner.key)?;
    if key == Pubkey::default() {
        if s.drop_entry(&program) {
            let space = Session::space(s.entries.len());
            session_ai.try_borrow_mut_data()?[..].fill(0);
            session_ai.resize(space)?;
            let keep = Rent::get()?.minimum_balance(space);
            let refund = session_ai.lamports().saturating_sub(keep);
            **session_ai.try_borrow_mut_lamports()? -= refund;
            **owner.try_borrow_mut_lamports()? += refund;
        }
    } else if let Some(i) = s.entries.iter().position(|e| e.program == program) {
        s.entries[i].revoke(&key);
    }
    s.write_to(&mut session_ai.try_borrow_mut_data()?)
}

fn load(program_id: &Pubkey, session_ai: &AccountInfo, owner: &Pubkey) -> Result<Session, ProgramError> {
    pda::validate(program_id, session_ai, &[b"session", owner.as_ref()])?;
    if session_ai.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    let s = Session::read_from(&session_ai.try_borrow_data()?)?;
    if s.owner != *owner {
        return Err(VaultError::BadLedgerOwner.into());
    }
    Ok(s)
}

/// The keys `owner` has granted, read off the store passed for them — or none, for a wallet
/// whose ledger predates the store. The account must be the owner's own store, or empty.
pub fn keys_of(program_id: &Pubkey, session_ai: &AccountInfo, owner: &Pubkey) -> Result<Option<Session>, ProgramError> {
    pda::validate(program_id, session_ai, &[b"session", owner.as_ref()])?;
    if session_ai.data_is_empty() {
        return Ok(None);
    }
    if session_ai.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }
    Ok(Some(Session::read_from(&session_ai.try_borrow_data()?)?))
}

/// Whether `consenter` may consent for `program` on `owner`'s ledger: the owner themselves, or
/// a key in their store granted for that program.
pub fn may_consent(
    program_id: &Pubkey,
    session_ai: &AccountInfo,
    owner: &Pubkey,
    consenter: &Pubkey,
    program: &Pubkey,
) -> Result<bool, ProgramError> {
    if consenter == owner {
        return Ok(true);
    }
    let Some(s) = keys_of(program_id, session_ai, owner)? else {
        return Ok(false);
    };
    Ok(s.allows(consenter, program, Clock::get()?.unix_timestamp))
}
