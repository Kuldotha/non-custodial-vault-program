use anchor_lang::prelude::*;

use crate::state::*;

/// The only instruction that moves value between accounts — and it is pure bookkeeping.
/// The reserves are never touched, which is why it works unchanged inside the rollup.
///
/// The XOR below is the whole point of this program: exactly one side must be a program,
/// so there is no instruction sequence here that moves value from one human to another.
#[derive(Accounts)]
pub struct Settle<'info> {
    #[account(
        mut,
        seeds = [b"ledger", src.owner.as_ref()],
        bump = src.bump,
    )]
    pub src: Account<'info, Ledger>,

    #[account(
        mut,
        seeds = [b"ledger", dst.owner.as_ref()],
        bump = dst.bump,
    )]
    pub dst: Account<'info, Ledger>,

    /// CHECK: must equal `src.owner`. Signs when the source is a program, or when a
    /// human is being debited.
    pub src_authority: UncheckedAccount<'info>,

    /// CHECK: must equal `dst.owner`. Signs when the destination is a program.
    pub dst_authority: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<Settle>, mint: Pubkey, amount: u64) -> Result<()> {
    let src = &ctx.accounts.src;
    let dst = &ctx.accounts.dst;

    require_keys_eq!(ctx.accounts.src_authority.key(), src.owner, VaultError::MissingUserSignature);
    require_keys_eq!(ctx.accounts.dst_authority.key(), dst.owner, VaultError::MissingUserSignature);

    // ── the two rules ────────────────────────────────────────────────────────
    //
    // 1. AT MOST ONE HUMAN. Two human ledgers may never meet, in either direction.
    //    This is what makes the vault not a payments program: there is no instruction
    //    anywhere that moves value from one person to another. `withdraw` derives its
    //    destination from the signer so it cannot be spelled there, `deposit` credits only
    //    the depositor, and this is the one place two ledgers touch at all.
    //
    //    Do NOT "simplify" this to `src.pda_auth != dst.pda_auth` or to a symmetric
    //    "whoever is debited signs". Both read as tidier and both make Alice-pays-Bob
    //    expressible in a single instruction.
    //
    // 2. THE DEBITED SIDE AUTHORISES. A human authorises by signing. A program authorises
    //    by `invoke_signed` over its own seeds, which it can only do for ledgers it owns.
    //    A program therefore cannot debit a human, and cannot debit another program.
    //
    // Checked before any balance moves.
    require!(src.pda_auth || dst.pda_auth, VaultError::NotProgramMediated);

    // Whoever is losing the balance has to have authorised it. A credit needs no signature,
    // which is what lets a game pay out to a player who has closed the app.
    require!(
        ctx.accounts.src_authority.is_signer,
        if src.pda_auth { VaultError::MissingProgramSignature } else { VaultError::MissingUserSignature },
    );

    let src_index = ctx.accounts.src.index_of(&mint).ok_or(VaultError::NoBalance)?;
    {
        let src = &mut ctx.accounts.src;
        let entry = &mut src.entries[src_index];
        entry.amount = entry
            .amount
            .checked_sub(amount)
            .ok_or(VaultError::Insufficient)?;
    }

    let dst_index = ctx.accounts.dst.index_or_claim(&mint)?;
    {
        let dst = &mut ctx.accounts.dst;
        let entry = &mut dst.entries[dst_index];
        entry.amount = entry.amount.checked_add(amount).ok_or(VaultError::Overflow)?;
    }

    Ok(())
}
