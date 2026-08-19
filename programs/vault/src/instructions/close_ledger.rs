use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, Transfer};
use ephemeral_rollups_sdk::access_control::instructions::ClosePermissionCpiBuilder;
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;

use crate::state::*;

/// Sweeps every balance back to the owner, then closes the ledger and its permission. basenet
/// only — Anchor's ownership check rejects a delegated ledger.
///
/// The **rent** goes to whoever put it up, not to the owner. For a wallet they are the same
/// account; for a sponsored PDA they are not, and routing it any other way would make
/// sponsoring a ledger a way of handing its owner money.
///
/// Every non-zero token entry must be paid out in the same transaction: pass its
/// `(vault_token, owner_token)` pair in `remaining_accounts`, **in entry order**. Closing is
/// refused while any balance is unaccounted for, so a partial list cannot strand value.
#[derive(Accounts)]
pub struct CloseLedger<'info> {
    /// CHECK: the ledger owner — wallet, or a program's PDA signing via invoke_signed.
    #[account(mut)]
    pub owner: Signer<'info>,

    /// Receives the rent back and must sign. Recorded when the ledger was opened; for a
    /// wallet's ledger it is the owner, so this is the owner signing twice over.
    #[account(mut)]
    pub rent_payer: Signer<'info>,

    #[account(
        mut,
        close = rent_payer,
        constraint = ledger.rent_payer == rent_payer.key() @ VaultError::NotRentPayer,
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,

    /// CHECK: the SOL reserve.
    #[account(mut, seeds = [b"vault"], bump)]
    pub vault: UncheckedAccount<'info>,

    /// CHECK: the ledger's permission account, closed here too.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program, pinned to its known address.
    #[account(address = PERMISSION_PROGRAM_ID)]
    pub permission_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handler<'info>(
    ctx: Context<'_, '_, '_, 'info, CloseLedger<'info>>,
) -> Result<()> {
    let owner_key = ctx.accounts.owner.key();
    let ledger_bump = ctx.accounts.ledger.bump;
    let vault_bump = ctx.bumps.vault;

    // ── sweep SOL (entry 0) ──────────────────────────────────────────────────
    let sol = ctx.accounts.ledger.entries[0].amount;
    if sol > 0 {
        let rent_floor = vault_floor()?;
        let spendable = ctx.accounts.vault.lamports().saturating_sub(rent_floor);
        require!(spendable >= sol, VaultError::InsufficientReserve);
        anchor_lang::system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.owner.to_account_info(),
                },
                &[&[b"vault".as_ref(), &[vault_bump]]],
            ),
            sol,
        )?;
        ctx.accounts.ledger.entries[0].amount = 0;
    }

    // ── sweep every non-zero token entry ─────────────────────────────────────
    let mut pair = 0usize;
    let capacity = ctx.accounts.ledger.capacity();
    for i in 1..capacity {
        let (mint, amount) = {
            let e = &ctx.accounts.ledger.entries[i];
            (e.mint, e.amount)
        };
        if mint == SOL_MINT || amount == 0 {
            continue;
        }

        let vault_token = ctx
            .remaining_accounts
            .get(pair * 2)
            .ok_or(VaultError::MissingTokenAccounts)?;
        let owner_token = ctx
            .remaining_accounts
            .get(pair * 2 + 1)
            .ok_or(VaultError::MissingTokenAccounts)?;
        pair += 1;

        let reserve = require_reserve(vault_token, &ctx.accounts.vault.key(), &mint)?;
        require!(reserve >= amount, VaultError::InsufficientReserve);

        let (d_mint, d_owner, _) = token_fields(owner_token)?;
        require_keys_eq!(d_mint, mint, VaultError::MintMismatch);
        require_keys_eq!(d_owner, owner_key, VaultError::MintMismatch);

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: vault_token.clone(),
                    to: owner_token.clone(),
                    authority: ctx.accounts.vault.to_account_info(),
                },
                &[&[b"vault".as_ref(), &[vault_bump]]],
            ),
            amount,
        )?;
        ctx.accounts.ledger.entries[i].amount = 0;
    }

    // Nothing may be left behind: if any entry still carries a balance the caller
    // gave us an incomplete account list, and closing would strand it.
    require!(
        ctx.accounts
            .ledger
            .entries
            .iter()
            .all(|e| e.amount == 0),
        VaultError::MissingTokenAccounts
    );

    // ── close the permission, then the ledger (Anchor's `close = rent_payer`) ─
    // bound to locals: the CPI builder borrows these for its whole lifetime
    let ledger_info = ctx.accounts.ledger.to_account_info();
    let perm_program = ctx.accounts.permission_program.to_account_info();
    let perm_info = ctx.accounts.permission.to_account_info();
    let payer_info = ctx.accounts.rent_payer.to_account_info();

    // Ledgers predating permissions have none, and the permission program panics on an empty
    // account — without this guard their rent would be locked up by its own absence.
    if !perm_info.data_is_empty() {
        ClosePermissionCpiBuilder::new(&perm_program)
            .payer(&payer_info)
            // the ledger authorises its own closure by signing with its seeds
            .authority(&ledger_info, false)
            .permissioned_account(&ledger_info, true)
            .permission(&perm_info)
            .invoke_signed(&[&[b"ledger", owner_key.as_ref(), &[ledger_bump]]])
            .map_err(|_| error!(VaultError::PermissionFailed))?;
    }

    Ok(())
}
