//! Shared harness for the vault's end-to-end tests.
//!
//! Everything runs in-process through `solana-program-test` (BanksClient) against the freshly
//! compiled program — no validator, no deploy, no devnet funds. Accounts the vault reads are
//! fabricated directly with `set_account`, which lets us reach states an external CPI would
//! otherwise gate (a ledger already exists, a permission is already present, a receipt is
//! already open) without the MagicBlock programs being present.
//!
//! What this harness deliberately cannot reach — because their only/last step is a
//! validator-native CPI — is:
//!   - `create_receipt` (CPIs the ephemeral vault to mint the account),
//!   - `delegate` / the commit inside `undelegate` (delegation program + magic context),
//!   - the permission-program CPI that completes `open_*`, `make_*`, and a first-ever `deposit`.
//! Their *validation/rejection* paths are covered here; only the CPI completion is devnet-only.

#![allow(dead_code)]

use anchor_lang::solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program_error::ProgramError,
    program_option::COption, program_pack::Pack, pubkey::Pubkey,
};
use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas};
use anchor_spl::token::spl_token;
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    instruction::{AccountMeta, Instruction, InstructionError},
    rent::Rent,
    signature::{Keypair, Signer},
    transaction::{Transaction, TransactionError},
};
use vault::state::{Entry, Ledger, VaultError, SOL_MINT};

/// A stand-in "member program" that receives `settle_receipt`'s callback. It asserts the vault
/// authority signed the call (the real proof of payment) and flips a byte in the first forwarded
/// account, so a test can prove the callback actually ran.
pub const MEMBER_ID: Pubkey = Pubkey::new_from_array([10u8; 32]);

fn mock_member(_id: &Pubkey, accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    // accounts: [receipt, vault_authority, ...forwarded]
    if !accounts[1].is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if let Some(flag) = accounts.get(2) {
        let mut d = flag.try_borrow_mut_data()?;
        if !d.is_empty() {
            d[0] = 1;
        }
    }
    Ok(())
}

/// A generic `invoke_signed` forwarder, standing in for the member program that owns a PDA
/// ledger. It signs for one of its PDAs so the vault's `owner: Signer` PDA paths (grow, authorize,
/// program-side settle, closing a PDA ledger) can run without deploying a bespoke program.
///
/// data = [num_seeds][len,bytes]*[inner ix data]; accounts = [target_program, ...forwarded].
/// The account matching the seed-derived PDA is elevated to a signer for the inner call.
pub const PROXY_ID: Pubkey = Pubkey::new_from_array([11u8; 32]);

fn proxy(_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
    use anchor_lang::solana_program::program::invoke_signed;

    let n = data[0] as usize;
    let mut i = 1usize;
    let mut seeds: Vec<&[u8]> = Vec::with_capacity(n);
    for _ in 0..n {
        let l = data[i] as usize;
        i += 1;
        seeds.push(&data[i..i + l]);
        i += l;
    }
    let ix_data = data[i..].to_vec();

    let target = accounts[0].key;
    let pda = Pubkey::create_program_address(&seeds, &PROXY_ID).map_err(|_| ProgramError::InvalidSeeds)?;
    let metas: Vec<AccountMeta> = accounts[1..]
        .iter()
        .map(|a| AccountMeta {
            pubkey: *a.key,
            is_signer: a.is_signer || *a.key == pda,
            is_writable: a.is_writable,
        })
        .collect();
    let ix = Instruction { program_id: *target, accounts: metas, data: ix_data };
    invoke_signed(&ix, accounts, &[&seeds])
}

/// The off-curve PDA a proxy signs for, plus the full seeds (prefix + bump) it must be given.
pub fn proxy_pda(prefix: &[u8]) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[prefix], &PROXY_ID)
}

/// Wraps a vault instruction so the proxy signs for `pda` (derived from `prefix`). The PDA's own
/// signer bit is cleared on the outer transaction — the proxy re-elevates it internally.
pub fn via_proxy(prefix: &[u8], inner: Instruction) -> Instruction {
    let (pda, bump) = proxy_pda(prefix);
    let mut data = vec![2u8, prefix.len() as u8];
    data.extend_from_slice(prefix);
    data.push(1);
    data.push(bump);
    data.extend_from_slice(&inner.data);

    let mut metas = vec![AccountMeta::new_readonly(inner.program_id, false)];
    for m in &inner.accounts {
        metas.push(AccountMeta {
            pubkey: m.pubkey,
            is_signer: m.is_signer && m.pubkey != pda,
            is_writable: m.is_writable,
        });
    }
    Instruction { program_id: PROXY_ID, accounts: metas, data }
}

/// `processor!` expects a `ProcessInstruction` whose accounts-slice and `AccountInfo` lifetimes
/// are independent; Anchor's `entry` ties them to one `'info`. The runtime hands both the same
/// invocation lifetime, so collapsing them is sound — the standard bridge.
fn vault_entry(id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let accounts: &[AccountInfo] = unsafe { core::mem::transmute(accounts) };
    vault::entry(id, accounts, data)
}

pub fn program_test() -> ProgramTest {
    let mut pt = ProgramTest::new("vault", vault::ID, processor!(vault_entry));
    pt.add_program("mock_member", MEMBER_ID, processor!(mock_member));
    // The SPL token program: `Deposit`/`Withdraw` type `token_program` as `Program<Token>`, so a
    // real executable account must sit at its id even on the SOL path — and it runs the token
    // transfers for real on the SPL path.
    pt.add_program(
        "spl_token",
        spl_token::ID,
        processor!(spl_token::processor::Processor::process),
    );
    pt.add_program("proxy", PROXY_ID, processor!(proxy));
    // A no-op stub at the magic program id: the `#[commit]` macro checks it is executable during
    // account validation, which is before `undelegate`'s authority check. The real commit CPI is
    // validator-native and unreachable here, so only pre-CPI rejections are exercised against it.
    pt.add_program("magic", magic_program(), processor!(noop));
    pt
}

fn noop(_id: &Pubkey, _accounts: &[AccountInfo], _data: &[u8]) -> ProgramResult {
    Ok(())
}

// ── error helpers ────────────────────────────────────────────────────────────

pub fn vault_err(e: VaultError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

pub fn err_code(err: BanksClientError) -> u32 {
    match err {
        BanksClientError::TransactionError(TransactionError::InstructionError(
            _,
            InstructionError::Custom(c),
        )) => c,
        other => panic!("expected a custom instruction error, got {other:?}"),
    }
}

// ── account fabrication ──────────────────────────────────────────────────────

pub fn ledger_pda(owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"ledger", owner.as_ref()], &vault::ID)
}

pub fn vault_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"vault"], &vault::ID)
}

pub fn vault_authority() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[], &vault::ID)
}

/// The real MagicBlock permission program id — what the `address =` constraint on every
/// `permission_program` account now pins to.
pub fn permission_program() -> Pubkey {
    ephemeral_rollups_sdk::consts::PERMISSION_PROGRAM_ID
}

pub fn spl_token_id() -> Pubkey {
    spl_token::ID
}

#[allow(deprecated)]
pub fn system_program_id() -> Pubkey {
    solana_sdk::system_program::id()
}

pub fn magic_program() -> Pubkey {
    ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID
}

pub fn magic_context() -> Pubkey {
    ephemeral_rollups_sdk::consts::MAGIC_CONTEXT_ID
}

fn rent_exempt(len: usize) -> u64 {
    Rent::default().minimum_balance(len)
}

/// A program-owned ledger at its canonical PDA, with `slots` entries and whatever balances the
/// caller writes in `set`. Returned as `(pda, account)` ready for `set_account`.
pub fn make_ledger(
    owner: &Pubkey,
    pda_auth: bool,
    rent_payer: &Pubkey,
    authorized: Pubkey,
    slots: usize,
    set: impl FnOnce(&mut Ledger),
) -> (Pubkey, Account) {
    let (pda, bump) = ledger_pda(owner);
    let mut l = Ledger::default();
    l.init(*owner, pda_auth, bump, *rent_payer, slots);
    l.authorized = authorized;
    set(&mut l);

    let mut data = Vec::new();
    l.try_serialize(&mut data).unwrap();
    let account = Account {
        lamports: rent_exempt(data.len()),
        data,
        owner: vault::ID,
        executable: false,
        rent_epoch: 0,
    };
    (pda, account)
}

/// A plain System-owned account holding `lamports` — a funded wallet, or a placeholder.
pub fn system_account(lamports: u64) -> Account {
    Account {
        lamports,
        data: vec![],
        owner: solana_sdk::system_program::id(),
        executable: false,
        rent_epoch: 0,
    }
}

/// A non-empty account owned by an arbitrary program — used to stand in for an already-created
/// permission so `deposit` skips its creation CPI, or as the callback's writable flag.
pub fn opaque_account(owner: Pubkey, len: usize) -> Account {
    Account {
        lamports: rent_exempt(len),
        data: vec![0u8; len],
        owner,
        executable: false,
        rent_epoch: 0,
    }
}

/// A real SPL token account (initialized), so the token program accepts it in a transfer.
pub fn spl_token_account(mint: Pubkey, owner: Pubkey, amount: u64) -> Account {
    let mut data = vec![0u8; spl_token::state::Account::LEN];
    spl_token::state::Account {
        mint,
        owner,
        amount,
        delegate: COption::None,
        state: spl_token::state::AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    }
    .pack_into_slice(&mut data);
    Account {
        lamports: rent_exempt(data.len()),
        data,
        owner: spl_token::ID,
        executable: false,
        rent_epoch: 0,
    }
}

/// The vault's canonical reserve for a mint — its associated token account.
pub fn vault_reserve(mint: &Pubkey) -> Pubkey {
    let (vault, _) = vault_pda();
    anchor_spl::associated_token::get_associated_token_address(&vault, mint)
}

pub async fn token_balance(ctx: &mut ProgramTestContext, token_account: Pubkey) -> u64 {
    let acct = ctx.banks_client.get_account(token_account).await.unwrap().unwrap();
    spl_token::state::Account::unpack(&acct.data).unwrap().amount
}

// ── receipt fabrication (mirrors instructions/receipt.rs layout) ─────────────

pub const RECEIPT_HEADER: usize = 123;
pub const MOVEMENT_SIZE: usize = 44;
pub const OWNER_SIZE: usize = 32;
pub const RECEIPT_DISCRIMINATOR: [u8; 8] = [39, 154, 73, 106, 80, 102, 145, 153];

pub struct Mv {
    pub mint: Pubkey,
    pub amount: u64,
    pub from: u8,
    pub to: u8,
}

/// Builds the raw bytes of a receipt exactly as `create_receipt` would write them, so
/// `settle_receipt` can be exercised without the ephemeral-vault CPI that creates one.
pub fn receipt_bytes(
    owners: &[Pubkey],
    authority: &Pubkey,
    member: &Pubkey,
    disc: [u8; 8],
    slot: u64,
    movements: &[Mv],
    args: &[u8],
) -> Vec<u8> {
    let extra = owners.len() - 1;
    let len = RECEIPT_HEADER + extra * OWNER_SIZE + movements.len() * MOVEMENT_SIZE + args.len();
    let mut d = vec![0u8; len];
    d[0..8].copy_from_slice(&RECEIPT_DISCRIMINATOR);
    d[8..40].copy_from_slice(owners[0].as_ref());
    d[40..72].copy_from_slice(authority.as_ref());
    d[72..104].copy_from_slice(member.as_ref());
    d[104..112].copy_from_slice(&disc);
    d[112..120].copy_from_slice(&slot.to_le_bytes());
    d[120] = owners.len() as u8;
    d[121] = movements.len() as u8;
    d[122] = args.len() as u8;
    for (i, o) in owners.iter().skip(1).enumerate() {
        let at = RECEIPT_HEADER + i * OWNER_SIZE;
        d[at..at + 32].copy_from_slice(o.as_ref());
    }
    let mv = RECEIPT_HEADER + extra * OWNER_SIZE;
    for (i, m) in movements.iter().enumerate() {
        let o = mv + i * MOVEMENT_SIZE;
        d[o..o + 32].copy_from_slice(m.mint.as_ref());
        d[o + 32..o + 40].copy_from_slice(&m.amount.to_le_bytes());
        d[o + 40] = m.from;
        d[o + 41] = m.to;
    }
    let ao = mv + movements.len() * MOVEMENT_SIZE;
    d[ao..ao + args.len()].copy_from_slice(args);
    d
}

pub fn receipt_account(data: Vec<u8>) -> Account {
    Account {
        lamports: rent_exempt(data.len()),
        data,
        owner: vault::ID,
        executable: false,
        rent_epoch: 0,
    }
}

// ── instruction + transaction plumbing ───────────────────────────────────────

pub fn vault_ix<A: ToAccountMetas, D: InstructionData>(
    accounts: A,
    data: D,
    extra: &[AccountMeta],
) -> Instruction {
    let mut metas = accounts.to_account_metas(None);
    metas.extend_from_slice(extra);
    Instruction {
        program_id: vault::ID,
        accounts: metas,
        data: data.data(),
    }
}

/// Marks the named accounts writable on an already-built instruction. `deposit`/`withdraw` type
/// the token accounts as bare `UncheckedAccount`, so `to_account_metas` leaves them read-only;
/// the real client passes them writable, which is what the token-transfer CPI needs.
pub fn mark_writable(ix: &mut Instruction, keys: &[Pubkey]) {
    for m in ix.accounts.iter_mut() {
        if keys.contains(&m.pubkey) {
            m.is_writable = true;
        }
    }
}

pub async fn send(
    ctx: &mut ProgramTestContext,
    ix: Instruction,
    signers: &[&Keypair],
) -> Result<(), BanksClientError> {
    let mut tx = Transaction::new_with_payer(&[ix], Some(&ctx.payer.pubkey()));
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    // Fetch fresh each time so the helper keeps working across warp_to_slot.
    let blockhash = ctx
        .banks_client
        .get_latest_blockhash()
        .await
        .unwrap_or(ctx.last_blockhash);
    tx.sign(&all, blockhash);
    ctx.banks_client.process_transaction(tx).await
}

pub async fn read_ledger(ctx: &mut ProgramTestContext, pubkey: Pubkey) -> Ledger {
    let acct = ctx
        .banks_client
        .get_account(pubkey)
        .await
        .unwrap()
        .expect("ledger account missing");
    Ledger::try_deserialize(&mut &acct.data[..]).unwrap()
}

pub async fn lamports_of(ctx: &mut ProgramTestContext, pubkey: Pubkey) -> u64 {
    ctx.banks_client
        .get_account(pubkey)
        .await
        .unwrap()
        .map(|a| a.lamports)
        .unwrap_or(0)
}

pub async fn current_slot(ctx: &mut ProgramTestContext) -> u64 {
    ctx.banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap()
        .slot
}

/// Convenience for entries: SOL lives at slot 0, everything else is a token slot.
pub fn set_entry(l: &mut Ledger, index: usize, mint: Pubkey, amount: u64) {
    l.entries[index] = Entry { mint, amount };
}

pub fn token_mint(n: u8) -> Pubkey {
    Pubkey::new_from_array([n; 32])
}

pub fn sol_mint() -> Pubkey {
    SOL_MINT
}
