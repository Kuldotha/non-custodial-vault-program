use anchor_lang::prelude::*;
use anchor_lang::system_program;
use anchor_spl::token::{self, Token, Transfer};
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};

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
/// - **the permission** is created if absent, which makes "no ledger is ever readable in a
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

    // Before the ledger can hold anything it must be unreadable in a rollup. Creating the
    // permission here — rather than in a separate instruction a caller could skip — is what
    // removes the window in which a ledger exists unprotected. It needs the ledger account
    // to exist and carry its discriminator, so write the fresh state out first.
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

/// Creates the ledger's basenet permission naming exactly one member at **flags 0**: the
/// owner. That is why a player can read their own balance inside a private rollup.
///
/// Nothing else is named, and no caller can add to the list. The vault reaches every ledger
/// it owns without being named — owning the permissioned account is enough — so `settle`
/// writing two ledgers at once needs no extra membership. Anything a caller could add here
/// would be a third party admitted to a private ledger for the life of the account, decided
/// by whoever happened to open it.
///
/// Flags 0 is deliberate. The flag set is `AUTHORITY`, `TX_LOGS`, `TX_BALANCES`,
/// `TX_MESSAGE`, `ACCOUNT_SIGNATURES`; none of them is a read flag, so membership alone is
/// what grants the read. Granting `AUTHORITY` would let an owner rewrite their own ACL and
/// so expose a ledger this program is supposed to keep private — in code that can never be
/// patched, that has to be impossible rather than discouraged. Every ledger's privacy is
/// therefore fixed at creation and no instruction can widen it.
///
/// Deliberately the basenet permission, not the ephemeral one. An ephemeral permission is
/// created inside the rollup — after the account is already delegated and live there —
/// which leaves a window in which the ledger is readable. Creating it on basenet closes
/// that window entirely: the rollup copies the basenet permission data when it needs it,
/// so the permission is never delegated and has no commit lifecycle.
fn create_permission(
    ctx: &Context<Deposit>,
    owner: Pubkey,
    bump: u8,
) -> Result<()> {
    let ledger_info = ctx.accounts.ledger.to_account_info();

    CreatePermissionCpiBuilder::new(&ctx.accounts.permission_program.to_account_info())
        .permissioned_account(&ledger_info)
        .permission(&ctx.accounts.permission.to_account_info())
        .payer(&ctx.accounts.owner.to_account_info())
        .system_program(&ctx.accounts.system_program.to_account_info())
        .args(MembersArgs {
            members: Some(vec![Member { flags: 0, pubkey: owner }]),
        })
        .invoke_signed(&[&[b"ledger", owner.as_ref(), &[bump]]])
        .map_err(|e| {
            error!(VaultError::PermissionFailed)
                .with_source(source!())
                .with_values(("cpi", e.to_string()))
        })
}
