//! 32-byte content hash used for `config_hash` and `feature_hash`.

use std::{fmt, str::FromStr};

use crate::CoreError;

/// A 32-byte content hash (stored as `BYTEA` of length 32). Text form is 64 lowercase hex
/// characters. This type only carries and formats the bytes; it does not compute hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash32([u8; 32]);

impl Hash32 {
    /// Wraps raw hash bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Parses exactly 64 hex characters (upper or lower case).
///
/// Errors: [`CoreError::InvalidHashLength`] with the byte length when it is not 64 (checked
/// first), else [`CoreError::InvalidHashChar`] with the index of the first non-hex byte.
impl FromStr for Hash32 {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let _ = s;
        todo!("Phase 0 task T6")
    }
}

/// Writes 64 lowercase hex characters. Must not allocate: fill a `[u8; 64]` on the stack.
impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = f;
        todo!("Phase 0 task T6")
    }
}
