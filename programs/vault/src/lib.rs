//! # Vault
//!
//! An immutable, multi-mint ledger primitive. It holds SOL and SPL tokens in a single
//! **vault** and records who is owed what in per-owner **ledgers** that can be delegated
//! to a MagicBlock ephemeral rollup.
//!
//! The defining property, enforced in `settle` and `settle_receipt` and not merely
//! documented: **value can never move between two humans.** At most one human side — program
//! to program is allowed, human to human is unrepresentable — so this cannot be used as a
//! payment rail. `deposit` and `withdraw` are wallet-only for the same reason.
//!
//! See `vault-program-spec.md` for the full design.

use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::ephemeral;

pub mod state;
pub mod instructions;

use instructions::*;

declare_id!("9vDAQgdHWCPQZabumgcuwoSLzWnRyQkSM1EHQnW8YXjs");

#[ephemeral]
#[program]
pub mod vault {
    use super::*;

    /// Funds ["vault"] to its rent floor. Permissionless and idempotent; run it once before
    /// anything touches the SOL path.
    pub fn initialize_vault(ctx: Context<InitializeVault>) -> Result<()> {
        instructions::initialize_vault::handler(ctx)
    }

    /// Opens an empty wallet ledger at a chosen size, owner-funded. A zero-amount `deposit`
    /// does the same at the default size.
    pub fn open_wallet_ledger(ctx: Context<OpenWalletLedger>, slots: u16) -> Result<()> {
        instructions::open_wallet_ledger::handler(ctx, slots)
    }

    /// Opens an empty ledger for a program's PDA, sponsor-funded. The member program is
    /// proven by deriving the owner from its seeds — a wrong program is unconstructible.
    pub fn open_pda_ledger(
        ctx: Context<OpenPdaLedger>,
        slots: u16,
        member_program: Pubkey,
        owner_seeds: Vec<Vec<u8>>,
    ) -> Result<()> {
        instructions::open_pda_ledger::handler(ctx, slots, member_program, owner_seeds)
    }

    /// Adds slots to a program's ledger, funded by its sponsor. Wallets grow through `deposit`.
    pub fn grow_pda_ledger(ctx: Context<GrowPdaLedger>, min_free: u16, slot_increase: u16) -> Result<()> {
        instructions::grow_pda_ledger::handler(ctx, min_free, slot_increase)
    }

    /// Deletes a ledger's permission — the owner explicitly giving privacy up, for a ledger
    /// that is meant to be watched. The only way a ledger becomes public.
    pub fn make_public(ctx: Context<MakePublic>) -> Result<()> {
        instructions::privacy::make_public_handler(ctx)
    }

    /// Takes it back for a wallet: the permission returns, naming the owner.
    pub fn make_wallet_ledger_private(ctx: Context<MakeWalletLedgerPrivate>) -> Result<()> {
        instructions::privacy::make_wallet_ledger_private_handler(ctx)
    }

    /// Takes it back for a program's PDA, with the same proof `open_pda_ledger` demands.
    pub fn make_pda_ledger_private(
        ctx: Context<MakePdaLedgerPrivate>,
        member_program: Pubkey,
        owner_seeds: Vec<Vec<u8>>,
    ) -> Result<()> {
        instructions::privacy::make_pda_ledger_private_handler(ctx, member_program, owner_seeds)
    }

    /// Wallet → vault. `mint` selects the asset; `SOL_MINT` moves lamports. Creates the ledger
    /// and its permission on first use, and tops up the free-slot band.
    pub fn deposit(
        ctx: Context<Deposit>,
        mint: Pubkey,
        amount: u64,
        min_free: Option<u16>,
        slot_increase: Option<u16>,
    ) -> Result<()> {
        instructions::deposit::handler(ctx, mint, amount, min_free, slot_increase)
    }

    /// Vault → wallet, and only ever the signer's own — no other beneficiary is expressible.
    pub fn withdraw(ctx: Context<Withdraw>, mint: Pubkey, amount: u64) -> Result<()> {
        instructions::withdraw::handler(ctx, mint, amount)
    }

    /// Moves value between two ledgers. Pure bookkeeping — the reserves are untouched.
    pub fn settle(ctx: Context<Settle>, mint: Pubkey, amount: u64) -> Result<()> {
        instructions::settle::handler(ctx, mint, amount)
    }

    /// Creates a receipt — an ephemeral, vault-owned account holding the terms. This is the
    /// approval: every owner a movement debits signs here, as does any wallet a credit would open
    /// a new slot for, so settle needs no further consent.
    ///
    /// `member_program` is proven, not trusted: the authority must sign, and `authority_seeds`
    /// must derive it under that program. It is the only program the settle will call back.
    /// `args` are opaque here — stored verbatim and handed back to that callback.
    pub fn create_receipt<'info>(
        ctx: Context<'_, '_, '_, 'info, CreateReceipt<'info>>,
        movements: Vec<Movement>,
        member_program: Pubkey,
        authority_seeds: Vec<Vec<u8>>,
        owners: Vec<Pubkey>,
        callback_disc: [u8; 8],
        args: Vec<u8>,
    ) -> Result<()> {
        instructions::receipt::create_handler(
            ctx, movements, member_program, authority_seeds, owners, callback_disc, args,
        )
    }

    /// Settles a receipt and calls back into the program that authorised it, in the same
    /// instruction. The receipt is zeroed and handed over *before* the callback runs, so the
    /// callback is told the terms in its instruction data rather than reading them — proof of
    /// payment is `vault_authority`'s signature on that call, not the account.
    ///
    /// Permissionless: the debits were approved at creation, so settle only executes them and
    /// fires the callback. Remaining accounts are forwarded with their privileges intact.
    pub fn settle_receipt<'info>(
        ctx: Context<'_, '_, '_, 'info, SettleReceipt<'info>>,
    ) -> Result<()> {
        instructions::receipt::settle_handler(ctx)
    }

    /// Grants or revokes a session key that may consent to debits of a wallet ledger. basenet
    /// only; the owner signs. An all-zero key revokes.
    pub fn assign_ledger_authorization(
        ctx: Context<AssignLedgerAuthorization>,
        authorized: Pubkey,
    ) -> Result<()> {
        instructions::authorize::handler(ctx, authorized)
    }

    /// Backfills a PDA ledger's stored member program, for ledgers opened before the field was
    /// written. The owner PDA signs and its seeds prove it under `member_program`.
    pub fn authorize_pda_ledger(
        ctx: Context<AuthorizePdaLedger>,
        member_program: Pubkey,
        owner_seeds: Vec<Vec<u8>>,
    ) -> Result<()> {
        instructions::authorize::authorize_pda_handler(ctx, member_program, owner_seeds)
    }

    /// Sweeps everything back to the owner, then closes the ledger and its permission.
    pub fn close_ledger<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLedger<'info>>,
    ) -> Result<()> {
        instructions::close_ledger::handler(ctx)
    }

    /// Hands a ledger to the rollup. No permission is required — a ledger bound for a private
    /// validator must have had one opened first, by whoever delegates it.
    pub fn delegate_ledger(ctx: Context<DelegateLedger>, validator: Option<Pubkey>) -> Result<()> {
        instructions::delegation::delegate_handler(ctx, validator)
    }

    /// Ends the session and returns the ledger to basenet. Sent to the rollup, not to basenet;
    /// the commit is implicit.
    pub fn undelegate(ctx: Context<Undelegate>) -> Result<()> {
        instructions::delegation::undelegate_handler(ctx)
    }
}
