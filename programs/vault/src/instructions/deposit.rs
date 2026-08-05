use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};
use anchor_lang::system_program;
use anchor_spl::token::{self, Token, Transfer};

use crate::state::*;

/// Wallet → vault, and the only way a ledger ever comes into existence.
///
/// One instruction for both assets: `mint` selects. When it is `SOL_MINT` the token
/// slots are placeholders and are never read, so the account layout stays fixed.
///
/// Three things happen here besides the transfer, all idempotent, and all deliberately
/// *not* separate instructions:
///
/// - **the ledger** is created if absent. There is no public `create_ledger`, so a ledger
///   cannot exist in a state this instruction did not produce;
/// - **the permission is created if absent**, which makes "no ledger is ever readable in a
///   rollup" structural rather than a check `delegate` has to remember to make;
/// - **headroom** is topped up, because a delegated ledger cannot be reallocated and
///   `settle` inside the rollup can only claim slots that already exist.
///
/// A program that is only ever *credited* through `settle` still needs a ledger. It opens
/// one by depositing — `amount` may be zero — signing for its own PDA with `invoke_signed`.
/// That is also how it replenishes headroom between sessions. A program's ledger holds
/// every mint it ever pays out, so it reaches its working size through repeated zero
/// deposits with a high `min_free`; each one grows by at most `slot_increase`.
#[derive(Accounts)]
#[instruction(mint: Pubkey, amount: u64, min_free: Option<u16>, slot_increase: Option<u16>)]
pub struct Deposit<'info> {
    /// CHECK: the ledger owner — a wallet, or a program's PDA signing via invoke_signed.
    /// Signing is what proves a program controls the vault-authority it is opening.
    #[account(mut)]
    pub owner: Signer<'info>,

    /// CHECK: created here on first use and validated by hand. Deliberately *not*
    /// `init_if_needed`: that constraint re-checks its `space` expression against the
    /// account's real length on every call, so the first growth would break every
    /// subsequent deposit. See `create_ledger_account`.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    /// CHECK: the ledger's basenet permission account, created here on first use.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program.
    pub permission_program: UncheckedAccount<'info>,

    /// CHECK: the SOL reserve. A System-owned PDA; holds every deposited lamport.
    #[account(mut, seeds = [b"vault"], bump)]
    pub vault: UncheckedAccount<'info>,

    /// CHECK: the SPL reserve for `mint` — the vault's ATA, which the caller must have
    /// created with `create_idempotent`. Placeholder on the SOL path, so it can carry
    /// neither a type nor a `mut` constraint; validated by hand in the handler.
    pub vault_token: UncheckedAccount<'info>,

    /// CHECK: the owner's token account. Placeholder on the SOL path.
    pub owner_token: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<Deposit>,
    mint: Pubkey,
    amount: u64,
    min_free: Option<u16>,
    slot_increase: Option<u16>,
) -> Result<()> {
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

    let step = slot_increase.unwrap_or(DEFAULT_SLOTS);
    let min_free = min_free.unwrap_or(DEFAULT_MIN_FREE);
    let ledger_info = ctx.accounts.ledger.to_account_info();

    let mut ledger = if ctx.accounts.ledger.data_is_empty() {
        create_ledger_account(
            &ledger_info,
            &ctx.accounts.owner,
            &ctx.accounts.system_program,
            ctx.bumps.ledger,
        )?
    } else {
        load_ledger(&ledger_info)?
    };

    // Created here, unconditionally, rather than left to whoever later delegates.
    //
    // A permission cannot be added to a delegated ledger — `OpenPermission` takes it as
    // `Account<Ledger>`, and while delegated the delegation program owns it — so repairing a
    // missing one means undelegate, create, delegate. For the whole time it was wrong the ledger
    // sat on a private validator readable by anyone holding a token, and nobody would have known.
    //
    // The rent comes back when the ledger closes, so a wallet that never touches a private
    // validator has lent 567 bytes, not spent them. That is the cheaper mistake.
    if ctx.accounts.permission.data_is_empty() {
        store_ledger(&ledger_info, &ledger)?;
        create_permission(&ctx, ledger.owner, ledger.bump)?;
    }

    let index = ledger.index_or_claim(&mint)?;

    if mint == SOL_MINT {
        // Rent must already be in place — see initialize_vault. Never funded from a
        // deposit, or the last lamports credited could not be withdrawn.
        require!(
            ctx.accounts.vault.lamports() >= vault_floor()?,
            VaultError::VaultNotInitialized
        );

        // Only the wallet itself can authorise a debit from its own lamports.
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.owner.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                },
            ),
            amount,
        )?;
    } else {
        require_reserve(&ctx.accounts.vault_token, &ctx.accounts.vault.key(), &mint)?;

        let (src_mint, _, _) = token_fields(&ctx.accounts.owner_token)?;
        require_keys_eq!(src_mint, mint, VaultError::MintMismatch);

        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.owner_token.to_account_info(),
                    to: ctx.accounts.vault_token.to_account_info(),
                    authority: ctx.accounts.owner.to_account_info(),
                },
            ),
            amount,
        )?;
    }

    let entry = &mut ledger.entries[index];
    entry.amount = entry.amount.checked_add(amount).ok_or(VaultError::Overflow)?;

    // Deposit is basenet-only, so it is the natural place to keep the headroom band —
    // a delegated ledger can never grow, and settle refuses rather than growing. Grows the
    // account before the write below, so the buffer is always big enough for the state.
    ensure_headroom(
        &ledger_info,
        &mut ledger,
        &ctx.accounts.owner,
        &ctx.accounts.system_program,
        min_free,
        step,
    )?;

    store_ledger(&ledger_info, &ledger)
}

/// Creates the ledger's permission, naming its owner at flags 0 — and the owner's program too
/// when the owner is a PDA, so a program can reach the ledgers it owns.
fn create_permission(ctx: &Context<Deposit>, owner: Pubkey, bump: u8) -> Result<()> {
    let owner_info = ctx.accounts.owner.to_account_info();
    let mut members = vec![Member { flags: 0, pubkey: owner }];
    if *owner_info.owner != anchor_lang::system_program::ID {
        members.push(Member { flags: 0, pubkey: *owner_info.owner });
    }

    let ledger_info = ctx.accounts.ledger.to_account_info();
    CreatePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .permissioned_account(&ledger_info)
        .permission(&ctx.accounts.permission.to_account_info())
        .payer(&ctx.accounts.owner.to_account_info())
        .system_program(&ctx.accounts.system_program.to_account_info())
        .args(MembersArgs { members: Some(members) })
        .invoke_signed(&[&[b"ledger", owner.as_ref(), &[bump]]])
        .map_err(|_| error!(VaultError::PermissionFailed))
}
