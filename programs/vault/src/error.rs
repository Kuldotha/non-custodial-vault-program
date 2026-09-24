use solana_program::program_error::ProgramError;

/// Codes start at 6000 to match Anchor's `ERROR_CODE_OFFSET`. Clients decode specific codes, so
/// the numbering is a wire API and must not move: append only, never reorder. (`settle_receipt`
/// also emits off-enum codes `7000 + check*100 + index` directly in its handler; those are not
/// part of this enum.)
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum VaultError {
    LedgerFull = 6000,
    NoBalance,
    Insufficient,
    Overflow,
    NotProgramMediated,
    MissingProgramSignature,
    MissingUserSignature,
    InsufficientReserve,
    MintMismatch,
    BadSlotCount,
    PermissionFailed,
    PermissionExists,
    MissingTokenAccounts,
    NotCanonicalReserve,
    VaultNotInitialized,
    BadLedgerOwner,
    NoAuthorization,
    BadAuthority,
    OffCurveOwnerNotAllowed,
    MustFundOwnLedger,
    NotRentPayer,
    LedgerExists,
    NotAReceipt,
    OwnerNotWallet,
    OwnerNotPda,
    MemberProgramMismatch,
    CannotAuthorizePdaLedger,
    BadAuthorizedKey,
    NotAuthorizedToConsent,
    ReceiptLive,
    ReceiptExpired,
    ReceiptNotConsumed,
    CallbackProgramMismatch,
    DuplicateLedger,
    SlotClaimNeedsConsent,
    /// A temporary session key with an expiry already past.
    BadExpiry,
}

impl From<VaultError> for ProgramError {
    fn from(e: VaultError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
