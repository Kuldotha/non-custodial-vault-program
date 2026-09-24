use solana_program::{program_error::ProgramError, pubkey::Pubkey};

/// `sha256("account:Session")[..8]`, so the store reads as an Anchor account to any client.
pub const SESSION_DISCRIMINATOR: [u8; 8] = [243, 81, 72, 115, 214, 188, 72, 144];

/// Persisted keys a program's ring holds; the temporary slot is one more.
pub const RING: usize = 5;

/// 8 discriminator + owner + bump + pad + u32 entry count. Entries follow, one per program.
pub const SESSION_HEADER: usize = 8 + 32 + 1 + 3 + 4;

const O_OWNER: usize = 8;
const O_BUMP: usize = 40;
const O_COUNT: usize = 44;

const _: () = assert!(SESSION_HEADER == 48);
const _: () = assert!(Entry::SIZE == 232);

/// One program's keys: the temporary slot and the ring.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Entry {
    pub program: Pubkey,
    /// The one slot for a session the client will not keep: overwritten by the next such
    /// session, and dead on its own once `expires_at` has passed.
    pub temporary: Pubkey,
    pub expires_at: i64,
    /// Persisted keys, oldest first; a sixth pushes the oldest out.
    pub ring: [Pubkey; RING],
}

impl Entry {
    pub const SIZE: usize = 32 + 32 + 8 + RING * 32;

    pub fn new(program: Pubkey) -> Entry {
        Entry { program, temporary: Pubkey::default(), expires_at: 0, ring: [Pubkey::default(); RING] }
    }

    pub fn allows(&self, key: &Pubkey, now: i64) -> bool {
        (self.temporary == *key && now < self.expires_at) || self.ring.contains(key)
    }

    /// A key already held stays where it is; a new one takes the first free slot, or the
    /// oldest one's, the rest moving up.
    pub fn grant(&mut self, key: Pubkey) {
        if self.ring.contains(&key) {
            return;
        }
        if let Some(free) = self.ring.iter().position(|k| *k == Pubkey::default()) {
            self.ring[free] = key;
            return;
        }
        self.ring.rotate_left(1);
        self.ring[RING - 1] = key;
    }

    pub fn grant_temporary(&mut self, key: Pubkey, expires_at: i64) {
        self.temporary = key;
        self.expires_at = expires_at;
    }

    /// Forgets `key` wherever it sits, the ring closing up behind it.
    pub fn revoke(&mut self, key: &Pubkey) {
        if self.temporary == *key {
            self.temporary = Pubkey::default();
            self.expires_at = 0;
        }
        let kept: Vec<Pubkey> = self.ring.iter().copied().filter(|k| k != key && *k != Pubkey::default()).collect();
        self.ring = [Pubkey::default(); RING];
        self.ring[..kept.len()].copy_from_slice(&kept);
    }
}

/// `["session", owner]` — one per wallet, created empty with its ledger, never delegated. The
/// keys the wallet has let its game clients hold, one entry per program: a game mints its own
/// key and never shares it, so a key consents for the one program it was minted for, in
/// `settle` and `settle_receipt`. On a rollup the store is a read-only clone of the basenet
/// account, refreshed whenever a transaction touches it, so a key granted on basenet consents
/// on the rollup at once and the ledger never has to come home for it.
///
/// An entry is added, on basenet, the first time a program is authorised — the owner paying
/// its rent — and dropped, rent refunded, when the program's access is revoked. Nobody manages
/// the keys within: a client asks for its key once, and a device whose key was pushed out of
/// the ring simply asks again.
pub struct Session {
    pub owner: Pubkey,
    pub bump: u8,
    pub entries: Vec<Entry>,
}

impl Session {
    pub fn new(owner: Pubkey, bump: u8) -> Session {
        Session { owner, bump, entries: Vec::new() }
    }

    pub fn space(entries: usize) -> usize {
        SESSION_HEADER + entries * Entry::SIZE
    }

    pub fn entry(&self, program: &Pubkey) -> Option<&Entry> {
        self.entries.iter().find(|e| e.program == *program)
    }

    /// The program's entry, added if it has none yet. Returns whether it was added, since that
    /// grows the account.
    pub fn entry_mut(&mut self, program: Pubkey) -> (&mut Entry, bool) {
        if let Some(i) = self.entries.iter().position(|e| e.program == program) {
            return (&mut self.entries[i], false);
        }
        self.entries.push(Entry::new(program));
        let last = self.entries.len() - 1;
        (&mut self.entries[last], true)
    }

    /// Drops the program's entry. Returns whether there was one.
    pub fn drop_entry(&mut self, program: &Pubkey) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.program != *program);
        self.entries.len() != before
    }

    /// Whether `key` may consent for `program` at `now`.
    pub fn allows(&self, key: &Pubkey, program: &Pubkey, now: i64) -> bool {
        *key != Pubkey::default() && self.entry(program).is_some_and(|e| e.allows(key, now))
    }

    pub fn read_from(data: &[u8]) -> Result<Session, ProgramError> {
        if data.len() < SESSION_HEADER || data[0..8] != SESSION_DISCRIMINATOR {
            return Err(ProgramError::InvalidAccountData);
        }
        let key = |at: usize| Pubkey::new_from_array(data[at..at + 32].try_into().unwrap());
        let count = u32::from_le_bytes(data[O_COUNT..O_COUNT + 4].try_into().unwrap()) as usize;
        if data.len() < Self::space(count) {
            return Err(ProgramError::InvalidAccountData);
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let at = SESSION_HEADER + i * Entry::SIZE;
            let mut ring = [Pubkey::default(); RING];
            for (r, slot) in ring.iter_mut().enumerate() {
                *slot = key(at + 72 + r * 32);
            }
            entries.push(Entry {
                program: key(at),
                temporary: key(at + 32),
                expires_at: i64::from_le_bytes(data[at + 64..at + 72].try_into().unwrap()),
                ring,
            });
        }
        Ok(Session { owner: key(O_OWNER), bump: data[O_BUMP], entries })
    }

    /// Writes the store back. The account must already be `space(entries.len())` long.
    pub fn write_to(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() < Self::space(self.entries.len()) {
            return Err(ProgramError::AccountDataTooSmall);
        }
        data[0..8].copy_from_slice(&SESSION_DISCRIMINATOR);
        data[O_OWNER..O_OWNER + 32].copy_from_slice(self.owner.as_ref());
        data[O_BUMP] = self.bump;
        data[O_BUMP + 1..O_COUNT].fill(0);
        data[O_COUNT..O_COUNT + 4].copy_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (i, e) in self.entries.iter().enumerate() {
            let at = SESSION_HEADER + i * Entry::SIZE;
            data[at..at + 32].copy_from_slice(e.program.as_ref());
            data[at + 32..at + 64].copy_from_slice(e.temporary.as_ref());
            data[at + 64..at + 72].copy_from_slice(&e.expires_at.to_le_bytes());
            for (r, k) in e.ring.iter().enumerate() {
                let o = at + 72 + r * 32;
                data[o..o + 32].copy_from_slice(k.as_ref());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    #[test]
    fn a_granted_key_consents_for_its_program_only() {
        let mut s = Session::new(key(1), 255);
        s.entry_mut(key(10)).0.grant(key(20));
        assert!(s.allows(&key(20), &key(10), 0));
        assert!(!s.allows(&key(20), &key(11), 0));
        assert!(!s.allows(&key(21), &key(10), 0));
        assert!(!s.allows(&Pubkey::default(), &key(10), 0));
    }

    #[test]
    fn each_program_has_its_own_ring_and_the_sixth_key_pushes_out_the_first() {
        let mut s = Session::new(key(1), 255);
        for n in 20..25 {
            s.entry_mut(key(10)).0.grant(key(n));
        }
        s.entry_mut(key(11)).0.grant(key(20));
        assert_eq!(s.entries.len(), 2);
        s.entry_mut(key(10)).0.grant(key(21));
        assert_eq!(s.entry(&key(10)).unwrap().ring[1], key(21), "a key already held stays put");
        s.entry_mut(key(10)).0.grant(key(25));
        let e = s.entry(&key(10)).unwrap();
        assert!(!e.allows(&key(20), 0), "the oldest went");
        assert!(e.allows(&key(21), 0));
        assert_eq!(e.ring[RING - 1], key(25));
        assert!(s.allows(&key(20), &key(11), 0), "the other program's ring is untouched");
    }

    #[test]
    fn the_temporary_key_dies_at_its_expiry_and_a_revoke_closes_the_ring_up() {
        let mut s = Session::new(key(1), 255);
        let (e, _) = s.entry_mut(key(10));
        e.grant_temporary(key(30), 1_000);
        assert!(e.allows(&key(30), 999));
        assert!(!e.allows(&key(30), 1_000));
        e.grant(key(20));
        e.grant(key(21));
        e.revoke(&key(20));
        assert_eq!(e.ring[0], key(21));
        assert_eq!(e.ring[1], Pubkey::default());
        assert!(s.drop_entry(&key(10)));
        assert!(!s.drop_entry(&key(10)));
        assert!(!s.allows(&key(21), &key(10), 0));
    }

    #[test]
    fn the_bytes_round_trip() {
        let mut s = Session::new(key(1), 254);
        s.entry_mut(key(10)).0.grant(key(20));
        s.entry_mut(key(11)).0.grant_temporary(key(30), 77);
        let mut data = vec![0u8; Session::space(2)];
        s.write_to(&mut data).unwrap();
        let back = Session::read_from(&data).unwrap();
        assert_eq!(back.owner, key(1));
        assert_eq!(back.bump, 254);
        assert_eq!(back.entries, s.entries);
        assert_eq!(Session::space(2), 512);
    }
}
