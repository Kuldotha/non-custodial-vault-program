# Vault Program — Design Specification

A minimal, immutable, multi-mint ledger primitive for Solana, designed to be delegated to
a MagicBlock ephemeral rollup and CPI'd into by game programs.

---

## 1. Purpose and non-goals

### Purpose

Hold SOL and SPL tokens on behalf of users and programs, tracking balances in a **ledger**
account that can be delegated to an ephemeral rollup. This lets a game mutate balances at
rollup speed (and, in a private rollup, confidentially) while the basenet boundary — value in
and value out — remains public, immutable, and attributable.

### Explicit non-goals

This program is **not** a payment primitive. It must be structurally incapable of moving
value between two humans. This is not a policy preference or a documentation note — it is
enforced in the instruction logic and is the single most important property of the design.

Also out of scope: yield, fees, admin authority, upgrade authority, front-end, hosted
service of any kind.

### Design constraints (non-negotiable)

| Constraint | Rationale |
|---|---|
| No upgrade authority (burned post-deploy) | Immutable = non-custodial. Nobody, including the deployer, can move user funds. |
| No fee of any kind | Keeps the program a library rather than a service. |
| No admin/authority account | Nothing to compromise, nothing to regulate. |
| `deposit` / `withdraw` are same-owner-only | The basenet boundary can never move value between people. |
| `settle` requires exactly one program side and one human side | Makes user→user transfer unreachable by any instruction sequence. |
| Totals are conservative across every operation | `settle` moves value; it never creates or destroys it. |

---

### Terminology

The ephemeral rollup is a **rollup**, not an L2. The settlement chain it commits to is
called **basenet** throughout this document and in the code.

---

## 2. Account model

Two distinct things, with two distinct names. Conflating them is what makes this design
hard to talk about:

- **Vault** — the singleton that *holds value*.
- **Ledger** — one per owner, recording *who is owed what*.

### 2.1 The vault (reserves)

```
["vault"]                 — singleton PDA. Holds all deposited SOL, and is the SPL
                            authority for every token account below.
ATA(["vault"], mint)      — one SPL token account per mint, authority = ["vault"].
```

The vault holds every lamport and every token the program has ever taken in. Ledgers hold
no value at all — only claims.

**The program never creates the token accounts.** They are the vault PDA's *associated*
token accounts, created by the caller with the ATA program's `create_idempotent` in the
same transaction. This is deliberate: authority over a token account comes from the `owner`
field inside it, never from how its address was derived, so a program-owned derivation
would buy nothing but an obligation to carry creation code — in a program that can never be
patched. The vault still asserts the canonical ATA address, so the reserve for a mint is
exactly one pool and deposits and withdrawals can never hit different ones.

The vault PDA's own rent-exempt minimum is not part of anyone's balance. It is carved out
of the SOL invariant (§2.4), and must be funded by `initialize_vault` before any SOL path
runs — never from a deposit, or the last lamports credited could not be withdrawn.

### 2.2 The ledger (claims)

One account per owner — human *or* program — holding balances for **many mints**.
Rationale: one delegation per player, not one per mint.

```rust
pub const SOL_MINT:      Pubkey = Pubkey::default();  // the System Program: it controls lamports
pub const INITIAL_SLOTS: u8 = 32;
pub const GROW_STEP:     u8 = 16;
pub const MIN_FREE:      u8 = 16;

pub struct Entry {
    pub mint:   Pubkey,   // 32
    pub amount: u64,      //  8
}                         // 40 bytes

pub struct Ledger {
    pub owner:    Pubkey,     // 32 — the seed, stored for cheap reads
    pub pda_auth: bool,       //  1 — true when `owner` is off-curve (a program's PDA)
    pub bump:     u8,         //  1
    pub _pad:     [u8; 6],    //  6
    pub entries:  Vec<Entry>, //  4 + 40 * capacity
}
```

**Slots are pre-allocated.** A ledger is created with `INITIAL_SLOTS` entries already
present, all zeroed apart from slot 0. This is what allows a new mint to be added to a
ledger *inside the rollup*: reallocation of a delegated account is not possible, so the
space must already be there. `capacity` is simply `entries.len()`; there is no separate
occupancy counter to fall out of sync. A slot is free when `mint == SOL_MINT && index != 0`.

### 2.3 Slot 0 is SOL

`entries[0]` is always the native SOL entry, keyed by `SOL_MINT` (the System Program —
the program that actually controls lamports). It is written at creation whether or not the
first deposit is SOL.

Reserving it *positionally* rather than by sentinel search is what makes the zero-mint
unambiguous: a zero mint at index 0 means SOL, a zero mint anywhere else means an unused
slot. SOL lookup is `entries[0]`, never a scan.

### 2.4 Invariants

Two lines, the same shape twice:

```
vault.lamports − rent_exempt(0)   >=   Σ over all ledgers of entries[0].amount
tokens(M).amount                  >=   Σ over all ledgers of entry(M).amount
```

`settle` preserves both trivially — it only moves between entries and never touches
the reserves. `deposit` and `withdraw` move both sides together atomically.

### 2.5 Ledger derivation and the discriminator

```
["ledger", owner_pubkey]
```

One derivation, for humans and programs alike. The discriminator is the **curve** of the
seed:

- **On-curve** — a real keypair exists; a human signs with it. `pda_auth = false`.
- **Off-curve** — a PDA; only the program that derives it can ever `invoke_signed` for it.
  `pda_auth = true`.

A human can never sign for an off-curve address (no private key exists), and
`find_program_address` never returns an on-curve address, so a program can never sign for
a human's. The two populations cannot overlap.

`pda_auth` is computed once at creation with `bytes_are_curve_point` and stored. Storing
is safe here — unlike a check on mutable account state — because the owner is fixed by the
PDA derivation and re-validated on every use, so the property can never drift.

A program therefore does not use its program ID as the ledger owner (it cannot sign for
that). It creates a **vault-authority** PDA of its own — e.g.
`find_program_address([b"vault_authority"], GAME_ID)` — and that PDA is the ledger owner.
The vault program needs no knowledge of the caller's seeds: it only checks that the owner
signed and that the ledger derives from it.

---

## 3. Instructions

Seven, total:

| Instruction | Who calls it |
|---|---|
| `initialize_vault` | the deployer, once, before burning the upgrade authority |
| `deposit` | anyone, for their own ledger |
| `withdraw` | the ledger owner |
| `settle` | a program, mediating |
| `close_ledger` | the ledger owner |
| `delegate_ledger` / `undelegate` | a program running a session |

Everything else that could have been an instruction is folded into `deposit`, because each
one existed only to put an account into a state `deposit` already has to produce. Creating a
ledger, creating its permission and growing it are **not** callable from outside. That is a
safety property, not just a smaller API: a ledger cannot exist in a state no instruction
produced, and cannot exist without the permission that keeps it unreadable in a rollup.

There is **one** deposit and **one** withdraw. The `mint` argument is the asset selector,
including for SOL — `SOL_MINT` is a legitimate input, not a sentinel to reject. When it is
passed, the token-account slots carry the System Program as a placeholder and are never
read.

### 3.1 `initialize_vault()`

Funds `["vault"]` to `rent_exempt(0)`. Restricted to the program's **upgrade authority**,
because it is a deployment step rather than a user one, and idempotent.

The vault's rent cannot come out of a deposit — the last lamports credited would be
unwithdrawable — so it must be in place before any SOL path runs. Every SOL path refuses an
unfunded vault with `VaultNotInitialized`.

> **Ordering hazard.** Once the upgrade authority is burned, `upgrade_authority_address` is
> `None` and no signer can satisfy the constraint again. If the vault is not funded before
> the burn, the SOL path is permanently unusable. Run it immediately after deploy (§7).

### 3.2 `deposit(mint, amount, min_free, slot_increase)`

```
1. Derive ["ledger", signer.key]; assert the passed account matches.
2. If uninitialised, create the ledger: INITIAL_SLOTS entries, slot 0 SOL, pda_auth
   from the owner's curve.
3. If the permission account is empty, create it (§3.6).
4. If mint == SOL_MINT:
     assert vault.lamports >= rent_exempt(0)                (§3.1)
     entry = &mut entries[0]
     System transfer `amount` from signer → ["vault"]      (only the wallet can debit itself)
   else:
     assert token_account.mint == mint
     entry = find_or_place(mint)                            (error if no free slot)
     SPL transfer `amount` from signer's token account → ATA(["vault"], mint)
5. entry.amount = entry.amount.checked_add(amount)?
6. ensure_headroom(min_free, slot_increase)                 (§3.5)
```

The ledger account is created **by hand**, not with Anchor's `init_if_needed`. That
constraint re-evaluates its `space` expression against the account's real length on *every*
call, not only at creation — so the first time `ensure_headroom` grows a ledger, `space` and
the actual size disagree permanently and every later deposit reverts with `ConstraintSpace`.
Growth and `init_if_needed` cannot coexist, and the constraint offers no way to say "size it
on creation, ignore it afterwards". `deposit` therefore creates, loads, and stores the
ledger itself.

The destination is derived, never passed as an argument. The signer must be the ledger
owner: there are no third-party deposits.

### 3.3 `withdraw(mint, amount)`

```
1. Derive ["ledger", signer.key]; assert match; assert signer.is_signer.
2. entry = SOL_MINT ? entries[0] : find(mint)               (error if absent)
3. entry.amount = entry.amount.checked_sub(amount)?
4. If mint == SOL_MINT:
     assert vault.lamports − rent_exempt(0) >= amount       // physical check
     System transfer `amount` from ["vault"] → signer       (signed with vault seeds)
   else:
     assert tokens(mint).amount >= amount                   // physical check
     SPL transfer `amount` from ATA(["vault"], mint) → signer's associated token account
5. Never prune a zero entry. Slots stay claimed; a delegated ledger cannot grow one back.
```

The destination is derived from the signer, never passed. This is what makes withdrawal
same-owner-only. Program ledgers withdraw through the same instruction, signed by
`invoke_signed` for their vault-authority, and the destination derives from that PDA.

### 3.4 `settle(mint, amount)` — the only cross-account movement

```rust
// accounts: [src_ledger, dst_ledger, src_authority, dst_authority]

// 1. Both ledgers initialised, derivations verified against their authorities.
// 2. XOR: exactly one side is a program.
require!(src.pda_auth != dst.pda_auth, NotProgramMediated);

// 3. The program side must sign (invoke_signed with its own PDA seeds).
let prog_auth = if src.pda_auth { src_authority } else { dst_authority };
require!(prog_auth.is_signer, MissingProgramSignature);

// 4. The human side signs ONLY when being debited.
if !src.pda_auth {
    require!(src_authority.is_signer, MissingUserSignature);
}

// 5. Move value. The reserves are untouched; this is pure bookkeeping.
let s = src.entry_mut(mint).ok_or(NoBalance)?;
s.amount = s.amount.checked_sub(amount).ok_or(Insufficient)?;
let d = dst.entry_or_place(mint)?;              // fails if dst has no free slot
d.amount = d.amount.checked_add(amount).ok_or(Overflow)?;
```

`settle` never learns that SOL is special: slot 0 is found by the same lookup as any other
mint, and no reserve account is involved either way.

#### Why the XOR matters

| src | dst | Result |
|---|---|---|
| human | program | ✅ user pays an entry fee / buys something |
| program | human | ✅ game pays out a prize |
| human | human | ❌ **rejected** — this is the whole point |
| program | program | ❌ rejected — no chaining between games |

Alice and Bob both have `pda_auth = false`. `settle(alice → bob)` fails at step 2, before
touching any balance. There is **no instruction sequence in this program** that moves value
from one person to another.

#### Why the signer asymmetry matters

A user must sign to be *debited* but not to be *credited*. So a game can push payouts to
winners who are offline, and can never pull from a user who did not authorise it.

#### Attack: fake authority

Someone creates a ledger whose owner is a PDA they do not control, hoping to mislabel it.
They cannot sign for it — only the deriving program can. They have locked their own tokens
in an account only a third party can move. Self-defeating.

#### Residual case (accepted)

Alice deploys a two-instruction forwarding program. Now `alice → AliceProgram → bob` works.
This is unavoidable at the primitive level and is correct: Alice deployed it, Alice
operates it, the obligations are hers. What matters is that **this program cannot be used
as a payment rail directly** — anyone who wants one must publish their own.

### 3.5 Growth — not an instruction

Every `deposit` keeps free slots in the band `MIN_FREE..INITIAL_SLOTS`: a ledger starts with
32, and when free drops below `min_free` it gains `slot_increase` more, rent funded by the
depositor. basenet only — a delegated ledger cannot realloc, which is the whole reason the
slots are pre-allocated.

Because `settle` fails rather than growing, the band is what guarantees a session never hits
the wall mid-play.

A program's ledger holds every mint it ever pays out, so it needs far more than 32. It
reaches its working size the same way it was created: repeated zero deposits with a high
`min_free`, each adding up to `slot_increase` entries. Setup-time cost, paid once.

### 3.6 The permission — not an instruction

Created inside `deposit` on first use, with exactly one member — **the owner, at flags 0** —
so the owner can read their own ledger inside a private rollup and nobody else can. The
ledger signs for itself with its own seeds.

> **`Some(...)`, never `None`.** `MembersArgs.members` is an `Option`, and the two values mean
> opposite things. `None` is *no ACL at all* — the account stays readable by anyone. Any
> `Some` restricts it. Verified on the TEE validator: with `None` the ledger was served in the
> clear to an anonymous caller. There is no error and no warning between the two — only the
> behaviour.

> **Membership is the read gate; no flag grants it.** The flag set is `AUTHORITY` (1),
> `TX_LOGS` (2), `TX_BALANCES` (4), `TX_MESSAGE` (8), `ACCOUNT_SIGNATURES` (16) — every one of
> them is about transaction-level visibility. Verified: a member at **flags 0** reads the
> account fine, while an anonymous caller and a non-member holding a valid token both get
> nothing.

**`AUTHORITY` is deliberately withheld from the owner.** It would let them rewrite their own
ACL and expose a ledger this program is meant to keep private. In code that can never be
patched that has to be impossible rather than discouraged, so every ledger's privacy is fixed
at creation and no instruction can widen it.

Note that the `private` flag visible on `EphemeralPermission` has no counterpart on the
basenet `Permission` struct. Privacy on basenet is expressed purely through the member list.

This is deliberately the **basenet** permission, not the ephemeral one. An ephemeral
permission is created inside the rollup, after the account is already delegated and live
there, which leaves a window in which the ledger is readable. Creating it on basenet
before delegation closes that window entirely.

The permission is never delegated and has no commit lifecycle — the rollup copies the
basenet permission data when it needs it.

`delegate_ledger` **refuses** unless the permission account already exists and is owned by
the permission program. Since the only way to create a ledger also creates its permission,
that check is now a backstop rather than the thing holding the ordering together.

### 3.7 `close_ledger()`

Sweeps every balance back to the owner and closes both the ledger and its permission
account, refunding all rent.

Each non-zero token entry needs its `(vault_token, owner_token)` pair in
`remaining_accounts`, in entry order. The instruction asserts that **every** entry is zero
before closing, so an incomplete account list can never strand value in the vault.

Basenet only: a delegated ledger is owned by the delegation program, so the ownership
check rejects it before anything runs.

### 3.8 Delegation

- `delegate_ledger(validator)` — basenet. Hands `["ledger", owner]` to the delegation program.
- `undelegate()` — rollup-side. Ends the session; the commit is implicit in undelegating, so
  there is no separate commit instruction to forget.
- `undelegate_ledger()` — the fixed-discriminator callback the delegation program invokes.

Reserves are never delegated: `["vault"]` and `ATA(["vault"], mint)` stay on basenet permanently.
That is precisely why `settle` can be pure bookkeeping.

### 3.9 What privacy actually covers

Confirmed against the TEE validator `MTEWGuqxUpYZGFJQcp8tLN7x5v9BSeoFHYWQQ3n3xzo`
(`devnet-tee.magicblock.app`, which requires a signed-challenge token — see
`scripts/tee-auth.mjs`):

- a delegated ledger is **not** served to an anonymous caller;
- nor to a caller holding a valid token who is not a member;
- but **is** served to its owner, the permission's sole member, at flags 0;
- a public rollup will still clone the account, but only ever mirrors the basenet copy
  byte for byte — it holds no session state.

**The boundary is the delegation point, not the account.** Delegation freezes the basenet
copy in the clear, so the balance a ledger *entered* the session with stays public forever.
Privacy covers what changes **during** the session. A game that wants an opening balance
private has to keep it off basenet in the first place.

Not yet proven: that an in-session mutation stays private. Demonstrating it needs a
`settle` inside the session, which needs a program-side signer — so it waits on a test game
program. The two read paths above are proven; the mutation path is inferred from them.

---

## 4. Delegation model

### 4.1 Roles

| Account | Delegation lifetime |
|---|---|
| Game ledger | Delegated for the whole session/season. Anchor. |
| Player ledgers | Delegated at join, undelegated at exit. Transient. |

All player ledgers **must delegate to the same validator as the game ledger**, because both
sides of a `settle` CPI must be in the same rollup. Joining a session is therefore
"delegate to wherever the game ledger is."

### 4.2 Commit strategy

**During a session, `settle` commits nothing.** Both sides stay delegated; balances mutate
freely in the rollup.

**Player exit:** commit + undelegate that player's ledger only. The game ledger stays
delegated and its basenet entry is now stale — safe, see §4.3.

**Season end:** the game ledger commits and undelegates once. No reconciliation needed —
every player already undelegated with a correct balance.

### 4.3 Staleness only ever blocks the stale party

`settle` is conservative: the sum across all ledgers is invariant, so §2.4 always holds in
aggregate. Individual basenet entries can be stale, and the consequences are asymmetric:

- **User → program (entry fees).** The player's basenet balance drops, the game's basenet entry is
  stale-low. The reserves hold *more* than the basenet entries claim. Conservative.
- **Program → user (payouts).** The player undelegates with their winnings while the game's
  stale entry still shows the pre-payout figure. The sum of basenet entries now exceeds the reserves
  by exactly the game's uncommitted delta.

The second case is safe without paired commits, because the over-claim is confined to the
*game's own* entry. The reserves still hold the true total, so every user can withdraw their
true balance; the only account that can hit the physical check in §3.3 is the game's, and
it clears the moment the game commits. **Committing on undelegation alone is sufficient.**
The cost of skipping it is an ops footgun — the game's own withdrawal failing early — not a
user-facing safety issue.

### 4.4 MagicBlock behaviour

**Confirmed:**

- Commit-without-undelegate is supported.
- Commit failure retries; a failed commit leaves the account delegated and re-committable.
  No state is lost, no partial commit occurs.
- Uncommitted state survives validator failure; the rollup ledger can be replayed.
- Reallocation of a delegated account: **assumed not possible.** §2.2 and §3.5 are built on
  this. If it turns out to be supported, the pre-allocation band can be relaxed — but do not
  design for that until confirmed.

**Not relevant any more:** whether the rollup commits lamport deltas. Because the reserves are
never delegated and SOL is an ordinary ledger entry, no lamport ever moves inside the
rollup. This was the deciding argument for SOL-as-entry over SOL-as-lamports.

---

## 5. Costs

| Action | Approx. cost | Paid by |
|---|---|---|
| Create a ledger (32 slots, ~1.3 KB) | ~0.01 SOL rent | the depositor |
| Create its permission account | rent, refunded on close | the depositor |
| growth (+16 slots, 640 B) | ~0.0045 SOL rent | the depositor who triggers it |
| Vault + token account per mint | rent, once per mint ever | first depositor of that mint |

A new player therefore meets one real cost — roughly a cent of rent — and never again.

---

## 6. Safety checklist

- [ ] All arithmetic uses `checked_add` / `checked_sub`. No exceptions.
- [ ] Every PDA passed in is re-derived and compared, never trusted.
- [ ] `deposit` source and `withdraw` destination are derived from the signer, never passed.
- [ ] `pda_auth` is set at creation from the owner's curve and **never** mutated.
- [ ] `settle` validates the XOR *before* mutating any balance.
- [ ] `withdraw` asserts the reserve physically covers the amount (both SOL and SPL paths).
- [ ] Every SPL path asserts the token account is the vault's canonical ATA for the mint.
- [ ] `initialize_vault` has been run; no SOL path accepts an unfunded vault.
- [ ] The SOL path never reads the token-account slots; the SPL path asserts
      `token_account.mint == mint`.
- [ ] Slot 0 is SOL in every ledger, always, from creation.
- [ ] A zero mint at index > 0 is a free slot and is never treated as SOL.
- [ ] `grow` refuses while delegated.
- [ ] No instruction accepts an arbitrary destination pubkey.
- [ ] `delegate_ledger` refuses without an existing permission account.
- [ ] `close_ledger` refuses while any entry still carries a balance.

---

## 7. Deployment

**Build note.** The program builds with `cargo build-sbf`, but the test suite needs a host
rustc of 1.89 or newer (`cargo +1.89.0 test`) — the `solana-*` crates pulled in by
`solana-curve25519` refuse to compile on older toolchains. The devnet harness is
`node scripts/exercise-devnet.mjs`.

Redeploying can outgrow the allocated program account ("invalid program argument" on
deploy); `solana program extend <id> <bytes>` fixes it. Watch for this — a failed deploy
leaves the *old* binary live, so the next test run silently exercises stale code.

**Extend by what is needed, not a round number.** Rent on a program data account is locked
until the program is closed, and closing an immutable program is not an option — so an
over-sized account is SOL gone for good. During iteration on devnet this cost ~1.5 SOL of
slack that cannot be recovered.

**The final deployment is sized exactly.** This program is never upgraded — the authority is
burned at step 5 — so it needs no headroom at all. Deploy it fresh at exactly
`ls -l target/deploy/vault.so`, with no extend, and never carry the iteration slack into it.

1. Deploy.
2. **Run `initialize_vault` immediately.** It requires the upgrade authority, so it becomes
   impossible after step 5 — and without it no SOL can ever be deposited.
3. Prove it (`scripts/exercise-devnet.mjs`, then `scripts/privacy-devnet.mjs`): exercise every instruction on a normal ER first, then move to a private
   rollup and confirm the ledgers are genuinely unreadable.
4. Verify the build reproducibly (verifiable build → source-to-bytecode match).
5. **Burn the upgrade authority — last, and only once the above is proven.** This is what
   makes the program non-custodial, and it is irreversible.
6. Publish source, open licence.
7. Ship a bare reference implementation in the repo. **No hosted front-end, no fee, no
   operated service.**

**Get an audit before it holds real value.** Immutable means no patching — a bug is
permanent and unfixable by design.

---

## 8. Summary of properties

| Property | Mechanism |
|---|---|
| Non-custodial | No upgrade authority, no admin, no path for the deployer to move funds |
| Not a payment rail | `settle` XOR makes user→user unreachable |
| Same-owner basenet boundary | `deposit`/`withdraw` derive the counterparty from the signer |
| Conservative totals | `settle` moves, never mints or burns |
| Solvent | Two invariants + physical reserve check on withdraw |
| Delegatable | One ledger per owner across all mints; pre-allocated slots |
| Uniform assets | SOL is entry 0; `settle` never special-cases it |
| Auditable value flow | basenet deposits/withdrawals are public and attributable |
| Confidential gameplay | Balance mutation happens in the rollup, not on basenet |

The last two together are the point: **value in and value out are legible; play is not.**
