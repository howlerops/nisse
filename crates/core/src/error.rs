/// Errors from parsing or validating the shared domain types.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    /// A hash string was not exactly 64 hex characters long.
    #[error("hash must be 64 hex characters, got {0}")]
    InvalidHashLength(usize),
    /// A hash string held a non-hex character at this byte index.
    #[error("hash has a non-hex character at index {0}")]
    InvalidHashChar(usize),
    /// A string did not name a variant of a store enum ([`crate::Action`] and friends).
    #[error("unknown enum value: {0}")]
    UnknownVariant(#[from] strum::ParseError),
    /// Sessions handed to [`crate::MarketCalendar::new`] were empty, unordered or overlapping.
    #[error("invalid calendar: {0}")]
    InvalidCalendar(&'static str),
    /// A stored features value was not an object of decimal strings.
    #[error("invalid features: {0}")]
    InvalidFeatures(String),
    /// A signing key or a signed manifest is malformed (not what was signed is a different
    /// error: [`Self::BadSignature`]).
    #[error("invalid signing input: {0}")]
    InvalidSigning(String),
    /// A manifest's signature does not verify its contents under the key given.
    #[error("the manifest's signature does not verify")]
    BadSignature,
}
