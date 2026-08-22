# vault

A non-custodial, multi-asset balance program for Solana, built to work inside
[MagicBlock](https://magicblock.gg) ephemeral rollups — including **private** ones, where the
validator runs in a TEE and account data is not world-readable.

The problem it solves: an app wants to charge its users and pay them out, thousands of times,
without a basenet transaction per action and without ever holding their money. The usual answer
is a custodial escrow account per app. This isn't that.

> **Status: devnet.** Deployed at `VAULTrDSUBZ8AXL2kGVYE8eKAn7tgWXRPAevNGUsyTV`, upgrade
> authority **not** burned. Unaudited. Don't put real money in it.
>
> This repository is the reference for that one instance. The intent is a single live vault,
> ultimately unowned — not a program you deploy yourself.

The program is native Rust — no framework. The wire format is Anchor-compatible on purpose:
instruction discriminators are `sha256("global:<name>")[..8]`, account layouts are byte-identical
to their Anchor equivalents, and `vault-idl.json` describes the whole surface, so any
Anchor-style client works unchanged.

---

## The shape of it

One account per owner — a **ledger** — holds every mint that owner has, as a flat table:

```
offset  size  field
     0     8  account discriminator
     8    32  owner
    40     1  pda_auth      (1 = owned by a program PDA, 0 = a human)
    41     1  bump
    42     6  padding
    48    32  rent_payer    (where rent returns on close; the only key that may grow it)
    80    32  authorized    (wallet: a session key or zero; PDA: the member program)
   112     4  capacity
   116    40  entry[0]      slot 0 is always SOL
   156    40  entry[1]
   ...        entry[capacity - 1]
```

Each entry is `mint: Pubkey` + `amount: u64`. Slot 0 is reserved for SOL; any other slot whose
mint is the System Program is free. A debited entry that reaches zero releases its slot (slot 0
excepted). Ledgers grow in place when they run low on free slots, funded by their rent payer.

The tokens themselves live in one **reserve** — PDA `["vault"]` — holding SOL directly and SPL
tokens in its associated token accounts. A ledger is a claim on the reserve, not a container.

Everything else follows from four rules:

1. **A withdrawal's destination is derived from the signer, never passed.** There is no way to
   spell a withdrawal to somebody else, so a bug in a calling program cannot redirect funds.
2. **The debited side authorises.** A human authorises by signing. A program authorises by
   `invoke_signed` over its own seeds, which it can only do for a ledger it owns. So a program
   cannot debit a human, and cannot debit another program. A *credit* needs no signature, which
   is what lets a program pay out to a user who has closed the app — with one exception: a credit
   that would claim a **new** mint slot on a human's ledger needs that human's consent (owner or
   session key), or anyone could fill a wallet's free slots with dust.
3. **Never two human ledgers in a movement.** Program to program is fine — a program moving a
   share of each sale into a prize pool it cannot later drain. Human to human is unrepresentable.
4. **A program's ledger is never deposited to or withdrawn from.** Off-curve owners are
   refused by both instructions. Value reaches a program only through `settle`.

### No instruction pays one person another

Rules 3 and 4 together are what make this not a payments program. `withdraw` sends only to the
ledger's owner, `deposit` credits only the depositor — both refuse a program's ledger outright —
and `settle`, the one place two ledgers touch at all, never accepts two human sides. Each person
moves their own balance in and out and spends it with a program; peer-to-peer payment belongs in
a separate program, written by someone who has decided to take that on.

In code, rule 3 is a single asymmetric check:

```rust
if !(src.pda_auth || dst.pda_auth) {
    return Err(VaultError::NotProgramMediated.into());
}
```

It looks like it wants adjusting — tightened to `src.pda_auth != dst.pda_auth`, exactly one
program side, or dropped in favour of "whoever is debited signs" alone. Neither survives
inspection: the XOR would forbid program-to-program settlement, which is legitimate — a protocol
sweeping its fees into its own treasury — and the signature rule on its own makes Alice-pays-Bob
expressible in a single instruction. The OR plus rule 2 give both properties; a tidy-up that
merges them removes one silently.

---

## Instructions

| instruction | where | who signs | what it does |
|---|---|---|---|
| `initialize_vault` | basenet | anyone | funds the reserve to its rent floor; idempotent |
| `open_wallet_ledger` | basenet | owner | an empty wallet ledger at a chosen size (max 256 slots per open), permission `[owner]` |
| `open_pda_ledger` | basenet | owner + payer | a program's ledger, sponsor-funded; the member program proven from the owner's seeds |
| `grow_pda_ledger` | basenet | owner + rent payer | adds slots to a program's ledger; wallets grow through `deposit` |
| `assign_ledger_authorization` | basenet | owner | sets a wallet ledger's session key; the zero key revokes |
| `make_public` | basenet | owner + rent payer | deletes the ledger's permission — privacy explicitly given up |
| `make_wallet_ledger_private` | basenet | owner | recreates a wallet ledger's permission, naming the owner |
| `make_pda_ledger_private` | basenet | owner + rent payer | recreates a program ledger's permission, with the same proof |
| `deposit` | basenet | owner | wallet → ledger, wallets only; creates the ledger and its permission on first use |
| `withdraw` | basenet | owner | ledger → the owner's wallet, wallets only |
| `settle` | either | the debited side; the credited human too, for a new mint slot | moves a balance between two ledgers, never two humans |
| `create_receipt` | rollup | member PDA + session key | writes agreed movements to an ephemeral account; schedules the reap |
| `settle_receipt` | rollup | session key | applies a receipt, runs the member program's callback, closes the receipt |
| `reap_receipt` | rollup | nobody — a scheduled crank | closes an orphaned receipt, returning its rent to the sponsor ledger |
| `delegate_ledger` | basenet | owner + payer | hands the ledger to a rollup validator |
| `undelegate` | rollup | any payer | commits the ledger back to basenet; permissionless |
| `close_ledger` | basenet | owner + rent payer | sweeps everything out, closes the permission, refunds the rent |

A wallet ledger's rent payer is its owner; a PDA ledger's is whoever sponsored the open. The
delegation program's fixed-discriminator undelegation callback is also handled, but is not part
of the callable surface.

The permission is created with the ledger and dies with it, so a ledger and its privacy share
one lifecycle and there is never a window in which a ledger delegated to a private rollup sits
readable. A PDA's permission names its **program** and its **sponsor**, nothing else — the PDA
itself can neither sign an RPC challenge nor be a transaction's top-level program, so naming it
would be decoration; the sponsor is what keeps a program's ledger readable by its operator. The
only verbs privacy has are the explicit opt-out: `make_public` deletes the permission, for the
rare ledger that is *meant* to be watched — a progressive pot is worthless as a secret, and a
public copy of the figure would be a second number free to drift — and the two
`make_*_ledger_private` variants take it back.

The wallet and PDA variants are separate instructions on purpose: they differ in who pays, who
the permission names, and what has to be proven, and each asserts its owner kind instead of
branching on it.

There is no `commit_ledger`, for two reasons: commit is implicit in `undelegate`, and a commit
writes the ledger's current state to basenet, where it is world-readable. On a private rollup
its absence is what keeps the play-by-play inside the validator — basenet sees one aggregate
change when the session ends, never the states in between.

### Session keys

`assign_ledger_authorization` stores a second key — `authorized` — on a wallet ledger. It is a
session key: an ephemeral keypair the player's client holds, allowed to consent to that ledger's
debits in `settle` and `settle_receipt` so gameplay never needs the wallet itself to sign.
Assigning the zero key revokes it; an off-curve key is refused. A PDA ledger cannot be assigned
one — its `authorized` is the member program, fixed at `open_pda_ledger`, and is what binds the
ledger to that program at receipt settlement.

### PDAs

```
ledger           ["ledger", owner]                        this program
reserve          ["vault"]                                this program
vault authority  []                                       this program (signs the receipt callback)
receipt          ["receipt", member_program, consenter]   this program (ephemeral)
permission       ["permission:", ledger]                  ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1
```

Note the colon in the permission seed. It is not a typo.

---

## Using it

### The caller creates token accounts, not the program

The vault moves tokens; it never opens an account to move them into. Both the reserve's ATA and
the owner's ATA must exist, so put `CreateIdempotent` in the same transaction:

```js
const reserve = PublicKey.findProgramAddressSync([Buffer.from('vault')], VAULT)[0];

tx.add(
  createAssociatedTokenAccountIdempotentInstruction(payer, ataOf(reserve, mint), reserve, mint),
  createAssociatedTokenAccountIdempotentInstruction(payer, ataOf(owner, mint), owner, mint),
  depositIx(owner, mint, amount),
);
```

Idempotent unconditionally — it costs nothing when the account is already there and saves a
round-trip per mint.

### Deposit

Opens the ledger and its permission on first use, so there is nothing to create beforehand.
`amount` may be zero, which is how you open a ledger without funding it. Deposits are self-only:
the signer is the ledger owner.

```js
const data = Buffer.concat([
  disc('deposit'),          // sha256('global:deposit')[..8] — see vault-idl.json
  mint.toBuffer(),          // SOL is the System Program id
  u64(amount),
  Buffer.from([0]),         // Option<u16> min_free      — None
  Buffer.from([0]),         // Option<u16> slot_increase — None
]);

keys = [
  sg(owner), rw(ledger), rw(permission), ro(PERMISSION_PROGRAM), rw(reserve),
  rw(isSol ? SystemProgram.programId : ataOf(reserve, mint)),
  rw(isSol ? SystemProgram.programId : ataOf(owner, mint)),
  ro(TOKEN_PROGRAM), ro(SystemProgram.programId),
];
```

`withdraw` takes the same shape without the permission accounts:
`[owner, ledger, reserve, vault_token, owner_token, token_program, system_program]`.

For SOL the two token slots carry the System Program as a placeholder and are never read.

### A program's treasury

A program can own ledgers — as many as it has PDAs. A treasury and a prize pool are two ledgers
of the same program.

**Neither is ever deposited to or withdrawn from.** `deposit` and `withdraw` refuse an off-curve
owner outright, so the only way value reaches or leaves a program is `settle` against a human who
already holds a balance:

```
fund     admin deposits to their own ledger, then settles admin → house
withdraw settle house → admin, then admin withdraws their own ledger
```

Both are ordinary human-to-program settles: the debited side signs, and there is never more than
one human involved. Nothing about a program's treasury is a different shape from anyone else's,
which is the point — a special path in and out is exactly where a way to move value between
people would hide.

What a PDA cannot do is open its own ledger: it holds no lamports for rent and cannot sign a
System transfer. Hence `open_pda_ledger`, which creates an empty one at a chosen size with the
rent paid by somebody else. Capped at 256 slots per call, since rent scales with the count and is
paid up front. It can be extended later with `grow_pda_ledger`, funded by the recorded rent
payer — `deposit` grows a wallet's ledger as it goes but refuses an off-curve owner, so that is
the only way a program's ledger grows.

Each ledger records a **rent payer**: where its rent goes when it closes, and the only account
that may grow it. A wallet funds its own ledger and nobody else may, because the rent comes back
to the owner and paying somebody's rent would otherwise be a way to hand them money. A PDA cannot
pay, so whoever does becomes the rent payer — the lamports return to them, which is what makes
sponsoring a program's ledger free of that problem.

The permission a PDA's ledger gets at `open_pda_ledger` names `[the member program, the
sponsor]` — and the program is **proven, not trusted**: its seeds must derive the owner's
address, and since address spaces cannot collide across programs, the owner's signature could
only have come from it. (Reading the program off the account's owner field would be correct
only by timing — delegation and reassignment change that field.) The program is named because
it has to reach its own ledgers to settle them, and the rollup's filter only ever sees an
instruction's top-level program. The sponsor is named because it is the one member that can
sign an RPC challenge: without it, a program's ledger is a book nobody — including its
operator — can open.

That is what a prize pool is built from. The program settles into it on every sale and out of it
only on a payout, and simply never writes an instruction that settles it anywhere else. There is
then no way to drain it, by anyone, ever — enforced by the absence of code rather than by a check
somebody could relax.

### A session

```js
delegate_ledger(payer, owner, validator)   // basenet; validator = None for the public cluster
  ... play ...
undelegate(payer)                          // sent to the rollup; commits on the way out
```

`undelegate` is permissionless: it only commits the ledger home — no value moves — so anyone may
pay to rescue a ledger whose keys are lost. Its accounts are
`[payer, authority, ledger, magic_program, magic_context, fees_vault]`, where the payer must be
the transaction's fee payer and `fees_vault` is the magic program's ephemeral vault.

Wait for **basenet** to reflect each change — the ledger's owner becoming the delegation program,
and then becoming the vault again. Do not wait for the rollup to hold a copy: validators clone
lazily, on first touch, so an account that has never been used there will not appear no matter how
long you watch.

Deposits and withdrawals only work on basenet. If a ledger is delegated, undelegate first, act,
then delegate again.

---

## Privacy, and the receipt pattern

In a private rollup, an account can carry a **permission** listing who may see it. A wallet's
ledger names exactly one member — its owner — which is what lets a player read their own balance
and nobody else read it. A PDA's names its program and its sponsor, as above.

The catch:

> A transaction touching a permissioned account is refused at submission unless the
> **top-level program of that instruction** is a member of the permission. Submitter identity and
> signatures are never consulted. CPI'd programs are invisible, because the filter runs before
> execution.

The vault is a member of every ledger by virtue of owning it. A *program* CPI-ing into the vault
is not — and cannot be, without every ledger enumerating every caller in advance (the permission
account holds 16 members, fixed at creation).

MagicBlock's position is correct and forced: any program handed an account can read its bytes and
copy them out, and CPI hands data over on entry. There is no "pass through without reading".

**The receipt is the way around it without weakening any of that.** Instead of the calling
program touching the ledgers, the work is split into two instructions of one transaction:

```
ix 0   program.request      →  CPI vault.create_receipt
                               The member PDA and the session key sign; the agreed movements are
                               written to an ephemeral vault-owned account, rent fronted by the
                               program's own ledger. No player ledger is present, so nothing is
                               permissioned and the filter admits the program's instruction.

ix 1   vault.settle_receipt →  top-level vault, so the filter admits this one too.
                               Verifies consent, applies the movements, then CPIs the member
                               program's callback — `callback_disc ++ owners[0] ++ args`, with
                               the receipt and the vault authority PDA as a signer in front of
                               any forwarded accounts. That signature is the proof the settle
                               happened: nothing else can produce it. Finally the receipt is
                               closed back onto the sponsor ledger and the reap is cancelled.
```

One transaction, so it is atomic: nothing is delivered unpaid, and the payment cannot happen
without the delivery. A receipt is only settleable in the slot it was created — after that it is
`ReceiptExpired` debris — so the two instructions must land together.

**Consent lives at settle, not at creation.** `create_receipt` touches no ledger, so it has
nothing to check consent against; `settle_receipt` runs top-level as the vault, which owns every
ledger and may read them past the filter — the one place the check is possible. The consenter —
the session key that seeded and signed the receipt — must sign `settle_receipt` too, and must be
the human ledger's owner or authorized key whenever that ledger is debited or gains a new mint
slot. Every program ledger in the receipt must have the receipt's member program as its
`authorized`, and at most one ledger may be human. A receipt that debits a human it has no right
to simply dies at settle.

The receipt's address is `["receipt", member_program, consenter]` — one live receipt per program
per session key, unforgeably that session's. A stale unsettled receipt at that address is
overwritten by the next `create_receipt`.

**Orphaned receipts reap themselves.** `create_receipt` schedules a one-shot task with the
rollup's scheduler, signed by the sponsor ledger, that fires `reap_receipt` against the receipt.
If the receipt was settled, the task was cancelled (and the reap is a no-op anyway); if the
transaction died between create and settle, the task fires after the creation slot has passed,
closes the receipt, and returns its rent to the sponsor ledger. Nothing is left behind either way.

---

## Talking to a private rollup

Reads fail closed and writes return `401` without a token. Get one by signing a challenge:

```
GET  /auth/challenge?pubkey=<pubkey>     → { challenge }
POST /auth/login  { pubkey, challenge, signature }  → { token }
```

then put `?token=<token>` on every RPC URL. No transaction, no account, no cost — it just proves
you hold the key, which is what lets the validator decide whether to serve a private ledger.

## Licence

[Apache License 2.0](LICENSE). Apache rather than MIT for the explicit patent grant, which
matters for on-chain code somebody else may build a product on.

## Layout

```
programs/vault/src/
  entrypoint.rs             program id + entrypoint
  instruction.rs            wire discriminators → dispatch
  constants.rs, error.rs
  state/
    ledger.rs               the ledger: layout, load/store, slot allocation
    receipt.rs              the receipt: layout, read/write
  instructions/             one file per instruction
  utils/
    account.rs              ledger creation and growth
    crank.rs                scheduling and cancelling the reap task
    pda.rs                  PDA validation, the curve check, the member-program proof
    permission.rs           permission create/close CPIs
    reserve.rs              the reserve's floor and canonical ATAs
    spl.rs                  SPL transfers
vault-idl.json              Anchor-style IDL of the wire format
vault-program-spec.md       the long-form design notes
```
