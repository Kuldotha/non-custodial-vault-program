use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};

use crate::constants::{ENTRY_SIZE, LEDGER_DISCRIMINATOR, SOL_MINT};
use crate::error::VaultError;

/// 8 discriminator + owner + pda_auth + bump + pad + rent_payer + authorized + u32 entry count.
/// Byte-identical to the Anchor `Ledger`: the account discriminator, then borsh's field order,
/// with the `Vec<Entry>` length written as a 4-byte little-endian prefix. Kept exact so a
/// native-written ledger is indistinguishable from an Anchor-written one — member programs index
/// into these bytes (e.g. slot-0 SOL at offset 148 = 116 + 32).
pub const HEADER: usize = 8 + 32 + 1 + 1 + 6 + 32 + 32 + 4;

const _: () = assert!(HEADER == 116);

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Entry {
    pub mint: Pubkey,
    pub amount: u64,
}

impl Default for Entry {
    fn default() -> Self {
        // A default (all-zero) mint is `SOL_MINT`; at index 0 that means SOL, elsewhere a free slot.
        Entry { mint: SOL_MINT, amount: 0 }
    }
}

/// `["ledger", owner]` — one per owner, human or program. Holds claims, never value.
pub struct Ledger {
    pub owner: Pubkey,
    /// true when `owner` is off-curve, i.e. a program's PDA.
    pub pda_auth: bool,
    pub bump: u8,
    /// Where the rent goes on close, and the only key that may grow it — the owner for a wallet
    /// ledger, the sponsor for a PDA's.
    pub rent_payer: Pubkey,
    /// Wallet: a session key that may consent to debits, or zero. PDA: the member program.
    pub authorized: Pubkey,
    /// Pre-allocated slots; `entries.len()` is the capacity.
    pub entries: Vec<Entry>,
}

impl Ledger {
    pub fn space(slots: usize) -> usize {
        HEADER + slots * ENTRY_SIZE
    }

    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    /// A fresh ledger: every slot present and zeroed, slot 0 claimed by SOL.
    pub fn new(owner: Pubkey, pda_auth: bool, bump: u8, rent_payer: Pubkey, slots: usize) -> Ledger {
        Ledger {
            owner,
            pda_auth,
            bump,
            rent_payer,
            authorized: Pubkey::default(),
            entries: vec![Entry::default(); slots],
        }
    }

    pub fn free_slots(&self) -> usize {
        self.entries
            .iter()
            .enumerate()
            .filter(|(i, e)| *i != 0 && e.mint == SOL_MINT)
            .count()
    }

    /// Whether `who` may end this ledger's rollup session.
    pub fn may_end_session(&self, who: &Pubkey) -> bool {
        *who == self.owner
            || (!self.pda_auth && self.authorized != Pubkey::default() && *who == self.authorized)
    }

    /// Index of the entry for `mint`, or None. SOL is positional — never a scan.
    pub fn index_of(&self, mint: &Pubkey) -> Option<usize> {
        if *mint == SOL_MINT {
            return Some(0);
        }
        self.entries.iter().position(|e| e.mint == *mint)
    }

    /// Index for `mint`, claiming a free slot if it has none yet.
    pub fn index_or_claim(&mut self, mint: &Pubkey) -> Result<usize, ProgramError> {
        if let Some(i) = self.index_of(mint) {
            return Ok(i);
        }
        let free = self
            .entries
            .iter()
            .enumerate()
            .position(|(i, e)| i != 0 && e.mint == SOL_MINT)
            .ok_or(VaultError::LedgerFull)?;
        self.entries[free].mint = *mint;
        Ok(free)
    }

    pub fn index_for_credit(&mut self, mint: &Pubkey, may_claim: bool) -> Result<usize, ProgramError> {
        if let Some(i) = self.index_of(mint) {
            return Ok(i);
        }
        if !may_claim {
            return Err(VaultError::SlotClaimNeedsConsent.into());
        }
        self.index_or_claim(mint)
    }

    /// Debits an entry, releasing its slot at zero (a released slot is what keeps a credit from
    /// filling a ledger with dust of mints its owner never asked for).
    pub fn debit(&mut self, index: usize, amount: u64) -> Result<(), ProgramError> {
        let entry = &mut self.entries[index];
        entry.amount = entry.amount.checked_sub(amount).ok_or(VaultError::Insufficient)?;
        if entry.amount == 0 && index != 0 {
            entry.mint = SOL_MINT;
        }
        Ok(())
    }

    pub fn credit(&mut self, index: usize, amount: u64) -> Result<(), ProgramError> {
        let entry = &mut self.entries[index];
        entry.amount = entry.amount.checked_add(amount).ok_or(VaultError::Overflow)?;
        Ok(())
    }

    /// Reads a ledger out of raw account bytes, checking the discriminator (Anchor's `try_deserialize`
    /// equivalent). Program ownership is the caller's check, matching each handler's needs.
    pub fn read_from(data: &[u8]) -> Result<Ledger, ProgramError> {
        if data.len() < HEADER || data[0..8] != LEDGER_DISCRIMINATOR {
            return Err(ProgramError::InvalidAccountData);
        }
        let owner = Pubkey::new_from_array(data[8..40].try_into().unwrap());
        let pda_auth = data[40] != 0;
        let bump = data[41];
        let rent_payer = Pubkey::new_from_array(data[48..80].try_into().unwrap());
        let authorized = Pubkey::new_from_array(data[80..112].try_into().unwrap());
        let count = u32::from_le_bytes(data[112..116].try_into().unwrap()) as usize;
        if data.len() < HEADER + count * ENTRY_SIZE {
            return Err(ProgramError::InvalidAccountData);
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let o = HEADER + i * ENTRY_SIZE;
            entries.push(Entry {
                mint: Pubkey::new_from_array(data[o..o + 32].try_into().unwrap()),
                amount: u64::from_le_bytes(data[o + 32..o + 40].try_into().unwrap()),
            });
        }
        Ok(Ledger { owner, pda_auth, bump, rent_payer, authorized, entries })
    }

    /// Writes the ledger back, byte-for-byte as Anchor would. The account must already be large
    /// enough — grow first.
    pub fn write_to(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() < HEADER + self.entries.len() * ENTRY_SIZE {
            return Err(ProgramError::AccountDataTooSmall);
        }
        data[0..8].copy_from_slice(&LEDGER_DISCRIMINATOR);
        data[8..40].copy_from_slice(self.owner.as_ref());
        data[40] = self.pda_auth as u8;
        data[41] = self.bump;
        data[42..48].fill(0);
        data[48..80].copy_from_slice(self.rent_payer.as_ref());
        data[80..112].copy_from_slice(self.authorized.as_ref());
        data[112..116].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (i, e) in self.entries.iter().enumerate() {
            let o = HEADER + i * ENTRY_SIZE;
            data[o..o + 32].copy_from_slice(e.mint.as_ref());
            data[o + 32..o + 40].copy_from_slice(&e.amount.to_le_bytes());
        }
        Ok(())
    }

    pub fn load(info: &AccountInfo) -> Result<Ledger, ProgramError> {
        Ledger::read_from(&info.try_borrow_data()?)
    }

    /// Loads a ledger and pins it: program-owned, valid discriminator, and the account address is
    /// the PDA of its own stored `(owner, bump)`. The equivalent of Anchor's `Account<Ledger>`
    /// seeds check, for the paths where the owner comes from the account rather than a signer.
    pub fn load_checked(info: &AccountInfo, program_id: &Pubkey) -> Result<Ledger, ProgramError> {
        if info.owner != program_id {
            return Err(VaultError::BadLedgerOwner.into());
        }
        let l = Ledger::read_from(&info.try_borrow_data()?)?;
        let derived =
            Pubkey::create_program_address(&[b"ledger", l.owner.as_ref(), &[l.bump]], program_id)
                .map_err(|_| ProgramError::from(VaultError::BadLedgerOwner))?;
        if derived != *info.key {
            return Err(VaultError::BadLedgerOwner.into());
        }
        Ok(l)
    }

    pub fn store(&self, info: &AccountInfo) -> Result<(), ProgramError> {
        self.write_to(&mut info.try_borrow_mut_data()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_exact_anchor_layout() {
        let owner = Pubkey::new_from_array([7u8; 32]);
        let rent_payer = Pubkey::new_from_array([9u8; 32]);
        let mut l = Ledger::new(owner, false, 254, rent_payer, 4);
        l.authorized = Pubkey::new_from_array([5u8; 32]);
        l.entries[0].amount = 12_345; // SOL in slot 0
        l.entries[1] = Entry { mint: Pubkey::new_from_array([2u8; 32]), amount: 77 };

        let mut buf = vec![0u8; Ledger::space(4)];
        l.write_to(&mut buf).unwrap();

        assert_eq!(&buf[0..8], &LEDGER_DISCRIMINATOR);
        assert_eq!(&buf[8..40], owner.as_ref());
        assert_eq!(buf[40], 0); // pda_auth
        assert_eq!(buf[41], 254); // bump
        assert_eq!(&buf[42..48], &[0u8; 6]); // pad
        assert_eq!(&buf[48..80], rent_payer.as_ref());
        assert_eq!(&buf[80..112], &[5u8; 32]); // authorized
        assert_eq!(u32::from_le_bytes(buf[112..116].try_into().unwrap()), 4);
        // slot 0: zero mint, amount at 148 — the member program's SOL_AMOUNT_AT.
        assert_eq!(&buf[116..148], &[0u8; 32]);
        assert_eq!(u64::from_le_bytes(buf[148..156].try_into().unwrap()), 12_345);
        // slot 1 begins at 156.
        assert_eq!(&buf[156..188], &[2u8; 32]);
        assert_eq!(u64::from_le_bytes(buf[188..196].try_into().unwrap()), 77);
        assert_eq!(Ledger::space(4), 116 + 4 * 40);

        let back = Ledger::read_from(&buf).unwrap();
        assert_eq!(back.owner, owner);
        assert_eq!(back.bump, 254);
        assert_eq!(back.rent_payer, rent_payer);
        assert_eq!(back.authorized, l.authorized);
        assert_eq!(back.entries.len(), 4);
        assert_eq!(back.entries[0].amount, 12_345);
        assert_eq!(back.entries[1], l.entries[1]);
    }

    #[test]
    fn debit_releases_slot_but_keeps_sol() {
        let mut l = Ledger::new(Pubkey::default(), false, 1, Pubkey::default(), 4);
        let m = Pubkey::new_from_array([3u8; 32]);
        let i = l.index_or_claim(&m).unwrap();
        l.credit(i, 10).unwrap();
        l.debit(i, 10).unwrap();
        assert_eq!(l.entries[i].mint, SOL_MINT); // released
        // slot 0 never releases
        l.credit(0, 5).unwrap();
        l.debit(0, 5).unwrap();
        assert_eq!(l.index_of(&SOL_MINT), Some(0));
    }
}
