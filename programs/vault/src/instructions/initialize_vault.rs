use anchor_lang::prelude::*;
use anchor_lang::system_program;

use crate::state::*;

/// Funds the SOL reserve to its rent-exempt minimum. Run once, before anything else.
///
/// Paid separately rather than out of deposits, or the last lamports credited to a ledger
/// could never be withdrawn — every SOL path spends `lamports - floor`.
///
/// Permissionless and idempotent: the vault is System-owned, so a plain transfer funds it
/// whatever this instruction says.
#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: the SOL reserve. A System-owned PDA holding every deposited lamport.
    #[account(mut, seeds = [b"vault"], bump)]
    pub vault: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<InitializeVault>) -> Result<()> {
    let floor = vault_floor()?;
    let have = ctx.accounts.vault.lamports();
    if have >= floor {
        return Ok(());
    }

    system_program::transfer(
        CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            system_program::Transfer {
                from: ctx.accounts.payer.to_account_info(),
                to: ctx.accounts.vault.to_account_info(),
            },
        ),
        floor - have,
    )?;

    Ok(())
}
