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

use ephemeral_rollups_sdk::consts::EPHEMERAL_VAULT_ID;
use ephemeral_rollups_sdk::ephemeral_accounts::EphemeralAccount;

use crate::error::VaultError;
use crate::state::{receipt, Ledger};
use crate::utils::crank;

/// Settles a receipt and calls back into the program that authorised it, in the same instruction.
/// Runs top-level as the vault, which owns every ledger, so it may read the human's ledger past the
/// rollup ACL — the one place consent can be checked, which is why consent lives here and not at
/// creation.
pub fn handler(program_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    let [receipt_ai, authority, consenter, callback_program, vault_authority, ephemeral_vault, _magic_program, magic_context, remaining @ ..] =
        accounts
    else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    if receipt_ai.owner != program_id {
        return Err(ProgramError::IllegalOwner);
    }

    let r = receipt::read(&receipt_ai.try_borrow_data()?)?;

    if r.slot != Clock::get()?.slot {
        return Err(VaultError::ReceiptExpired.into());
    }
    if *authority.key != r.authority {
        return Err(VaultError::BadAuthority.into());
    }
    if *callback_program.key != r.member {
        return Err(VaultError::CallbackProgramMismatch.into());
    }
    if *consenter.key != r.consenter {
        return Err(VaultError::BadAuthority.into());
    }
    // Settlement is not permissionless: the receipt's consenter — the session key that seeded and
    // signed it at creation — must sign here too. It is the only author that can (the authority is
    // an off-curve member PDA), and same-slot means it is already a signer of this transaction.
    if !consenter.is_signer {
        return Err(VaultError::NotAuthorizedToConsent.into());
    }

    let ledger_count = r.owners.len();
    if remaining.len() < ledger_count {
        return Err(VaultError::NoAuthorization.into());
    }
    let (ledger_infos, forwarded) = remaining.split_at(ledger_count);

    // Off-enum codes: 7000 + check*100 + index.
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

    // The one human ledger must have its owner or session key sign whenever it is debited or gets a
    // new token slot. Program ledgers are already covered by `authorized == member` above.
    if let Some(hi) = ledgers.iter().position(|l| !l.pda_auth) {
        let debited = r.movements.iter().any(|m| m.from as usize == hi);
        let new_slot = r
            .movements
            .iter()
            .any(|m| m.to as usize == hi && ledgers[hi].index_of(&m.mint).is_none());
        if debited || new_slot {
            // The consenter already signed (checked above); here it must be this ledger's party.
            let ok = *consenter.key == ledgers[hi].owner || *consenter.key == ledgers[hi].authorized;
            if !ok {
                return Err(VaultError::NotAuthorizedToConsent.into());
            }
        }
    }

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

    // Kill the discriminator so the receipt cannot be re-settled during the callback; the vault
    // keeps ownership to close it off the sponsor ledger once the callback returns.
    receipt_ai.try_borrow_mut_data()?[..8].fill(0);

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
        metas.push(AccountMeta { pubkey: *a.key, is_signer: false, is_writable: a.is_writable });
        infos.push(a.clone());
    }
    let mut data = Vec::with_capacity(8 + 32 + r.args.len());
    data.extend_from_slice(&r.callback_disc);
    data.extend_from_slice(r.owners[0].as_ref());
    data.extend_from_slice(&r.args);
    invoke_signed(
        &Instruction { program_id: r.member, accounts: metas, data },
        &infos,
        &[&[&[va_bump]]],
    )?;

    if *ephemeral_vault.key != EPHEMERAL_VAULT_ID {
        return Err(ProgramError::InvalidArgument);
    }
    let si = r.owners.iter().position(|o| *o == r.authority).ok_or(VaultError::BadAuthority)?;
    let ledger_bump = [ledgers[si].bump];
    let ledger_seeds: [&[u8]; 3] = [b"ledger", r.authority.as_ref(), &ledger_bump];
    EphemeralAccount::new(&ledger_infos[si], receipt_ai, ephemeral_vault)
        .with_signer_seeds(&[&ledger_seeds])
        .close()?;

    crank::cancel_reap(&ledger_infos[si], &r.authority, ledgers[si].bump, magic_context, receipt_ai.key)
}
