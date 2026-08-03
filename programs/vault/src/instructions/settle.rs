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

    // Exactly one program side and one human side. Checked before any balance moves.
    require!(src.pda_auth != dst.pda_auth, VaultError::NotProgramMediated);

    // The program side always signs — it is the one mediating the movement.
    let program_side = if src.pda_auth {
        &ctx.accounts.src_authority
    } else {
        &ctx.accounts.dst_authority
    };
    require!(program_side.is_signer, VaultError::MissingProgramSignature);

    // A human signs to be debited, never to be credited. So a game can pay out to a
    // player who is offline, and can never pull from one who did not authorise it.
    if !src.pda_auth {
        require!(
            ctx.accounts.src_authority.is_signer,
            VaultError::MissingUserSignature
        );
    }

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
