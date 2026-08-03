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
}

pub fn handler(ctx: Context<Withdraw>, mint: Pubkey, amount: u64) -> Result<()> {
    // Off-curve owners are refused outright. A program's ledger is filled and emptied by
    // `settle` against a human who already holds a balance, and that is the only way in or out.
    //
    // Two reasons this is a hard rule rather than a convention. It keeps the wallet paying in
    // and the wallet taking delivery the same person — anything else needs a stand-in wallet on
    // both sides, and a stand-in that can differ is a transfer between people wearing a
    // program as a disguise. And it does not depend on the System program refusing to debit a
    // PDA: SPL transfers only need the authority to sign, so without this check a PDA could
    // move *tokens* in and out directly while SOL stayed impossible.
    require!(
        !is_pda(&ctx.accounts.owner.key()),
        VaultError::OffCurveOwnerNotAllowed
    );

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
                    to: ctx.accounts.owner.to_account_info(),
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
        require_keys_eq!(dst_owner, ctx.accounts.owner.key(), VaultError::MintMismatch);

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
