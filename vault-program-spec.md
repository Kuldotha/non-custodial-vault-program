# Vault Program — Design Specification

A minimal, immutable, multi-mint ledger primitive for Solana, designed to be delegated to
a MagicBlock ephemeral rollup and CPI'd into by member programs (games).

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
| Every movement has at least one program side | Makes user→user transfer unreachable by any instruction sequence. |
| Totals are conservative across every operation | `settle` moves value; it never creates or destroys it. |

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
pub const SOL_MINT:          Pubkey = Pubkey::default(); // the System Program: it controls lamports
pub const DEFAULT_SLOTS:     u16 = 32;
pub const DEFAULT_MIN_FREE:  u16 = 16;
pub const MAX_SLOT_INCREASE: u16 = 256;   // Solana caps a single realloc at 10 KiB = 256 entries
pub const MAX_GROWTH:        u16 = 256;   // most slots one open instruction may allocate up front

pub struct Entry {
    pub mint:   Pubkey,   // 32
    pub amount: u64,      //  8
}                         // 40 bytes

pub struct Ledger {
    pub owner:      Pubkey,     // 32 — the seed, stored for cheap reads
    pub pda_auth:   bool,       //  1 — true when `owner` is off-curve (a program's PDA)
    pub bump:       u8,         //  1
    // 6 bytes padding
    pub rent_payer: Pubkey,     // 32 — where rent returns on close; the only key that may grow it
    pub authorized: Pubkey,     // 32 — wallet: a session key or zero; PDA: the member program
    pub entries:    Vec<Entry>, //  4 + 40 × capacity
}
```

Serialized behind an 8-byte account discriminator, the header is 116 bytes, with entries
following at 40 bytes each.

**Slots are pre-allocated.** A ledger's account is sized for its full capacity, all slots
present and zeroed apart from slot 0. This is what allows a new mint to be added to a
ledger *inside the rollup*: reallocation of a delegated account is not possible, so the
space must already be there. `capacity` is simply `entries.len()`; there is no separate
occupancy counter to fall out of sync. A slot is free when `mint == SOL_MINT && index != 0`,
and a debited entry that reaches zero releases its slot (slot 0 excepted) — a released slot
is what keeps credits from permanently filling a ledger with the dust of mints its owner
never asked for.

**The rent payer** is recorded at creation: a wallet must fund its own ledger (paying
someone's rent would otherwise be a way to hand them money — the rent comes back to the
owner at close), while a PDA cannot pay, so its sponsor becomes the rent payer and the rent
returns to them. Only the rent payer may fund growth.

**The authorized key** is the ledger's second identity. On a wallet ledger it is a session
key — assignable and revocable by the owner — that may consent to the ledger's debits. On a
PDA ledger it is the member program, set at open, and binds the ledger to that program at
receipt settlement.

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

`settle` and `settle_receipt` preserve both trivially — they only move between entries and
never touch the reserves. `deposit` and `withdraw` move both sides together atomically.

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

`pda_auth` is computed once at creation with the curve-validation syscall and stored.
Storing is safe here — unlike a check on mutable account state — because the owner is fixed
by the PDA derivation and re-validated on every use, so the property can never drift.

A program therefore does not use its program ID as the ledger owner (it cannot sign for
that). It creates an authority PDA of its own — e.g.
`find_program_address([b"vault_authority"], GAME_ID)` — and that PDA is the ledger owner.
Where the vault needs to know *which* program is behind an owner (`open_pda_ledger`,
`make_pda_ledger_private`, `create_receipt`), the caller passes the program id and the
owner's seeds and the vault re-derives: the seeds must produce the owner's address under
that program, and since address spaces cannot collide across programs, the owner's
signature could only have come from it. The account's `owner` field is never used for
this — delegation and reassignment rewrite it.

---

## 3. Instructions

| Instruction | Where | Who calls it |
|---|---|---|
| `initialize_vault` | basenet | anyone, once; idempotent |
| `open_wallet_ledger` | basenet | a wallet, for itself |
| `open_pda_ledger` | basenet | a program (its PDA signs) plus a sponsor |
| `grow_pda_ledger` | basenet | a program plus its recorded rent payer |
| `assign_ledger_authorization` | basenet | a wallet, for its own ledger |
| `make_public` / `make_wallet_ledger_private` / `make_pda_ledger_private` | basenet | the owner (plus rent payer where rent moves) |
| `deposit` | basenet | a wallet, for its own ledger |
| `withdraw` | basenet | the ledger owner |
| `settle` | either | the debited side |
| `create_receipt` | rollup | a member program, by CPI |
| `settle_receipt` | rollup | the session key, top-level |
| `reap_receipt` | rollup | the rollup's task scheduler |
| `delegate_ledger` / `undelegate` | basenet / rollup | the owner / anyone |
| `close_ledger` | basenet | the owner plus rent payer |

Wallet-ledger creation is also folded into `deposit`: a first deposit creates the ledger
and its permission, so the common path needs no setup call. A ledger cannot exist without
the permission that keeps it unreadable in a rollup — every creation path makes both.

There is **one** deposit and **one** withdraw. The `mint` argument is the asset selector,
including for SOL — `SOL_MINT` is a legitimate input, not a sentinel to reject. When it is
passed, the token-account slots carry the System Program as a placeholder and are never
read.

### 3.1 `initialize_vault()`

Funds `["vault"]` to `rent_exempt(0)`. Permissionless and idempotent: it only ever tops the
vault up to its floor, so there is nothing to gate. It must run before any SOL path — the
vault's rent cannot come out of a deposit, or the last lamports credited would be
unwithdrawable — and every SOL path refuses an unfunded vault with `VaultNotInitialized`.

### 3.2 `deposit(mint, amount, min_free, slot_increase)`

```
1. Derive ["ledger", signer.key]; assert the passed account matches. Refuse an off-curve
   signer: a program's ledger moves value only through settle.
2. If uninitialised, create the ledger: DEFAULT_SLOTS entries, slot 0 SOL, pda_auth from
   the owner's curve, rent payer = owner.
3. If the permission account is empty, create it (§3.7).
4. ensure_headroom(min_free, slot_increase)   — before the claim, not after: a deposit of
   a new mint into a full ledger would otherwise fail on the very growth it is about to
   perform (§3.6).
5. If mint == SOL_MINT:
     assert vault.lamports >= rent_exempt(0)              (§3.1)
     System transfer `amount` from signer → ["vault"]     (only the wallet can debit itself)
   else:
     assert the reserve is the canonical ATA(["vault"], mint)
     assert source token_account.mint == mint
     SPL transfer `amount` from the signer's token account → the reserve
6. entry.amount = entry.amount.checked_add(amount)?
```

The ledger account is created by transfer-then-allocate-then-assign rather than
`create_account`: the latter aborts on a pre-funded account, so anyone could block a
ledger's creation by sending its address a lamport first.

The destination is derived, never passed as an argument. The signer must be the ledger
owner: there are no third-party deposits.

### 3.3 `withdraw(mint, amount)`

```
1. Derive ["ledger", signer.key]; assert match; assert signer.is_signer; refuse off-curve.
2. entry = SOL_MINT ? entries[0] : find(mint)               (error if absent)
3. entry.amount = entry.amount.checked_sub(amount)?         (slot released if it hits zero)
4. If mint == SOL_MINT:
     assert vault.lamports − rent_exempt(0) >= amount       // physical check
     System transfer `amount` from ["vault"] → signer       (signed with vault seeds)
   else:
     assert tokens(mint).amount >= amount                   // physical check
     SPL transfer from ATA(["vault"], mint) → the signer's token account, whose owner
     field must be the signer
```

The destination is derived from the signer, never passed. This is what makes withdrawal
same-owner-only. Program ledgers never withdraw at all — `withdraw` refuses an off-curve
owner; value leaves a program only through `settle`.

### 3.4 `settle(mint, amount)` — the cross-ledger movement

```rust
// accounts: [src_ledger, dst_ledger, src_authority, dst_consenter]

// 1. Both ledgers loaded and pinned to their own (owner, bump) derivation; src != dst.
// 2. src_authority must be the source ledger's owner.
// 3. At least one side is a program.
if !(src.pda_auth || dst.pda_auth) { return Err(NotProgramMediated); }

// 4. The debited side authorises: a human by signing, a program by invoke_signed
//    over its own seeds.
require!(src_authority.is_signer);

// 5. A credit that would claim a NEW mint slot on a human ledger needs that human's
//    consent: dst_consenter must sign and be the ledger's owner or session key.
//    Crediting an existing entry, or any program ledger, needs nothing.

// 6. Move value. The reserves are untouched; this is pure bookkeeping.
src.debit(mint, amount)?;      // checked_sub; slot released at zero
dst.credit(mint, amount)?;     // checked_add; claims a free slot if consented
```

`settle` never learns that SOL is special: slot 0 is found by the same lookup as any other
mint, and no reserve account is involved either way.

#### Why at-least-one-program matters

| src | dst | Result |
|---|---|---|
| human | program | ✅ user pays an entry fee / buys something |
| program | human | ✅ game pays out a prize |
| program | program | ✅ a protocol sweeping fees into its own treasury |
| human | human | ❌ **rejected** — this is the whole point |

Alice and Bob both have `pda_auth = false`. `settle(alice → bob)` fails at step 3, before
touching any balance. There is **no instruction sequence in this program** that moves value
from one person to another. Program-to-program is deliberately allowed: a program can only
debit ledgers it owns, so the movement stays within one program's obligations.

#### Why the signer asymmetry matters

A user must sign to be *debited* but not to be *credited* (an existing entry, at least). So
a game can push payouts to winners who are offline, and can never pull from a user who did
not authorise it. The new-slot consent in step 5 closes the one gap that asymmetry leaves:
without it, anyone could burn a wallet's free slots by settling dust mints into it.

#### Attack: fake authority

Someone creates a ledger whose owner is a PDA they do not control, hoping to mislabel it.
They cannot sign for it — only the deriving program can. They have locked their own tokens
in an account only a third party can move. Self-defeating.

#### Residual case (accepted)

Alice deploys a two-instruction forwarding program. Now `alice → AliceProgram → bob` works.
This is unavoidable at the primitive level and is correct: Alice deployed it, Alice
operates it, the obligations are hers. What matters is that **this program cannot be used
as a payment rail directly** — anyone who wants one must publish their own.

### 3.5 Session keys — `assign_ledger_authorization(authorized)`

A wallet owner stores a second key on their ledger: a **session key**, held by the client,
allowed to consent to that ledger's debits in `settle` and `settle_receipt`. Gameplay then
never needs the wallet itself to sign — the wallet delegates consent for a session and
revokes it (by assigning the zero key) when done. An off-curve session key is refused, and
a PDA ledger cannot be assigned one at all: its `authorized` is the member program, fixed
at `open_pda_ledger`, and means something different (§3.10).

basenet only; the owner signs.

### 3.6 Growth

Every `deposit` keeps free slots above `min_free` (default 16), adding `slot_increase`
(default 32, max 256 — a single realloc is capped at 10 KiB) when the ledger runs low, rent
funded by the rent payer. basenet only — a delegated ledger cannot realloc, which is the
whole reason the slots are pre-allocated.

Because `settle` fails rather than growing, the headroom band is what guarantees a session
never hits the wall mid-play.

A program's ledger is opened at a chosen size with `open_pda_ledger` (up to 256 slots,
rent paid up front by the sponsor) and extended with `grow_pda_ledger`, funded by the
recorded rent payer. `deposit` refuses an off-curve owner, so that is the only way a
program's ledger grows.

### 3.7 The permission

Every ledger is created together with a basenet **permission** account — the ACL a private
rollup enforces. A wallet ledger's names exactly one member, the owner at flags 0, so the
owner can read their own ledger inside a private rollup and nobody else can. A PDA ledger's
names two: the member program (proven from seeds, §2.5) and the sponsor. The program is
named because the rollup's filter only ever sees an instruction's top-level program and the
program must reach its own ledgers; the sponsor because it is the one member that can sign
an RPC challenge — without it, a program's ledger is a book nobody, including its operator,
can open. The PDA itself is not named: it can neither sign a challenge nor head a
transaction, so naming it would be decoration. The ledger signs for its own permission with
its own seeds.

> **`Some(...)`, never `None`.** `MembersArgs.members` is an `Option`, and the two values
> mean opposite things: `None` is *no ACL at all* — the account stays readable by anyone —
> while any `Some` restricts it. There is no error and no warning between the two — only
> the behaviour.

> **Membership is the read gate; no flag grants it.** The flag set — `AUTHORITY` (1),
> `TX_LOGS` (2), `TX_BALANCES` (4), `TX_MESSAGE` (8), `ACCOUNT_SIGNATURES` (16) — is about
> transaction-level visibility. A member at flags 0 reads the account; an anonymous caller,
> or a non-member holding a valid token, gets nothing.

**`AUTHORITY` is deliberately withheld from every member.** It would let them rewrite the
ACL and expose a ledger this program is meant to keep private. In code that can never be
patched that has to be impossible rather than discouraged, so a ledger's privacy is fixed
at creation and no instruction can widen it.

This is deliberately the **basenet** permission, not the ephemeral one. An ephemeral
permission is created inside the rollup, after the account is already delegated and live
there, which leaves a window in which the ledger is readable. Creating it on basenet
before delegation closes that window entirely. The permission is never delegated and has
no commit lifecycle — the rollup copies the basenet permission data when it needs it.

Privacy's only verbs are the explicit opt-out and its reversal. `make_public` deletes the
permission — for the rare ledger that is *meant* to be watched, a progressive pot being
worthless as a secret — and `make_wallet_ledger_private` / `make_pda_ledger_private`
recreate it, with the same membership and the same proof as at creation.

### 3.8 `close_ledger()`

Sweeps every balance back to the owner and closes both the ledger and its permission
account. The rent goes to the recorded rent payer, who must sign alongside the owner.

Each non-zero token entry needs its `(vault_token, owner_token)` pair in the remaining
accounts, in entry order. The instruction asserts that **every** entry is zero before
closing, so an incomplete account list can never strand value in the vault.

Basenet only: a delegated ledger is owned by the delegation program, so the ownership
check rejects it before anything runs.

### 3.9 Delegation

- `delegate_ledger(validator)` — basenet. The owner signs; hands `["ledger", owner]` to the
  delegation program. `validator = None` targets the public cluster. Delegation deliberately
  does not gate on the permission's existence: the vault works just as well outside a TEE,
  where no ACL applies, and since every creation path makes a permission, a ledger only
  lacks one after an explicit `make_public` — the owner already chose to be readable.
- `undelegate()` — rollup-side, **permissionless**. Ends the session; the commit is
  implicit, so there is no separate commit instruction to forget. It only writes the
  ledger's true state home — no value moves — so anyone may pay to rescue a ledger whose
  keys are lost. The payer must be the transaction's fee payer (the rollup only lets an
  account be written if it is delegated or is the fee payer).
- The delegation program's fixed-discriminator undelegation callback is handled to finalise
  the return.

Reserves are never delegated: `["vault"]` and `ATA(["vault"], mint)` stay on basenet
permanently. That is precisely why `settle` can be pure bookkeeping.

### 3.10 Receipts

In a private rollup, a transaction touching a permissioned account is refused at submission
unless the **top-level program of that instruction** is a member of the permission.
Submitter identity and signatures are never consulted, and CPI'd programs are invisible —
the filter runs before execution. The vault is a member of every ledger by virtue of owning
it; a game CPI-ing into the vault is not, and cannot be without every ledger enumerating
every caller in advance. The receipt splits the game's intent from the vault's execution so
each instruction passes the filter on its own merits, without weakening the ACL.

**`create_receipt(movements, member_program, authority_seeds, owners, callback_disc, args)`**
— reached by CPI from the member program. The program's authority PDA signs (proven against
`member_program` by seeds, §2.5) and the session key — the **consenter** — signs. The agreed
movements, the owner list, the callback discriminator and opaque args are written to an
ephemeral vault-owned account at `["receipt", member_program, consenter]`, its rent fronted
by the program's own ledger. No player ledger is present, so nothing is permissioned. Owner
indices are bounds-checked, duplicates refused. A same-slot receipt at that address cannot
be overwritten; an earlier slot's is debris and is reused. Finally a one-shot **reap task**
is scheduled with the rollup's task scheduler, signed by the sponsor ledger (§3.11).

**`settle_receipt()`** — top-level vault, so the filter admits it, and the one place
consent can be checked: the vault owns every ledger and may read them past the ACL. That is
why consent lives at settle rather than creation. Checks, in order:

1. The receipt is vault-owned, carries the receipt discriminator, and was created **this
   slot** — one slot later it is `ReceiptExpired`. Create and settle must land in the same
   transaction, which is also what makes payment-and-delivery atomic.
2. The passed authority, consenter and callback program match the receipt's.
3. The consenter signs. Settlement is not permissionless: only the session that authored
   the receipt can execute it.
4. Every ledger account matches its recorded owner and derivation; no duplicates. Every
   **program** ledger's `authorized` must equal the receipt's member program — a receipt
   cannot move another program's treasury.
5. At most one ledger is human, whatever the shape of the receipt.
6. If the human ledger is debited, or gains a new mint slot, the consenter must be its
   owner or its session key. A receipt that only credits existing entries took nothing and
   needs no relationship to the human at all — which is what lets a payout be executed for
   a player who has closed the app.
7. The movements are applied — checked arithmetic, slots claimed and released as in
   `settle`.
8. The receipt's discriminator is zeroed, so it cannot be re-settled during the callback.
9. The member program's callback is CPI'd: `callback_disc ++ owners[0] ++ args`, with the
   receipt and the **vault authority** PDA (`[]`, as a signer) in front of the forwarded
   accounts. That signature is the proof the settle happened — nothing else can produce it,
   so the member program delivers if and only if it sees it.
10. The receipt is closed back onto the sponsor ledger and the reap task cancelled.

Ledger-validation failures report as `ProgramError::Custom(7000 + check*100 + index)` so a
client can see which account failed which check; everything else uses the error enum.

### 3.11 `reap_receipt()` — the orphan crank

A receipt whose transaction died between create and settle would otherwise sit as rent
debris on the sponsor ledger. The task scheduled at creation fires once, after the creation
slot has passed (the same-slot settle window), and closes the receipt back onto the sponsor
ledger. It is idempotent and fails soft: a settled receipt is already gone, a wrong-slot or
wrong-discriminator account is left alone. The task's id is derived from the receipt
address, which is what lets `settle_receipt` cancel it.

### 3.12 What privacy actually covers

Against a TEE validator:

- a delegated ledger is **not** served to an anonymous caller;
- nor to a caller holding a valid token who is not a member;
- but **is** served to a member of its permission, at flags 0;
- a public rollup will still clone the account, but only ever mirrors the basenet copy
  byte for byte — it holds no session state.

**The boundary is the delegation point, not the account.** Delegation freezes the basenet
copy in the clear, so the balance a ledger *entered* the session with stays public forever.
Privacy covers what changes **during** the session. A game that wants an opening balance
private has to keep it off basenet in the first place.

---

## 4. Delegation model

### 4.1 Roles

| Account | Delegation lifetime |
|---|---|
| Game ledger | Delegated for the whole session/season. Long-lived. |
| Player ledgers | Delegated at join, undelegated at exit. Transient. |

All player ledgers **must delegate to the same validator as the game ledger**, because both
sides of a movement must be in the same rollup. Joining a session is therefore "delegate to
wherever the game ledger is."

### 4.2 Commit strategy

**During a session, nothing commits.** Both sides stay delegated; balances mutate freely in
the rollup.

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
true balance; the only ledger that can hit the physical check in §3.3 is the game's, and
its programs settle rather than withdraw anyway — it clears the moment the game commits.
**Committing on undelegation alone is sufficient.**

### 4.4 Rollup behaviour relied upon

- Commit-without-undelegate is supported; a failed commit leaves the account delegated and
  re-committable. No state is lost, no partial commit occurs.
- Uncommitted state survives validator failure; the rollup ledger can be replayed.
- Validators clone accounts lazily, on first touch — an account that has never been used in
  the rollup will not appear there, no matter how long you watch. Wait on **basenet** for
  delegation state changes.
- Reallocation of a delegated account is not possible. §2.2 and §3.6 are built on this.
- No lamport ever moves inside the rollup: the reserves are never delegated and SOL is an
  ordinary ledger entry. This was the deciding argument for SOL-as-entry over
  SOL-as-lamports.

---

## 5. Costs

| Action | Approx. cost | Paid by |
|---|---|---|
| Create a ledger (32 slots, ~1.4 KB) | ~0.01 SOL rent | the rent payer |
| Create its permission account | rent, refunded on close | the rent payer |
| Growth (+16 slots, 640 B) | ~0.0045 SOL rent | the rent payer |
| Vault token account per mint | rent, once per mint ever | first depositor of that mint |
| A receipt | rent, fronted by the sponsor ledger, returned at settle or reap | the member program |

A new player therefore meets one real cost — roughly a cent of rent — and never again.

---

## 6. Safety checklist

- All arithmetic uses `checked_add` / `checked_sub`. No exceptions.
- Every PDA passed in is re-derived and compared, never trusted.
- `deposit` source and `withdraw` destination are derived from the signer, never passed.
- `pda_auth` is set at creation from the owner's curve and never mutated.
- `settle` and `settle_receipt` refuse a second human ledger *before* mutating any balance.
- `withdraw` asserts the reserve physically covers the amount (both SOL and SPL paths).
- Every SPL path asserts the token account is the vault's canonical ATA for the mint.
- `initialize_vault` has been run; no SOL path accepts an unfunded vault.
- The SOL path never reads the token-account slots; the SPL path asserts
  `token_account.mint == mint`.
- Slot 0 is SOL in every ledger, always, from creation.
- A zero mint at index > 0 is a free slot and is never treated as SOL.
- A new mint slot on a human ledger is only ever claimed with that human's consent.
- Growth is basenet-only: a delegated ledger is not vault-owned, so it fails the load.
- No instruction accepts an arbitrary destination pubkey.
- A receipt settles only in its creation slot, only by its consenter, and only against
  ledgers whose recorded owners it names.
- `close_ledger` refuses while any entry still carries a balance.

---

## 7. Deployment

**Build.** `cargo build-sbf`. Unit and integration tests run with `cargo test` (mollusk-svm
harness). The devnet exercise scripts are `scripts/exercise-devnet.mjs` and
`scripts/privacy-devnet.mjs`; `scripts/tee-auth.mjs` handles the private-rollup token flow.

**Size exactly.** Rent on a program data account is locked until the program is closed, and
closing an immutable program is not an option — an over-sized account is SOL gone for good.
This program is never upgraded once the authority is burned, so the final deployment needs
no headroom at all: deploy at exactly the binary's size, with no `solana program extend`.
(While iterating on devnet, note that a failed redeploy for lack of space leaves the *old*
binary live, so the next test run silently exercises stale code.)

1. Deploy.
2. Run `initialize_vault` — permissionless and idempotent, but without it no SOL can be
   deposited.
3. Prove it: exercise every instruction on a normal ER first, then move to a private
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
| Not a payment rail | No movement ever has two human sides |
| Same-owner basenet boundary | `deposit`/`withdraw` derive the counterparty from the signer |
| Conservative totals | `settle` moves, never mints or burns |
| Solvent | Two invariants + physical reserve check on withdraw |
| Delegatable | One ledger per owner across all mints; pre-allocated slots |
| Uniform assets | SOL is entry 0; `settle` never special-cases it |
| Sessions without wallet signatures | A revocable session key consents in the wallet's place |
| ACL-compatible CPI | The receipt: intent written blind, consent checked at top-level settle |
| Auditable value flow | basenet deposits/withdrawals are public and attributable |
| Confidential gameplay | Balance mutation happens in the rollup, not on basenet |

The last two together are the point: **value in and value out are legible; play is not.**
