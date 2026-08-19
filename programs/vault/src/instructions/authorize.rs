use anchor_lang::prelude::*;

use crate::instructions::open_pda_ledger::verify_pda_owner;
use crate::state::*;

/// Grants — or revokes, with an all-zero key — a session key that may consent to debits of
/// this ledger.
///
/// Neither side may be a program: not the ledger, not the key. A program consents by
/// `invoke_signed` over its own seeds and nothing else, and a session key standing in for
/// that is the one capability the vault must never hand out.
///
/// The key is unscoped — any program may write a receipt against this ledger — so it is full
/// spending authority over the balance. It cannot reach the wallet behind it: `withdraw`
/// derives its ledger from the signer. Revoking a leaked key means undelegating first, since
/// Anchor's owner check keeps this to basenet.
#[derive(Accounts)]
pub struct AssignLedgerAuthorization<'info> {
    #[account(
        mut,
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,
    pub owner: Signer<'info>,
}

pub fn handler(ctx: Context<AssignLedgerAuthorization>, authorized: Pubkey) -> Result<()> {
    let ledger = &mut ctx.accounts.ledger;
    require!(!ledger.pda_auth, VaultError::CannotAuthorizePdaLedger);

    require!(
        authorized == Pubkey::default() || !is_pda(&authorized),
        VaultError::BadAuthorizedKey
    );

    ledger.authorized = authorized;
    Ok(())
}

/// Backfills a PDA ledger's stored authority — the member program it belongs to — for ledgers
/// opened before that field was written. The owner PDA signs and its seeds derive it under
/// `member_program`, so only the true program can set it, to its own key.
#[derive(Accounts)]
#[instruction(member_program: Pubkey, owner_seeds: Vec<Vec<u8>>)]
pub struct AuthorizePdaLedger<'info> {
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [b"ledger", owner.key().as_ref()],
        bump = ledger.bump,
        has_one = owner,
    )]
    pub ledger: Account<'info, Ledger>,
}

pub fn authorize_pda_handler(
    ctx: Context<AuthorizePdaLedger>,
    member_program: Pubkey,
    owner_seeds: Vec<Vec<u8>>,
) -> Result<()> {
    verify_pda_owner(&ctx.accounts.owner, &member_program, &owner_seeds)?;
    let ledger = &mut ctx.accounts.ledger;
    require!(ledger.pda_auth, VaultError::OwnerNotPda);
    ledger.authorized = member_program;
    Ok(())
}
