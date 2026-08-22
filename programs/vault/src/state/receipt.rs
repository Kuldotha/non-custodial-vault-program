use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::error::VaultError;

/// The receipt's 8-byte account discriminator, `sha256("account:Receipt")[..8]`. The receipt is
/// written and read by hand (its length is variable and the ephemeral vault mints it), so
/// `settle_receipt` checks this itself — it is the only thing that stops another vault-owned
/// account, a `Ledger` above all, from being reinterpreted as a receipt and settled with no
/// consent. The runtime's zero-on-reassign rule means only `create_receipt` can ever place it in
/// a vault-owned account.
pub const RECEIPT_DISCRIMINATOR: [u8; 8] = [39, 154, 73, 106, 80, 102, 145, 153];

pub const RECEIPT_HEADER: usize = 155;
pub const MOVEMENT_SIZE: usize = 32 + 8 + 1 + 1;
pub const OWNER_SIZE: usize = 32;

const O_DISCRIMINATOR: usize = 0;
/// The consent key that authored this receipt — the receipt's PDA is seeded by it and it signs
/// creation, so a receipt is unforgeably its consenter's. Settle checks it against the human
/// ledger's owner/authorized.
const O_CONSENTER: usize = 8;
const O_OWNER0: usize = 40;
const O_AUTHORITY: usize = 72;
const O_MEMBER: usize = 104;
const O_CALLBACK_DISC: usize = 136;
const O_SLOT: usize = 144;
const O_LEDGERS: usize = 152;
const O_COUNT: usize = 153;
const O_ARGS_LEN: usize = 154;

/// One value movement in a receipt. `from`/`to` index the receipt's owner list.
#[derive(BorshDeserialize, BorshSerialize, Clone)]
pub struct Movement {
    pub mint: Pubkey,
    pub amount: u64,
    pub from: u8,
    pub to: u8,
}

pub fn len_for(owners: usize, movements: usize, args: usize) -> usize {
    RECEIPT_HEADER + (owners - 1) * OWNER_SIZE + movements * MOVEMENT_SIZE + args
}

/// The `created` slot of an existing receipt, read without a full parse (used by `create_receipt`
/// to tell a live same-slot receipt from settle-able debris).
pub fn slot_of(data: &[u8]) -> u64 {
    u64::from_le_bytes(data[O_SLOT..O_SLOT + 8].try_into().unwrap())
}

pub fn authority_of(data: &[u8]) -> Pubkey {
    Pubkey::new_from_array(data[O_AUTHORITY..O_AUTHORITY + 32].try_into().unwrap())
}

/// Writes a receipt's bytes exactly. `data` must be `len_for(owners.len(), movements.len(),
/// args.len())` long.
#[allow(clippy::too_many_arguments)]
pub fn write(
    data: &mut [u8],
    consenter: &Pubkey,
    owners: &[Pubkey],
    authority: &Pubkey,
    member: &Pubkey,
    callback_disc: &[u8; 8],
    slot: u64,
    movements: &[Movement],
    args: &[u8],
) {
    data[O_DISCRIMINATOR..O_DISCRIMINATOR + 8].copy_from_slice(&RECEIPT_DISCRIMINATOR);
    data[O_CONSENTER..O_CONSENTER + 32].copy_from_slice(consenter.as_ref());
    data[O_OWNER0..O_OWNER0 + 32].copy_from_slice(owners[0].as_ref());
    data[O_AUTHORITY..O_AUTHORITY + 32].copy_from_slice(authority.as_ref());
    data[O_MEMBER..O_MEMBER + 32].copy_from_slice(member.as_ref());
    data[O_CALLBACK_DISC..O_CALLBACK_DISC + 8].copy_from_slice(callback_disc);
    data[O_SLOT..O_SLOT + 8].copy_from_slice(&slot.to_le_bytes());
    data[O_LEDGERS] = owners.len() as u8;
    data[O_COUNT] = movements.len() as u8;
    data[O_ARGS_LEN] = args.len() as u8;
    for (i, o) in owners.iter().skip(1).enumerate() {
        let at = RECEIPT_HEADER + i * OWNER_SIZE;
        data[at..at + 32].copy_from_slice(o.as_ref());
    }
    let mv = RECEIPT_HEADER + (owners.len() - 1) * OWNER_SIZE;
    for (i, m) in movements.iter().enumerate() {
        let o = mv + i * MOVEMENT_SIZE;
        data[o..o + 32].copy_from_slice(m.mint.as_ref());
        data[o + 32..o + 40].copy_from_slice(&m.amount.to_le_bytes());
        data[o + 40] = m.from;
        data[o + 41] = m.to;
    }
    let ao = mv + movements.len() * MOVEMENT_SIZE;
    data[ao..ao + args.len()].copy_from_slice(args);
}

/// A parsed receipt, as `settle_receipt` reads it.
pub struct Receipt {
    pub consenter: Pubkey,
    pub owners: Vec<Pubkey>,
    pub authority: Pubkey,
    pub member: Pubkey,
    pub callback_disc: [u8; 8],
    pub slot: u64,
    pub movements: Vec<Movement>,
    pub args: Vec<u8>,
}

/// Reads and validates a receipt from raw bytes. Checks the discriminator first — the whole
/// defense against a non-receipt account passed in the receipt slot.
pub fn read(data: &[u8]) -> Result<Receipt, ProgramError> {
    if data.len() < RECEIPT_HEADER {
        return Err(VaultError::NoAuthorization.into());
    }
    if data[O_DISCRIMINATOR..O_DISCRIMINATOR + 8] != RECEIPT_DISCRIMINATOR {
        return Err(VaultError::NotAReceipt.into());
    }
    let n = data[O_LEDGERS] as usize;
    let count = data[O_COUNT] as usize;
    let args_len = data[O_ARGS_LEN] as usize;
    if n < 1 {
        return Err(VaultError::NoAuthorization.into());
    }
    let mv = RECEIPT_HEADER + (n - 1) * OWNER_SIZE;
    if data.len() < mv + count * MOVEMENT_SIZE + args_len {
        return Err(VaultError::NoAuthorization.into());
    }

    let mut owners = vec![Pubkey::new_from_array(data[O_OWNER0..O_OWNER0 + 32].try_into().unwrap())];
    for i in 0..n - 1 {
        let at = RECEIPT_HEADER + i * OWNER_SIZE;
        owners.push(Pubkey::new_from_array(data[at..at + 32].try_into().unwrap()));
    }
    let mut movements = Vec::with_capacity(count);
    for i in 0..count {
        let o = mv + i * MOVEMENT_SIZE;
        let (from, to) = (data[o + 40], data[o + 41]);
        // Only create_receipt can place a receipt in a vault-owned account, and it bounds these —
        // but a parser that indexes owners must not trust that from afar.
        if from as usize >= n || to as usize >= n {
            return Err(VaultError::NoAuthorization.into());
        }
        movements.push(Movement {
            mint: Pubkey::new_from_array(data[o..o + 32].try_into().unwrap()),
            amount: u64::from_le_bytes(data[o + 32..o + 40].try_into().unwrap()),
            from,
            to,
        });
    }
    let ao = mv + count * MOVEMENT_SIZE;
    Ok(Receipt {
        consenter: Pubkey::new_from_array(data[O_CONSENTER..O_CONSENTER + 32].try_into().unwrap()),
        owners,
        authority: Pubkey::new_from_array(data[O_AUTHORITY..O_AUTHORITY + 32].try_into().unwrap()),
        member: Pubkey::new_from_array(data[O_MEMBER..O_MEMBER + 32].try_into().unwrap()),
        callback_disc: data[O_CALLBACK_DISC..O_CALLBACK_DISC + 8].try_into().unwrap(),
        slot: u64::from_le_bytes(data[O_SLOT..O_SLOT + 8].try_into().unwrap()),
        movements,
        args: data[ao..ao + args_len].to_vec(),
    })
}
