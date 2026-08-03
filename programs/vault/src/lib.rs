//! # Vault
//!
//! An immutable, multi-mint ledger primitive. It holds SOL and SPL tokens in a single
//! **vault** and records who is owed what in per-owner **ledgers** that can be delegated
//! to a MagicBlock ephemeral rollup.
//!
//! The defining property, enforced in `settle` and not merely documented: **value can
//! never move between two humans.** Every movement has exactly one program side and one
//! human side, so this cannot be used as a payment rail.
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

    /// Creates a receipt — an ephemeral, vault-owned account holding authorised terms.
    pub fn create_receipt(
        ctx: Context<CreateReceipt>,
        nonce: u64,
        movements: Vec<Movement>,
    ) -> Result<()> {
        instructions::receipt::create_handler(ctx, nonce, movements)
    }

    /// Settles a receipt. Needs no signature: both parties consented at creation, and the
    /// receipt names both ledger owners so neither can be substituted.
    pub fn settle_receipt(ctx: Context<SettleReceipt>) -> Result<()> {
        instructions::receipt::settle_handler(ctx)
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
