use anchor_lang::prelude::*;

use crate::state::*;

/// Grants — or revokes — a session key's right to consent to debits of this ledger, locked
/// to a single program authority. basenet only: the account may grow here, and a delegated
/// account cannot realloc (the owner check inside `load_ledger` enforces this for free — a
/// delegated ledger belongs to the delegation program).
///
/// Wallet ledgers only. A session key on a program's ledger would be a signature that
/// consents on a program's behalf, which is exactly the capability the vault must never
/// hand out: the program side of a movement consents by `invoke_signed` over its own
/// seeds, and by nothing else.
///
/// The authority lock is what bounds a stolen session key. Unscoped, a leaked key could
/// consent to a debit toward any program — including one deployed just to receive it and
/// withdraw. Scoped, the worst it can do is spend the ledger through the one program the
/// owner chose, whose payouts come back to the same ledger.
#[derive(Accounts)]
pub struct AssignLedgerAuthorization<'info> {
    /// CHECK: raw because the account may resize here — `Account<Ledger>` cannot express
    /// "grow me", for the same reason `deposit` handles the ledger raw.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    /// Also funds the trailer's rent on first assignment; a wallet ledger's rent payer is
    /// always its owner, so this stays consistent with the growth rule.
    #[account(mut)]
    pub owner: Signer<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<AssignLedgerAuthorization>,
    authorized: Pubkey,
    authority: Pubkey,
) -> Result<()> {
    let info = ctx.accounts.ledger.to_account_info();
    let mut ledger = load_ledger(&info)?;
    require!(!ledger.pda_auth, VaultError::CannotAuthorizePdaLedger);

    let clearing = authorized == Pubkey::default();
    // An off-curve authorized key could only ever sign via some program's `invoke_signed`,
    // which would make this a program-consent grant wearing a session key's clothes.
    require!(clearing || !is_pda(&authorized), VaultError::BadAuthorizedKey);
    require!(clearing || authority != Pubkey::default(), VaultError::BadAuthorizedKey);

    if ledger._pad[0] != LEDGER_V_AUTHORIZED {
        // Revoking on a ledger that never had a grant: nothing to clear, and migrating it
        // just to write zeros would charge the owner rent for nothing.
        if clearing {
            return Ok(());
        }
        ledger._pad[0] = LEDGER_V_AUTHORIZED;
        let new_space = info.data_len() + AUTH_TRAILER;
        let needed = Rent::get()?.minimum_balance(new_space);
        let have = info.lamports();
        if needed > have {
            anchor_lang::system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.owner.to_account_info(),
                        to: info.clone(),
                    },
                ),
                needed - have,
            )?;
        }
        info.resize(new_space)?;
        store_ledger(&info, &ledger)?;
    }

    let mut d = info.try_borrow_mut_data()?;
    let o = d.len() - AUTH_TRAILER;
    d[o..o + 32].copy_from_slice(&authorized.to_bytes());
    d[o + 32..o + 64].copy_from_slice(&authority.to_bytes());
    Ok(())
}
