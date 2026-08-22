use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
    pubkey::Pubkey,
};

use ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;

const SCHEDULE_TASK: u32 = 6;
const CANCEL_TASK: u32 = 7;

// The ER fires a new task once, ~immediately, ignoring the delay on the first fire; iterations=1
// makes that the only fire. Non-zero interval is required even so.
const INTERVAL_MS: u64 = 1000;

pub fn task_id(receipt: &Pubkey) -> u64 {
    u64::from_le_bytes(receipt.to_bytes()[..8].try_into().unwrap())
}

fn reap_ix(program_id: &Pubkey, disc: [u8; 8], receipt: &Pubkey, ledger: &Pubkey, ephemeral_vault: &Pubkey) -> Instruction {
    Instruction {
        program_id: *program_id,
        data: disc.to_vec(),
        accounts: vec![
            AccountMeta::new(*receipt, false),
            AccountMeta::new(*ledger, false),
            AccountMeta::new(*ephemeral_vault, false),
            AccountMeta::new_readonly(MAGIC_PROGRAM_ID, false),
        ],
    }
}

fn serialize_schedule(id: u64, ix: &Instruction) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SCHEDULE_TASK.to_le_bytes());
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&INTERVAL_MS.to_le_bytes());
    buf.extend_from_slice(&1u64.to_le_bytes()); // iterations
    buf.extend_from_slice(&1u64.to_le_bytes()); // one instruction
    buf.extend_from_slice(ix.program_id.as_ref());
    buf.extend_from_slice(&(ix.accounts.len() as u64).to_le_bytes());
    for m in &ix.accounts {
        buf.extend_from_slice(m.pubkey.as_ref());
        buf.push(m.is_signer as u8);
        buf.push(m.is_writable as u8);
    }
    buf.extend_from_slice(&(ix.data.len() as u64).to_le_bytes());
    buf.extend_from_slice(&ix.data);
    buf
}

fn ledger_seeds<'a>(owner: &'a Pubkey, bump: &'a [u8; 1]) -> [&'a [u8]; 3] {
    [b"ledger", owner.as_ref(), bump]
}

/// Schedules the one-shot reap that closes this receipt if settle never comes. The task authority
/// is the sponsor ledger — a game-and-vault-permissioned account, so a game-top-level tx may mark
/// it writable, and the vault can sign it. The task's own accounts are readonly here (the inner
/// reap ix carries their writable metas).
#[allow(clippy::too_many_arguments)]
pub fn schedule_reap<'a>(
    program_id: &Pubkey,
    reap_disc: [u8; 8],
    ledger: &AccountInfo<'a>,
    ledger_owner: &Pubkey,
    ledger_bump: u8,
    magic_context: &AccountInfo<'a>,
    magic_program: &AccountInfo<'a>,
    receipt: &AccountInfo<'a>,
    ephemeral_vault: &AccountInfo<'a>,
) -> ProgramResult {
    let inner = reap_ix(program_id, reap_disc, receipt.key, ledger.key, ephemeral_vault.key);
    let data = serialize_schedule(task_id(receipt.key), &inner);
    let ix = Instruction {
        program_id: MAGIC_PROGRAM_ID,
        data,
        accounts: vec![
            AccountMeta::new(*ledger.key, true),
            AccountMeta::new(*magic_context.key, false),
            AccountMeta::new_readonly(*receipt.key, false),
            AccountMeta::new_readonly(*ledger.key, false),
            AccountMeta::new_readonly(*ephemeral_vault.key, false),
            AccountMeta::new_readonly(*magic_program.key, false),
        ],
    };
    let bump = [ledger_bump];
    invoke_signed(
        &ix,
        &[ledger.clone(), magic_context.clone(), receipt.clone(), ephemeral_vault.clone(), magic_program.clone()],
        &[&ledger_seeds(ledger_owner, &bump)],
    )
}

pub fn cancel_reap<'a>(
    ledger: &AccountInfo<'a>,
    ledger_owner: &Pubkey,
    ledger_bump: u8,
    magic_context: &AccountInfo<'a>,
    receipt: &Pubkey,
) -> ProgramResult {
    let mut data = Vec::with_capacity(12);
    data.extend_from_slice(&CANCEL_TASK.to_le_bytes());
    data.extend_from_slice(&(task_id(receipt) as i64).to_le_bytes());
    let ix = Instruction {
        program_id: MAGIC_PROGRAM_ID,
        data,
        accounts: vec![
            AccountMeta::new(*ledger.key, true),
            AccountMeta::new(*magic_context.key, false),
        ],
    };
    let bump = [ledger_bump];
    invoke_signed(
        &ix,
        &[ledger.clone(), magic_context.clone()],
        &[&ledger_seeds(ledger_owner, &bump)],
    )
}
