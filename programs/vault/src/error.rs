use solana_program::program_error::ProgramError;

/// Codes start at 6000 to match Anchor's `ERROR_CODE_OFFSET`, and the order is the exact order
/// of the old `#[error_code]` enum — clients decode specific codes, so this is a wire API and the
/// numbering must not move. (`settle_receipt` also emits off-enum codes `7000 + check*100 + index`
/// directly in its handler; those are not part of this enum.)
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
}

impl From<VaultError> for ProgramError {
    fn from(e: VaultError) -> Self {
        ProgramError::Custom(e as u32)
    }
}
