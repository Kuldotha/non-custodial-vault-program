# vault

A non-custodial, multi-asset balance program for Solana, built to work inside
[MagicBlock](https://magicblock.gg) ephemeral rollups — including **private** ones, where the
validator runs in a TEE and account data is not world-readable.

The problem it solves: an app wants to charge its users and pay them out, thousands of times,
without a basenet transaction per action and without ever holding their money. The usual answer
is a custodial escrow account per app. This isn't that.

> **Status: devnet.** Deployed at `9vDAQgdHWCPQZabumgcuwoSLzWnRyQkSM1EHQnW8YXjs`, upgrade
> authority **not** burned. Unaudited. Don't put real money in it.
>
> This repository is the reference for that one instance. The intent is a single live vault,
> ultimately unowned — not a program you deploy yourself.

---

## The shape of it

One account per owner — a **ledger** — holds every mint that owner has, as a flat table:

```
offset  size  field
     0     8  anchor discriminator
     8    32  owner
    40     1  pda_auth      (1 = owned by a program PDA, 0 = a human)
    41     1  bump
    42     6  padding
    48     4  capacity
    52    40  entry[0]      slot 0 is always SOL
    92    40  entry[1]
   ...        entry[capacity - 1]
```

Each entry is `mint: Pubkey` + `amount: u64`. Slot 0 is reserved for SOL; any other slot whose
mint is the System Program is free. Ledgers grow in place when they run low on free slots, funded
from the depositor.

The tokens themselves live in one **reserve** — PDA `["vault"]` — holding SOL directly and SPL
tokens in its associated token accounts. A ledger is a claim on the reserve, not a container.

Everything else follows from four rules:

1. **A withdrawal's destination is derived from the signer, never passed.** There is no way to
   spell a withdrawal to somebody else, so a bug in a calling program cannot redirect funds.
2. **The debited side authorises.** A human authorises by signing. A program authorises by
   `invoke_signed` over its own seeds, which it can only do for a ledger it owns. So a program
   cannot debit a human, and cannot debit another program. A *credit* needs no signature, which
   is what lets a program pay out to a user who has closed the app.
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
require!(src.pda_auth || dst.pda_auth, VaultError::NotProgramMediated);
```

It looks like it wants adjusting — tightened to `src.pda_auth != dst.pda_auth`, exactly one
program side, or dropped in favour of "whoever is debited signs" alone. Neither survives
inspection: the XOR would forbid program-to-program settlement, which is legitimate (the prize
pool above), and the signature rule on its own makes Alice-pays-Bob expressible in a single
instruction. The OR plus rule 2 give both properties; a tidy-up that merges them removes one
silently.

---

## Instructions

| instruction | where | who signs | what it does |
|---|---|---|---|
| `initialize_vault` | basenet | upgrade authority | creates the reserve, once |
| `open_ledger` | basenet | owner + payer | an empty ledger at a chosen size (max 256 slots), with its permission |
| `grow_ledger` | basenet | owner + rent payer | adds slots |
| `deposit` | basenet | owner | wallet → ledger, wallets only; creates the ledger and its permission on first use |
| `withdraw` | basenet | owner | ledger → the owner's wallet, wallets only |
| `settle` | either | the debited side | moves a balance between two ledgers, never two humans |
| `create_receipt` | rollup | program; the human if debited | writes agreed movements to an ephemeral account |
| `settle_receipt` | rollup | nobody | applies a receipt, hands it back to its author |
| `delegate_ledger` | basenet | owner + payer | hands the ledger to a rollup validator |
| `undelegate` | rollup | payer | commits the ledger back to basenet |
| `close_ledger` | basenet | owner + rent payer | sweeps everything out, closes the permission, refunds the rent |

The permission has no verbs of its own: it is created with the ledger and dies with it, so a
ledger and its privacy share one lifecycle and there is never a window in which a delegated
ledger sits readable. There is likewise no `commit_ledger`, for two reasons: commit is implicit
in `undelegate`, and a commit writes the ledger's current state to basenet, where it is
world-readable. On a private rollup its absence is what keeps the play-by-play inside the
validator — basenet sees one aggregate change when the session ends, never the states in
between.

### PDAs

```
ledger      ["ledger", owner]                    this program
reserve     ["vault"]                            this program
receipt     ["receipt", authority, nonce_le]     this program (ephemeral)
permission  ["permission:", ledger]              ACLseoPoyC3cBqoUtkbjZ4aDrkurZW86v19pXz2XQnp1
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
`amount` may be zero, which is how you open a ledger without funding it.

```js
const data = Buffer.concat([
  anchorDisc('deposit'),
  mint.toBuffer(),          // SOL is the System Program id
  u64(amount),
  Buffer.from([0]),         // Option<u16> min_free    — None
  Buffer.from([0]),         // Option<u16> slot_increase — None
]);

keys = [
  sg(owner), rw(ledger), rw(permission), ro(PERMISSION_PROGRAM), rw(reserve),
  rw(isSol ? SystemProgram.programId : ataOf(reserve, mint)),
  rw(isSol ? SystemProgram.programId : ataOf(sponsor, mint)),
  ro(TOKEN_PROGRAM), ro(SystemProgram.programId),
  sg(sponsor),                 // the owner itself for a self-deposit
  ro(ownerProgram),            // System Program as a placeholder when depositing to yourself
  ro(ownerProgramData),
];
```

`withdraw` mirrors it: a `receiver` signer followed by the same two program accounts.

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
System transfer. Hence `open_ledger`, which creates an empty one at a chosen size with the rent
paid by somebody else. Capped at 256 slots, since rent scales with the count and is paid up
front. It can be extended later with `grow_ledger`, funded by the same account that opened it —
`deposit` grows a wallet's ledger as it goes but refuses an off-curve owner, so that is the only
way a program's ledger grows.

Each ledger records a **rent payer**: where its rent goes when it closes, and the only account
that may grow it. A wallet funds its own ledger and nobody else may, because the rent comes back
to the owner and paying somebody's rent would otherwise be a way to hand them money. A PDA cannot
pay, so whoever does becomes the rent payer — the lamports return to them, which is what makes
sponsoring a program's ledger free of that problem.

The permission a PDA's ledger gets at `open_ledger` names `[owner, the program behind it]`, both
derived — the program is read off the ledger account's owner field, never passed. A program has
to reach its own ledgers to settle them, and the rollup's filter only ever sees an instruction's
top-level program, so a permission naming the PDA alone would not admit it.

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
and nobody else read it. A PDA's names the PDA and its program, as above.

The catch, established by testing rather than documentation:

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
program touching the ledgers, the two are separated into different instructions:

```
ix 0   program.request      →  CPI vault.create_receipt
                               writes {human, authority, movements[]} to an ephemeral account.
                               No ledger is present, so nothing is permissioned.

ix 1   vault.settle_receipt →  top-level, so the filter admits it.
                               Applies the movements, zeroes the receipt, and assigns it to
                               the program.

ix 2   program.deliver      →  the receipt is now the program's. That ownership *is* the proof
                               the settle happened — it could not have been given the receipt
                               otherwise. Consume it and reclaim the rent.
```

All three in one transaction, so it is atomic: nothing is delivered unpaid, and the payment
cannot happen without the delivery.

`settle_receipt` requires no signature. Consent was captured when the receipt was written — the
program signed for its PDA, and the human signed if any movement debits them. A receipt that only
credits the human took nothing from them and needed no consent, which is what lets a payout be
submitted by an unfunded throwaway key. Either way the receipt names both ledger owners, so
whoever submits it cannot substitute either side.

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
  lib.rs                    entrypoints
  state.rs                  Ledger, slot allocation, headroom, errors
  instructions/
    initialize_vault.rs
    open_ledger.rs          a PDA's ledger, with its permission
    grow_ledger.rs
    deposit.rs              creates a wallet's ledger and its permission; grows it
    withdraw.rs
    settle.rs               the never-two-humans rule
    receipt.rs              create_receipt, settle_receipt
    delegation.rs           delegate_ledger, undelegate
    close_ledger.rs
vault-program-spec.md       the long-form design notes
```
