use solana_program::{
    account_info::AccountInfo,
    program::{invoke, invoke_signed},
    program_error::ProgramError,
    sysvar::{rent::Rent, Sysvar},
};
use solana_system_interface::instruction as system_instruction;

use crate::constants::{DEFAULT_SLOTS, MAX_SLOT_INCREASE};
use crate::error::VaultError;
use crate::state::{Entry, Ledger, Session};
use crate::utils::pda::is_pda;

/// Creates `["ledger", owner]` — a program-owned account sized for `slots`. The rent may come from
/// somebody other than the owner (a PDA cannot pay its own), but a wallet must fund its own.
///
/// transfer-then-allocate-then-assign, not `create_account`: the latter aborts on a pre-funded
/// account, so anyone could block a ledger by sending it a lamport first.
pub fn create_ledger_account_sized<'a>(
    info: &AccountInfo<'a>,
    payer: &AccountInfo<'a>,
    owner: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    bump: u8,
    slots: usize,
) -> Result<Ledger, ProgramError> {
    let rent_payer = if is_pda(owner.key) {
        *payer.key
    } else {
        if payer.key != owner.key {
            return Err(VaultError::MustFundOwnLedger.into());
        }
        *owner.key
    };

    let space = Ledger::space(slots);
    let rent = Rent::get()?.minimum_balance(space);
    let owner_key = *owner.key;
    let bump_arr = [bump];
    let signer_seeds: &[&[u8]] = &[b"ledger", owner_key.as_ref(), &bump_arr];

    let have = info.lamports();
    if have < rent {
        invoke(
            &system_instruction::transfer(payer.key, info.key, rent - have),
            &[payer.clone(), info.clone(), system_program.clone()],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(info.key, space as u64),
        &[info.clone(), system_program.clone()],
        &[signer_seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(info.key, &crate::ID),
        &[info.clone(), system_program.clone()],
        &[signer_seeds],
    )?;

    Ok(Ledger::new(*owner.key, is_pda(owner.key), bump, rent_payer, slots))
}

/// Creates `["session", owner]` beside a wallet's ledger, empty, owner-funded, the same way and
/// for the same reason: a pre-funded address must not be able to block it. It grows by one
/// program entry per game the owner authorises.
pub fn create_session_account<'a>(
    info: &AccountInfo<'a>,
    owner: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    bump: u8,
) -> Result<Session, ProgramError> {
    let space = Session::space(0);
    let rent = Rent::get()?.minimum_balance(space);
    let owner_key = *owner.key;
    let bump_arr = [bump];
    let signer_seeds: &[&[u8]] = &[b"session", owner_key.as_ref(), &bump_arr];

    let have = info.lamports();
    if have < rent {
        invoke(
            &system_instruction::transfer(owner.key, info.key, rent - have),
            &[owner.clone(), info.clone(), system_program.clone()],
        )?;
    }
    invoke_signed(
        &system_instruction::allocate(info.key, space as u64),
        &[info.clone(), system_program.clone()],
        &[signer_seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(info.key, &crate::ID),
        &[info.clone(), system_program.clone()],
        &[signer_seeds],
    )?;
    let s = Session::new(owner_key, bump);
    s.write_to(&mut info.try_borrow_mut_data()?)?;
    Ok(s)
}

/// The wallet path — owner funds its own ledger at the default size.
pub fn create_ledger_account<'a>(
    info: &AccountInfo<'a>,
    owner: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    bump: u8,
) -> Result<Ledger, ProgramError> {
    create_ledger_account_sized(info, owner, owner, system_program, bump, DEFAULT_SLOTS as usize)
}

/// Adds `slot_increase` slots when fewer than `min_free` are free, funding the rent from `payer`
/// (which must be the recorded rent payer). basenet only — a delegated account cannot be resized.
pub fn ensure_headroom<'a>(
    info: &AccountInfo<'a>,
    ledger: &mut Ledger,
    payer: &AccountInfo<'a>,
    system_program: &AccountInfo<'a>,
    min_free: u16,
    slot_increase: u16,
) -> Result<(), ProgramError> {
    if *payer.key != ledger.rent_payer {
        return Err(VaultError::NotRentPayer.into());
    }
    if ledger.free_slots() >= min_free as usize {
        return Ok(());
    }
    if !(slot_increase > 0 && slot_increase <= MAX_SLOT_INCREASE) {
        return Err(VaultError::BadSlotCount.into());
    }

    for _ in 0..slot_increase {
        ledger.entries.push(Entry::default());
    }
    let new_space = Ledger::space(ledger.capacity());
    let needed = Rent::get()?.minimum_balance(new_space);
    let have = info.lamports();
    if needed > have {
        invoke(
            &system_instruction::transfer(payer.key, info.key, needed - have),
            &[payer.clone(), info.clone(), system_program.clone()],
        )?;
    }
    info.resize(new_space)?;
    Ok(())
}
