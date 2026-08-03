use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, Transfer};

use crate::state::*;

/// The destination is derived from the signer and never passed, which is what makes
/// withdrawal same-owner-only.
#[derive(Accounts)]
#[instruction(mint: Pubkey)]
pub struct Withdraw<'info> {
    /// CHECK: the ledger owner — a wallet, or a program's PDA signing via invoke_signed.
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,

    /// CHECK: the SOL reserve.
    #[account(mut, seeds = [b"vault"], bump)]
    pub vault: UncheckedAccount<'info>,

    /// CHECK: the SPL reserve for `mint`. Placeholder on the SOL path, so no `mut` constraint.
    pub vault_token: UncheckedAccount<'info>,

    /// CHECK: the owner's token account. Placeholder on the SOL path.
    pub owner_token: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,

    /// Receives the withdrawal. The same account as `owner` when a wallet withdraws its own
    /// balance — the destination stays derived, never passed. For a PDA's ledger, the wallet
    /// taking delivery, which must sign: value leaves a program ledger only into a wallet that
    /// agreed to accept it.
    #[account(mut)]
    pub receiver: Signer<'info>,

    /// CHECK: the program owning `owner` when that is a PDA. Placeholder otherwise — pass the
    /// System Program. Validated by hand against the PDA's owner field.
    pub owner_program: UncheckedAccount<'info>,

    /// CHECK: that program's ProgramData, which names its upgrade authority. Placeholder on
    /// the self-service path. Validated against the address the program account points at.
    pub owner_program_data: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<Withdraw>, mint: Pubkey, amount: u64) -> Result<()> {
    // Self-service, or a PDA's ledger with an on-curve receiver standing in — see
    // resolve_counterparty for why both must sign.
    resolve_counterparty(
        &ctx.accounts.owner.to_account_info(),
        &ctx.accounts.receiver.to_account_info(),
        &ctx.accounts.owner_program.to_account_info(),
        &ctx.accounts.owner_program_data.to_account_info(),
    )?;

    let ledger = &mut ctx.accounts.ledger;
    let index = ledger.index_of(&mint).ok_or(VaultError::NoBalance)?;

    {
        let entry = &mut ledger.entries[index];
        entry.amount = entry
            .amount
            .checked_sub(amount)
            .ok_or(VaultError::Insufficient)?;
    }

    if mint == SOL_MINT {
        // The vault's own rent is not part of anyone's balance (spec §2.4).
        let rent_floor = vault_floor()?;
        let spendable = ctx
            .accounts
            .vault
            .lamports()
            .saturating_sub(rent_floor);
        require!(spendable >= amount, VaultError::InsufficientReserve);

        // Debiting a PDA the System Program owns: CPI, signed with the vault's seeds.
        let bump = ctx.bumps.vault;
        anchor_lang::system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.vault.to_account_info(),
                    to: ctx.accounts.receiver.to_account_info(),
                },
                &[&[b"vault".as_ref(), &[bump]]],
            ),
            amount,
        )?;
    } else {
        let reserve = require_reserve(&ctx.accounts.vault_token, &ctx.accounts.vault.key(), &mint)?;
        require!(reserve >= amount, VaultError::InsufficientReserve);

        let (dst_mint, dst_owner, _) = token_fields(&ctx.accounts.owner_token)?;
        require_keys_eq!(dst_mint, mint, VaultError::MintMismatch);
        require_keys_eq!(dst_owner, ctx.accounts.receiver.key(), VaultError::MintMismatch);

        let bump = ctx.bumps.vault;
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.vault_token.to_account_info(),
                    to: ctx.accounts.owner_token.to_account_info(),
                    authority: ctx.accounts.vault.to_account_info(),
                },
                &[&[b"vault".as_ref(), &[bump]]],
            ),
            amount,
        )?;
    }

    Ok(())
}
