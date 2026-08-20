# Vault: Anchor → native migration plan

Status: **planned, not started.** This doc is a handoff so another session can pick the work up.
The vault currently compiles and all 37 tests pass on Anchor `0.31.1`. The goal is to remove
Anchor and restructure the program to match the author's other native programs
(`scratch-cards-program`, `dark-galaxy`), without changing what the program *does*.

Read this whole doc before writing code. The mechanical port is easy; the risk is entirely in
**wire/on-chain compatibility with an already-deployed member program**, covered in §1.

---

## 0. Reference: how the native programs are built

`scratch-cards-program` (in the working set at `/Users/tedosijses/projects/scratch-cards-program`)
is the template. It is *already a member program of the vault*, so it also demonstrates every
ER-SDK-without-Anchor call the vault needs. Study these files first:

- `src/entrypoint.rs` — `entrypoint!(process_instruction)` → `instruction::dispatch`.
- `src/instruction.rs` — the `instructions! { … }` macro: a borsh `enum Instruction`, a
  `ProcessInstruction` trait (`fn process(&self, program_id, accounts) -> ProgramResult`), a dense
  **append-only** variant list whose position is the wire discriminator, an `ix` module of index
  constants, and a `dispatch` that special-cases the delegation program's fixed 8-byte
  `UNDELEGATE_DISC`.
- `src/error.rs` — `#[repr(u32)] enum GameError` → `ProgramError::Custom(e as u32)`.
- `src/state/config.rs` — `bytemuck` `#[repr(C)]` `Pod`/`Zeroable` structs cast straight from
  account bytes with `try_from_bytes`/`try_from_bytes_mut`; a Pod header (`discriminator: u64`,
  `version: u64`, …) followed by fixed-size records appended in the same account and cast per
  index; compile-time `assert!(size_of::<…>() % 8 == 0)` guards.
- `src/utils/pda.rs` — `validate(program_id, account, seeds) -> bump`, `close(receiver, account)`,
  `transfer_owned`, `min_balance(space)`.
- `src/instructions/*.rs` — each handler is a `#[derive(BorshDeserialize)] struct` + an
  `impl ProcessInstruction`. Accounts are destructured positionally
  (`let [a, b, c, ..] = accounts else { return Err(NotEnoughAccountKeys) }`), signer/PDA checks are
  explicit, no macro magic.
- `src/instructions/delegation.rs`, `delegate_treasury.rs`, `undelegate_treasury.rs` — native ER
  delegation via `ephemeral_rollups_sdk::cpi::{delegate_account, undelegate_account,
  DelegateAccounts, DelegateConfig}` and `ephem::commit_and_undelegate_accounts`.
- `src/utils/vault.rs` — the CPI shims into *this* vault. **This is the file that pins us to the
  current wire format** (see §1).
- Tests: `tests/*.rs` use `mollusk-svm` (not `solana-program-test`), `Cargo.toml` dev-dep.

`Cargo.toml` shape to mirror:

```toml
[dependencies]
solana-program = "3"
solana-system-interface = "2"
ephemeral-rollups-sdk = { version = "0.14.x", features = ["modular-sdk"] } # + access-control
borsh = { version = "1", features = ["derive"] }
bytemuck = { version = "1", features = ["derive", "min_const_generics"] }
spl-token = { version = "…", features = ["no-entrypoint"] }   # vault touches SPL reserves

[profile.release]
opt-level = "s"   # "s", not "z" — see scratch-cards note about the 4096-byte stack frame
codegen-units = 1
panic = "abort"
strip = true
```

Note the ER SDK feature: scratch-cards uses `modular-sdk`; the vault additionally needs
`access-control` (for `CreatePermissionCpiBuilder`/`ClosePermissionCpiBuilder`) and the
`ephemeral_accounts::EphemeralAccount` + `consts::{EPHEMERAL_VAULT_ID, PERMISSION_PROGRAM_ID,
MAGIC_PROGRAM_ID, MAGIC_CONTEXT_ID}` items it already imports. Confirm the non-anchor feature set
exposes all of these when wiring up the manifest.

---

## 1. The compatibility constraint (read before choosing an approach)

The vault (`9vDAQgdHWCPQZabumgcuwoSLzWnRyQkSM1EHQnW8YXjs`) is deployed and **`scratch-cards-program`
CPIs into it with three hard dependencies on the current Anchor wire format**, all in
`scratch-cards-program/src/utils/vault.rs`:

1. **Instruction discriminators** are Anchor's `sha256("global:<name>")[..8]`, hardcoded as consts:
   `SETTLE_DISC`, `DEPOSIT_DISC`, `DELEGATE_LEDGER_DISC`, `UNDELEGATE_DISC`, `WITHDRAW_DISC`,
   `CLOSE_LEDGER_DISC`, `OPEN_PDA_LEDGER_DISC`, `MAKE_PUBLIC_DISC`, `MAKE_PDA_LEDGER_PRIVATE_DISC`.
2. **Per-instruction account orderings** are replicated by hand in each CPI builder. A native
   rewrite must keep the *same account order per instruction* or update these in lockstep.
3. **A raw byte offset into `Ledger`**: `SOL_AMOUNT_AT = 116 + 32 = 148` — Anchor's 8-byte account
   discriminator + `Ledger::HEADER`(116) puts slot-0's amount at 148. Any change to the `Ledger`
   serialization (discriminator, header, or the `Vec<Entry>` length prefix) moves this and silently
   returns a wrong SOL balance to the member.

The **receipt** is *not* an Anchor account — it is already hand-serialized raw bytes
(`instructions/receipt.rs`, 8-byte `RECEIPT_DISCRIMINATOR` + fixed offsets), so it ports verbatim
with no compat concern.

Also note the deploy-atomicity item from the recent audit: the receipt layout just grew 7 bytes
(header 116→123). No receipts exist on-chain, so it's safe, but don't let an old build create a
receipt a new build would misread.

### Decision still open (author to confirm)

The author was asked to choose between two approaches and instead redirected to writing this doc,
so **the approach is not yet chosen.** The two coherent options:

- **(A) Wire-compatible drop-in (recommended).** Keep the program ID, keep the Anchor
  discriminators (the native `dispatch` matches the 8-byte `sha256("global:name")` values, exactly
  the way scratch-cards already special-cases `UNDELEGATE_DISC`), keep every per-instruction account
  order, and **replicate the exact `Ledger` on-chain bytes** (8-byte discriminator `[43,41,21,213,
  180,176,95,32]`, then the current borsh header + `Vec<Entry>` length-prefixed layout) in native
  read/write helpers. Result: `scratch-cards-program`, existing devnet ledgers, and any clients keep
  working untouched; this is *purely* an internal rewrite. Cost: the dispatcher matches sha256
  discriminators rather than a dense u64 index, so it's marginally less "clean" than scratch-cards.

- **(B) Clean native scheme.** Adopt scratch-cards' conventions fully — dense u64-index
  discriminators and a clean Pod `Ledger` (Pod header with `discriminator`/`version`, `Entry`
  records appended and cast per index like `Config`/`CardConfig`). Reads best and matches the other
  programs exactly. Cost: must update `scratch-cards-program/src/utils/vault.rs` (all discriminators,
  account orders, and the offset-148 read) in lockstep, and treat existing devnet ledgers as
  abandoned — **close them first to reclaim rent** (test funds are finite and unreplenishable;
  see the mainnet-readiness / dev-environment memories), then deploy fresh.

Recommendation: **(A)** unless the author specifically wants the u64-index scheme, because the vault
is a shared primitive with a live consumer and (A) needs zero coordination. Under (A) the `Ledger`
can still shed Anchor: model it like `Config` — a Pod header cast from bytes + `Entry` records — but
choose the header's field order/sizes so the serialized bytes are **identical** to today's Anchor
layout (including the 8-byte discriminator value and the 4-byte vec-length prefix). That keeps
offset 148 valid while removing the Anchor dependency.

---

## 2. Anchor → native mapping (per concept)

| Anchor thing (current) | Native replacement |
|---|---|
| `#[program]` mod + `declare_id!` | `entrypoint!` in `entrypoint.rs`, `declare_id!`, `instruction::dispatch` |
| `#[derive(Accounts)]` structs | positional `let [..] = accounts else { Err(NotEnoughAccountKeys) }` + explicit checks |
| instruction args (handler params) | `#[derive(BorshDeserialize)]` struct fields |
| `#[account(seeds=…, bump)]` | `pda::validate(program_id, acct, seeds)` returning the bump |
| `has_one`, `constraint`, `address=` | explicit `if key != … { Err(..) }` guards |
| `Account<'info, Ledger>` (de/serialize) | `Ledger::load`/`load_mut` reading bytes (see §1 for layout) |
| `#[account]` 8-byte discriminator | keep the same 8 bytes under (A); Pod `u64` disc under (B) |
| `#[error_code] enum VaultError` | `#[repr(u32)] enum VaultError` → `ProgramError::Custom`. **Preserve the exact numeric codes** — the receipt-settle path also emits off-enum codes `7000 + check*100 + index` that clients read; keep those. Anchor offsets custom errors by `ERROR_CODE_OFFSET (6000)` — decide whether to keep that offset (matters for any client matching specific codes). |
| `#[ephemeral]` (injects undelegate handler) | an `Undelegate` variant + `UNDELEGATE_DISC` special-case in `dispatch` (copy scratch-cards) |
| `#[delegate]` accounts | `cpi::delegate_account(DelegateAccounts{…}, seeds, DelegateConfig{…})` |
| `#[commit]` accounts | `ephem::commit_and_undelegate_accounts(payer, vec![pda], magic_context, magic_program, None)` |
| `EphemeralAccount` (receipt mint/resize) | unchanged — same SDK type, takes `AccountInfo`s |
| `CreatePermissionCpiBuilder` / `ClosePermissionCpiBuilder` | unchanged — same builders, `access-control` feature |
| `anchor_spl::token::{transfer, …}` | `spl_token::instruction::transfer` + `invoke`/`invoke_signed` |
| `anchor_lang::system_program::{transfer, allocate, assign}` | `solana_system_interface`/`solana_program::system_instruction` + `invoke_signed` |
| `Rent::get()`, `Clock::get()` | identical (`solana_program::sysvar`) |
| `solana_curve25519` `is_pda` | identical — no Anchor involved |
| tests: `solana-program-test` + `BanksClient` | `mollusk-svm` (see scratch-cards `tests/`) |

Key manual-work items already solved in the current code that carry over unchanged in spirit:
`create_ledger_account_sized` (transfer→allocate→assign to tolerate a pre-funded PDA), `ensure_headroom`
(realloc capped at `MAX_SLOT_INCREASE`=256 = 10 KiB), `require_reserve` (ATA derivation check),
`token_fields` (raw SPL account read), the receipt raw-byte layout, and the settle callback's
`assign`→`invoke_signed` handoff. Port these as plain functions in `utils/`.

---

## 3. Proposed file layout (mirrors scratch-cards)

```
programs/vault/  (or flatten to repo root like scratch-cards — author's call)
  src/
    entrypoint.rs        declare_id + entrypoint! + dispatch call
    instruction.rs       instructions!{…} enum, ProcessInstruction, dispatch (+ UNDELEGATE_DISC)
    error.rs             #[repr(u32)] VaultError  (preserve numeric codes)
    constants.rs         SOL_MINT, DEFAULT_SLOTS, MAX_SLOTS, MAX_SLOT_INCREASE, seeds, ids
    state/
      mod.rs
      ledger.rs          Ledger (Pod header + Entry records; layout per §1), Entry
      receipt.rs         receipt offsets + RECEIPT_DISCRIMINATOR + read/write (from instructions/receipt.rs)
    utils/
      pda.rs             validate / close / min_balance / transfer_owned
      account.rs         create_ledger_account_sized, ensure_headroom, load/store ledger
      reserve.rs         require_reserve, token_fields, vault_floor
      spl.rs             token transfer helpers
      permission.rs      create/close permission CPI wrappers
    instructions/
      initialize_vault.rs open_wallet_ledger.rs open_pda_ledger.rs grow_pda_ledger.rs
      deposit.rs withdraw.rs settle.rs
      create_receipt.rs settle_receipt.rs
      authorize.rs (assign_ledger_authorization + authorize_pda_ledger)
      privacy.rs (make_public + make_wallet_ledger_private + make_pda_ledger_private)
      close_ledger.rs delegation.rs (delegate_ledger + undelegate)
  tests/  (mollusk-svm rewrites of settle / deposit_withdraw / lifecycle / pda_paths / receipt)
```

Instruction → wire-discriminator table: under **(A)** the dispatcher must map each Anchor
`sha256("global:<name>")[..8]` to its handler. The nine the member program uses are already
enumerated as consts in `scratch-cards-program/src/utils/vault.rs` — copy them and compute the
rest (`initialize_vault`, `open_wallet_ledger`, `grow_pda_ledger`, `make_wallet_ledger_private`,
`assign_ledger_authorization`, `authorize_pda_ledger`, `create_receipt`, `settle_receipt`,
`delegate_ledger`) with `sha256("global:<snake_name>")[..8]`.

---

## 4. Order of work

1. Manifest + `entrypoint.rs` + `error.rs` + `constants.rs`; get an empty program to compile SBF.
2. `state/ledger.rs` with a **byte-exactness test** against the current Anchor serialization
   (fabricate a `Ledger` both ways, assert identical bytes) — this is the single most important test
   under approach (A). `state/receipt.rs` ported from `instructions/receipt.rs`.
3. `utils/*` helpers.
4. Instruction handlers, simplest first: `initialize_vault`, `open_*`, `deposit`/`withdraw`,
   `settle`, then `create_receipt`/`settle_receipt`, then delegation/privacy/close.
5. Port tests to mollusk one file at a time; keep asserting the same error codes.
6. Diff-audit against the Anchor version: for each instruction, confirm identical account order,
   identical checks, identical error codes.
7. Re-run the cross-program check: build `scratch-cards-program` against the new vault and run its
   vault-touching tests unchanged (under (A) they must pass with zero edits).

## 5. Watch-outs

- **Account order is load-bearing** under (A). The member replicates it by hand; a reordered field
  in any `Accounts` struct silently breaks a live CPI. Keep an explicit per-instruction order list.
- **Error codes are an API.** The private-rollup settle path deliberately encodes *which ledger
  failed which check* into off-enum custom codes (`7000 + check*100 + index`); clients decode these.
  Preserve them exactly, and decide on the `6000` Anchor offset for enum errors.
- **The `Ledger` `Vec<Entry>` length prefix.** Anchor writes a 4-byte little-endian length. If the
  native `Ledger` models capacity as, say, a `u64`, the bytes shift and offset 148 breaks. Match it.
- **`opt-level = "s"`, not `"z"`** — see the profile note copied from scratch-cards (a `z` build
  pushed two functions past sBPF's 4096-byte stack frame → UB).
- **Binary size is a correctness constraint on the rollup**: the ER clones the program at the size it
  first saw and does not re-clone when it grows. Keep the native binary ≤ the current cloned
  allocation, or re-clone deliberately.
- The permission account in `deposit` is intentionally unbound (validated by the permission
  program's CPI); keep that behavior and the comment.

## 6. Definition of done

- No `anchor-lang` / `anchor-spl` dependency anywhere in the vault crate.
- Structure and idioms match `scratch-cards-program`.
- All behaviors preserved; mollusk tests cover what the current 37 tests cover, same error codes.
- Under (A): `scratch-cards-program` builds and its vault CPIs pass with **no** edits.
  Under (B): `scratch-cards-program/src/utils/vault.rs` updated in the same change and its tests pass.
