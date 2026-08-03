use anchor_lang::prelude::*;
use anchor_lang::system_program;

use crate::state::*;

/// Funds the SOL reserve PDA to its rent-exempt minimum. Must be run once, before
/// anything else — every SOL path refuses to touch an unfunded vault.
///
/// The vault's rent is not part of any ledger's balance, so it cannot come out of
/// deposits: the last lamports credited would be permanently unwithdrawable. Paying it
/// up front, once, keeps `vault.lamports() - rent >= Σ SOL entries` true from genesis.
///
/// Restricted to the program's upgrade authority: this is a deployment step, not a user
/// one. **It must therefore be run before the upgrade authority is burned** — afterwards
/// `upgrade_authority_address` is `None` and no signer can ever satisfy the constraint,
/// which would strand the SOL path permanently. See the deployment order in the spec.
///
/// Idempotent — calling it again does nothing.
#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: only its address is used, to reach the program data account.
    #[account(constraint = program.programdata_address()? == Some(program_data.key()))]
    pub program: Program<'info, crate::program::Vault>,

    #[account(constraint = program_data.upgrade_authority_address == Some(payer.key())
        @ VaultError::NotUpgradeAuthority)]
    pub program_data: Account<'info, ProgramData>,

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
