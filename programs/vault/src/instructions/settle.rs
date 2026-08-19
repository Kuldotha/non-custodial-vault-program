use anchor_lang::prelude::*;

use crate::state::*;

/// Moves value between two ledgers, as pure bookkeeping — the reserves are never touched,
/// which is why it works unchanged inside the rollup. `settle_receipt` is the other way
/// ledgers move, and enforces the same one-human rule separately.
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
        // Aliasing src and dst would deserialize one account into two copies and write the
        // credit back last, minting balance from nothing. Anchor does not dedupe for us.
        constraint = dst.key() != src.key() @ VaultError::DuplicateLedger,
    )]
    pub dst: Account<'info, Ledger>,

    /// CHECK: must equal `src.owner` and must sign — a wallet directly, a PDA by
    /// `invoke_signed`. The destination needs no such account: a credit is not authorised
    /// by anyone, and `dst.owner` is already bound to `dst` by the seeds above.
    pub src_authority: UncheckedAccount<'info>,

    /// CHECK: consents to opening a new slot on a human `dst` — its owner or session key.
    pub dst_consenter: UncheckedAccount<'info>,
}

pub fn handler(ctx: Context<Settle>, mint: Pubkey, amount: u64) -> Result<()> {
    let src = &ctx.accounts.src;
    let dst = &ctx.accounts.dst;

    require_keys_eq!(ctx.accounts.src_authority.key(), src.owner, VaultError::BadAuthority);

    // At most one human, so Alice-pays-Bob is unrepresentable. Do NOT "simplify" to
    // `src.pda_auth != dst.pda_auth`, which reads tidier and allows exactly that.
    require!(src.pda_auth || dst.pda_auth, VaultError::NotProgramMediated);

    // The debited side authorises: a human by signing, a program by `invoke_signed` over seeds
    // it alone holds. A credit needs none, so a game can pay a player who has closed the app.
    require!(
        ctx.accounts.src_authority.is_signer,
        if src.pda_auth { VaultError::MissingProgramSignature } else { VaultError::MissingUserSignature },
    );

    let may_claim = if dst.pda_auth || dst.index_of(&mint).is_some() {
        true
    } else {
        let consenter = &ctx.accounts.dst_consenter;
        require!(consenter.is_signer, VaultError::MissingUserSignature);
        let allowed = consenter.key() == dst.owner
            || (dst.authorized != Pubkey::default() && dst.authorized == consenter.key());
        require!(allowed, VaultError::NotAuthorizedToConsent);
        true
    };

    let src_index = ctx.accounts.src.index_of(&mint).ok_or(VaultError::NoBalance)?;
    ctx.accounts.src.debit(src_index, amount)?;

    let dst_index = ctx.accounts.dst.index_for_credit(&mint, may_claim)?;
    ctx.accounts.dst.credit(dst_index, amount)?;

    Ok(())
}
