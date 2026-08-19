use solana_program::{
    account_info::AccountInfo,
    clock::Clock,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::Sysvar,
};

use crate::error::VaultError;
use crate::state::{receipt, Ledger};

/// Settles a receipt and calls back into the program that authorised it, in the same instruction.
/// The receipt is zeroed and handed over before the callback runs. Permissionless: the debits were
/// approved at creation.
/// Accounts: [receipt, authority, callback_program, vault_authority] + ledgers + forwarded
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [receipt_ai, authority, callback_program, vault_authority, remaining @ ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    // Must be owned by this program — proves the vault wrote the terms — and carry the receipt
    // discriminator (checked in `receipt::read`).
    if receipt_ai.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    let r = receipt::read(&receipt_ai.try_borrow_data()?)?;

    // Same slot, or nothing.
    if r.slot != Clock::get()?.slot {
        return Err(VaultError::ReceiptExpired.into());
    }
    if *authority.key != r.authority {
        return Err(VaultError::BadAuthority.into());
    }
    if *callback_program.key != r.member {
        return Err(VaultError::CallbackProgramMismatch.into());
    }

    let ledger_count = r.owners.len();
    if remaining.len() < ledger_count {
        return Err(VaultError::NoAuthorization.into());
    }
    let (ledger_infos, forwarded) = remaining.split_at(ledger_count);

    // Off-enum codes: 7000 + check*100 + index. Transaction logs are off in a private rollup, so
    // the error code is the only channel wide enough to say which ledger failed which check.
    let code = |c: u32, idx: usize| -> ProgramError { ProgramError::Custom(7000 + c * 100 + idx as u32) };

    let mut ledgers = Vec::with_capacity(ledger_count);
    for (idx, (info, owner)) in ledger_infos.iter().zip(r.owners.iter()).enumerate() {
        if info.owner != program_id {
            return Err(code(0, idx));
        }
        let ledger = Ledger::read_from(&info.try_borrow_data()?)?;
        if ledger.owner != *owner {
            return Err(code(1, idx));
        }
        let derived =
            Pubkey::create_program_address(&[b"ledger", owner.as_ref(), &[ledger.bump]], program_id)
                .map_err(|_| code(2, idx))?;
        if derived != *info.key {
            return Err(code(2, idx));
        }
        if ledger.pda_auth && ledger.authorized != r.member {
            return Err(code(3, idx));
        }
        if ledger_infos[..idx].iter().any(|p| p.key == info.key) {
            return Err(code(4, idx));
        }
        ledgers.push(ledger);
    }

    // Value can never move between two people, whatever the shape of the receipt.
    if ledgers.iter().filter(|l| !l.pda_auth).count() > 1 {
        return Err(VaultError::NotProgramMediated.into());
    }

    // No consent here: the debits were approved by their owners at creation.
    for m in &r.movements {
        let (f, t) = (m.from as usize, m.to as usize);
        let i = ledgers[f].index_of(&m.mint).ok_or(VaultError::NoBalance)?;
        ledgers[f].debit(i, m.amount)?;
        let j = ledgers[t].index_or_claim(&m.mint)?;
        ledgers[t].credit(j, m.amount)?;
    }
    for (info, ledger) in ledger_infos.iter().zip(ledgers.iter()) {
        ledger.store(info)?;
    }

    // Consumed before the callback, not after: a callback that never sees a live receipt cannot
    // replay one. Zeroing also wipes the discriminator, and the runtime's zero-on-reassign rule
    // then makes the handoff below the only way ownership can move.
    receipt_ai.try_borrow_mut_data()?.fill(0);
    receipt_ai.assign(&r.member);

    let (va_pda, va_bump) = Pubkey::find_program_address(&[], program_id);
    if *vault_authority.key != va_pda {
        return Err(ProgramError::InvalidSeeds);
    }

    let mut metas = vec![
        AccountMeta::new(*receipt_ai.key, false),
        AccountMeta::new_readonly(*vault_authority.key, true),
    ];
    let mut infos = vec![receipt_ai.clone(), vault_authority.clone(), callback_program.clone()];
    for a in forwarded {
        // A callback signs its own work with its own seeds; it inherits nothing.
        metas.push(AccountMeta { pubkey: *a.key, is_signer: false, is_writable: a.is_writable });
        infos.push(a.clone());
    }
    // Owner 0 travels in the data because the receipt has been zeroed by now.
    let mut data = Vec::with_capacity(8 + 32 + r.args.len());
    data.extend_from_slice(&r.callback_disc);
    data.extend_from_slice(r.owners[0].as_ref());
    data.extend_from_slice(&r.args);

    invoke_signed(
        &Instruction { program_id: r.member, accounts: metas, data },
        &infos,
        &[&[&[va_bump]]],
    )
}
