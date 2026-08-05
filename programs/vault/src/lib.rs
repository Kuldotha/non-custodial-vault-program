//! # Vault
//!
//! An immutable, multi-mint ledger primitive. It holds SOL and SPL tokens in a single
//! **vault** and records who is owed what in per-owner **ledgers** that can be delegated
//! to a MagicBlock ephemeral rollup.
//!
//! The defining property, enforced in `settle` and not merely documented: **value can
//! never move between two humans.** Every movement has *at most* one human side — program
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

    /// Funds ["vault"] to its rent floor. Upgrade authority only, and it must be run
    /// before that authority is burned. Idempotent.
    pub fn initialize_vault(ctx: Context<InitializeVault>) -> Result<()> {
        instructions::initialize_vault::handler(ctx)
    }

    /// Wallet → vault. `mint` is the asset selector; `SOL_MINT` moves lamports.
    ///
    /// Also the only way a ledger is created: it opens the ledger and its permission on
    /// first use and keeps the headroom band, so none of those is a separate instruction.
    /// A program opens its own ledger by depositing zero, and grows it the same way.
    /// Opens an empty wallet ledger at a chosen size, owner-funded.
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
    pub fn grow_pda_ledger(ctx: Context<GrowPdaLedger>, min_free: u16, step: u16) -> Result<()> {
        instructions::grow_pda_ledger::handler(ctx, min_free, step)
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

    pub fn deposit(
        ctx: Context<Deposit>,
        mint: Pubkey,
        amount: u64,
        min_free: Option<u16>,
        slot_increase: Option<u16>,
    ) -> Result<()> {
        instructions::deposit::handler(ctx, mint, amount, min_free, slot_increase)
    }

    /// Vault → wallet. The destination is derived from the signer, never passed.
    pub fn withdraw(ctx: Context<Withdraw>, mint: Pubkey, amount: u64) -> Result<()> {
        instructions::withdraw::handler(ctx, mint, amount)
    }

    /// The only cross-ledger movement. Pure bookkeeping — the reserves are untouched.
    pub fn settle(ctx: Context<Settle>, mint: Pubkey, amount: u64) -> Result<()> {
        instructions::settle::handler(ctx, mint, amount)
    }

    /// Creates a receipt — an ephemeral, vault-owned account holding the program's side of
    /// the terms. Human-permissionless: a debit means nothing until its settle is consented.
    pub fn create_receipt(
        ctx: Context<CreateReceipt>,
        nonce: u64,
        movements: Vec<Movement>,
    ) -> Result<()> {
        instructions::receipt::create_handler(ctx, nonce, movements)
    }

    /// Settles a receipt. A debit of the human side requires the consenter to sign — the
    /// ledger's owner, or its session key for this receipt's authority. Credits need nobody.
    pub fn settle_receipt(ctx: Context<SettleReceipt>) -> Result<()> {
        instructions::receipt::settle_handler(ctx)
    }

    /// Grants or revokes a session key that may consent to debits of a wallet ledger, locked
    /// to a single program authority. basenet only; the owner signs and funds the trailer.
    /// An all-zero key revokes.
    pub fn assign_ledger_authorization(
        ctx: Context<AssignLedgerAuthorization>,
        authorized: Pubkey,
        authority: Pubkey,
    ) -> Result<()> {
        instructions::authorize::handler(ctx, authorized, authority)
    }

    /// Sweeps everything back to the owner, then closes the ledger and its permission.
    pub fn close_ledger<'info>(
        ctx: Context<'_, '_, '_, 'info, CloseLedger<'info>>,
    ) -> Result<()> {
        instructions::close_ledger::handler(ctx)
    }

    /// Hands a ledger to the rollup. Refuses if it has no permission.
    pub fn delegate_ledger(ctx: Context<DelegateLedger>, validator: Option<Pubkey>) -> Result<()> {
        instructions::delegation::delegate_handler(ctx, validator)
    }

    /// Ends the session and returns the ledger to basenet. Commit is implicit.
    pub fn undelegate(ctx: Context<Undelegate>) -> Result<()> {
        instructions::delegation::undelegate_handler(ctx)
    }
}
