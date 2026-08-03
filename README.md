# vault

A non-custodial, multi-asset balance program for Solana, built to work inside
[MagicBlock](https://magicblock.gg) ephemeral rollups — including **private** ones, where the
validator runs in a TEE and account data is not world-readable.

The problem it solves: a game wants to take a fee and pay a prize, thousands of times, without a
basenet transaction per action and without ever holding the player's money. The usual answer is a
custodial escrow account per game. This isn't that.

> **Status: devnet.** Deployed at `9vDAQgdHWCPQZabumgcuwoSLzWnRyQkSM1EHQnW8YXjs`, upgrade
> authority **not** burned. Unaudited. Don't put real money in it.

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

Everything else follows from three rules:

1. **A withdrawal's destination is derived from the signer, never passed.** There is no way to
   spell a withdrawal to somebody else, so a bug in a calling program cannot redirect funds.
2. **The debited side authorises.** A human authorises by signing. A program authorises by
   `invoke_signed` over its own seeds, which it can only do for a ledger it owns. So a program
   cannot debit a human, and cannot debit another program. A *credit* needs no signature, which
   is what lets a game pay out to a player who has closed the app.
3. **At most one human ledger in a movement.** Two human ledgers may never meet, in either
   direction.
4. **A program's ledger is never deposited to or withdrawn from.** Off-curve owners are
   refused by both instructions. Value reaches a program only through `settle`.

### Rule 3 is the one that matters most

It is what makes this not a payments program: **no instruction here takes value from one person
and gives it to another.** `withdraw` sends only to the ledger's owner and `deposit`
credits only the depositor — both refuse a program's ledger outright — so `settle` is the only
place two ledgers touch at all, and refusing two human sides there closes the last door.

Stated precisely, because the honest version is narrower than the slogan: the vault exposes no
peer-to-peer instruction. It cannot stop value moving between people *through* primitives Solana
provides regardless. Somebody can deploy a stub program, settle into its ledger, hand the upgrade
authority to another wallet with `set_authority`, and let them upgrade it into something that
settles the balance back out to themselves. That is a transfer, and it is available whether or not
this program exists — `solana program close --recipient` pays programdata rent to any address, and
System transfers move SOL between wallets all day. Any program holding balances composes with
those, because whoever controls a program controls what its treasury does.

The vault does not try to prevent it, and could not: it cannot tell a game from a stub, and the
route runs through the loader rather than through anything here. What it can do — and does — is
offer no instruction of its own that moves value from one person to another.

That has consequences well beyond the code. A program that lets strangers pay each other is doing
something quite different, legally, from one that lets each person move their own balance in and
out and spend it with a program. This vault is deliberately the second thing. Peer-to-peer payment
belongs in a separate program, written by someone who has decided to take that on.

The check is therefore asymmetric on purpose:

```rust
require!(src.pda_auth || dst.pda_auth, VaultError::NotProgramMediated);
```

It looks like it wants to be `src.pda_auth != dst.pda_auth`, or like the signature rule alone
should be enough. Neither is true. The XOR forbids program-to-program movement, which is
legitimate — a game moving a share of each sale into a jackpot account it cannot later drain.
"Whoever is debited signs", on its own, makes Alice-pays-Bob expressible in a single instruction.
Only the pair of rules gives both properties, and a tidy-up that merges them removes one silently.

---

## Instructions

| instruction | where | who signs | what it does |
|---|---|---|---|
| `initialize_vault` | basenet | upgrade authority | creates the reserve, once |
| `open_ledger` | basenet | owner + payer | opens an empty ledger at a chosen size (max 256 slots); the only way a PDA gets one |
| `grow_ledger` | basenet | owner + rent payer | adds slots to an existing ledger |
| `open_permission` | basenet | owner + payer | makes a ledger private. Optional — only needed before delegating |
| `close_permission` | basenet | owner + rent payer | gives that privacy up again |
| `deposit` | basenet | owner | wallet → ledger; opens the ledger and its permission on first use. Wallets only |
| `withdraw` | basenet | owner | ledger → wallet, same owner only. Wallets only |
| `settle` | either | the debited side | moves a balance between two ledgers, at most one of them human |
| `create_receipt` | rollup | human + program | writes an agreed set of movements to an ephemeral account |
| `settle_receipt` | rollup | nobody | applies a receipt, then hands it back to its author |
| `delegate_ledger` | basenet | payer + owner | hands the ledger to a rollup validator |
| `undelegate` | rollup | payer | commits the ledger back to basenet |
| `close_ledger` | basenet | owner + rent payer | sweeps everything out and refunds all rent |

There is deliberately no `create_permission` and no `commit_ledger`. A wallet's ledger
is created by the deposit that first needs it, grows from inside `deposit`, and gets its
permission at creation; commit is implicit in `undelegate`. `open_ledger` exists only because a
PDA can do none of that for itself.

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

A program can own ledgers — as many as it has PDAs. A game's house and jackpot are two ledgers of
the same program.

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
paid by somebody else. Capped at 256 slots, since rent scales with the count and is paid up front. It can be extended
later with `grow_ledger`, funded by the same account that opened it — `deposit` grows a wallet's
ledger as it goes but refuses an off-curve owner, so that is the only way a program's ledger
grows.

Each ledger records a **rent payer**: where its rent goes when it closes, and the only account
that may grow it. A wallet funds its own ledger and nobody else may, because the rent comes back
to the owner and paying somebody's rent would otherwise be a way to hand them money. A PDA cannot
pay, so whoever does becomes the rent payer — the lamports return to them, which is what makes
sponsoring a program's ledger free of that problem.

`open_permission` creates its permission as `[owner, the program behind it]`, both derived — the program read off
the account's owner field, never passed. A program has to reach its own ledgers to settle them,
and the rollup's filter only ever sees an instruction's top-level program, so a PDA in that list
would not admit it. The payer is deliberately absent: paying rent for an empty account credits
nobody and buys no visibility.

That is what a jackpot is built from. The game settles into `["jackpot"]` on every sale and out of
it only on a win, and simply never writes an instruction that settles it anywhere else. There is
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

In a private rollup, an account can carry a **permission** listing who may see it. A ledger's
permission names exactly one member: its owner. That is what lets a player read their own balance
and nobody else read it.

The catch, established by testing rather than documentation:

> A transaction touching a permissioned account is refused at submission unless the
> **top-level program of that instruction** is a member of the permission. Submitter identity and
> signatures are never consulted. CPI'd programs are invisible, because the filter runs before
> execution.

The vault is a member of every ledger by virtue of owning it. A *game* CPI-ing into the vault is
not — and cannot be, without every ledger enumerating every game in advance (the permission
account holds 16 members, fixed at creation).

MagicBlock's position is correct and forced: any program handed an account can read its bytes and
copy them out, and CPI hands data over on entry. There is no "pass through without reading".

**The receipt is the way around it without weakening any of that.** Instead of the game calling
the vault with the ledgers, the two are separated into different instructions:

```
ix 0   game.request_purchase   →  CPI vault.create_receipt
                                  writes {human, authority, movements[]} to an ephemeral account.
                                  No ledger is present, so nothing is permissioned.

ix 1   vault.settle_receipt     →  top-level, so the filter admits it.
                                  Applies the movements, zeroes the receipt, and assigns it to
                                  the game.

ix 2   game.buy_card            →  the receipt is now game-owned. That ownership *is* the proof
                                  the settle happened — the game could not have been given it
                                  otherwise. Consume it and reclaim the rent.
```

All three in one transaction, so it is atomic: the card cannot exist unpaid, and the payment
cannot happen without the card.

`settle_receipt` requires no signature. Both parties consented when the receipt was written — the
human signed, the program signed for its PDA — and the receipt names both ledger owners, so
whoever submits it cannot substitute either side.

---

## Build, deploy

```bash
cargo build-sbf
solana program deploy target/deploy/vault.so \
  --program-id target/deploy/vault-keypair.json \
  --upgrade-authority <authority.json> --url devnet
```

Verify the deploy landed — a failed deploy prints a signature and leaves the old binary live:

```bash
solana program dump <PROGRAM_ID> /tmp/onchain.so --url devnet
head -c "$(stat -f%z target/deploy/vault.so)" /tmp/onchain.so | shasum -a256
shasum -a256 target/deploy/vault.so
```

Check for orphaned buffer accounts afterwards; a failed upgrade strands their rent:

```bash
solana program show --buffers --url devnet -k <authority.json>
```

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
    deposit.rs              creates the ledger and its permission; grows it
    withdraw.rs
    settle.rs               the human/program XOR
    receipt.rs              create_receipt, settle_receipt
    delegation.rs           delegate_ledger, undelegate
    close_ledger.rs
vault-program-spec.md       the long-form design notes
```
