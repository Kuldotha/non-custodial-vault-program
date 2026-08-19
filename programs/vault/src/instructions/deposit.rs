use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::access_control::instructions::CreatePermissionCpiBuilder;
use ephemeral_rollups_sdk::access_control::structs::{Member, MembersArgs};
use ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID;
use anchor_lang::system_program;
use anchor_spl::token::{self, Token, Transfer};

use crate::state::*;

/// Wallet → vault. One instruction for both assets: `mint` selects, and on the SOL path the
/// token slots are placeholders that are never read, so the account layout stays fixed.
///
/// Three things happen besides the transfer, all idempotent and none of them worth a separate
/// instruction: the ledger is created if absent, its permission is created if absent, and
/// headroom is topped up — a delegated ledger cannot be reallocated, and `settle` inside the
/// rollup can only use slots that already exist.
#[derive(Accounts)]
#[instruction(mint: Pubkey, amount: u64, min_free: Option<u16>, slot_increase: Option<u16>)]
pub struct Deposit<'info> {
    /// CHECK: the wallet depositing. Off-curve owners are refused in the handler.
    #[account(mut)]
    pub owner: Signer<'info>,

    /// CHECK: validated by hand, and deliberately *not* `init_if_needed` — see
    /// `create_ledger_account` for why.
    #[account(mut, seeds = [b"ledger", owner.key().as_ref()], bump)]
    pub ledger: UncheckedAccount<'info>,

    /// CHECK: the ledger's basenet permission account, created here on first use.
    #[account(mut)]
    pub permission: UncheckedAccount<'info>,

    /// CHECK: the MagicBlock permission program, pinned to its known address.
    #[account(address = PERMISSION_PROGRAM_ID)]
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
    // Off-curve owners are refused outright — a program's ledger moves value only through
    // `settle`. That keeps the wallet paying in and the wallet taking delivery the same person,
    // and it does not lean on the System program refusing to debit a PDA: an SPL transfer needs
    // only the authority's signature, so without this a PDA could move tokens in and out while
    // SOL stayed impossible.
    require!(
        !is_pda(&ctx.accounts.owner.key()),
        VaultError::OffCurveOwnerNotAllowed
    );

    let slot_increase = slot_increase.unwrap_or(DEFAULT_SLOTS);
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
    // A permission cannot be added to a delegated ledger, so repairing a missing one means
    // undelegate, create, delegate — and until then the ledger sits on a private validator
    // readable by anyone holding a token, silently. The rent returns when the ledger closes, so
    // a wallet that never touches a private validator has lent 567 bytes, not spent them.
    //
    // `permission` is not seed-checked here: the permission program derives and validates the
    // canonical PDA inside the CPI below, so a wrong *empty* account fails there. A *non-empty*
    // junk account slips past this `data_is_empty` gate and skips creation, leaving the ledger
    // unprotected — but only the owner signs their own deposit, so that is self-harm, not an
    // attack on anyone else, and a real client always passes the canonical permission.
    if ctx.accounts.permission.data_is_empty() {
        store_ledger(&ledger_info, &ledger)?;
        create_permission(&ctx, ledger.owner, ledger.bump)?;
    }

    // Before the claim, not after: a deposit of a new mint into a ledger with no free slot
    // would otherwise fail on the very growth this call is about to perform.
    ensure_headroom(
        &ledger_info,
        &mut ledger,
        &ctx.accounts.owner,
        &ctx.accounts.system_program,
        min_free,
        slot_increase,
    )?;

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

    ledger.credit(index, amount)?;

    store_ledger(&ledger_info, &ledger)
}

/// Creates the ledger's permission, naming its owner at flags 0.
fn create_permission(ctx: &Context<Deposit>, owner: Pubkey, bump: u8) -> Result<()> {
    // One member: the owner. Off-curve owners are refused above, so there is no PDA case.
    let members = vec![Member { flags: 0, pubkey: owner }];

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
