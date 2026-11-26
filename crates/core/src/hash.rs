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

/// Lowercase digit table used by [`Hash32`]'s `Display` impl.
const HEX: &[u8; 16] = b"0123456789abcdef";

/// The value of one hex digit (either case), or `None` if the byte is not a hex digit.
const fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parses exactly 64 hex characters (upper or lower case).
///
/// Errors: [`CoreError::InvalidHashLength`] with the byte length when it is not 64 (checked
/// first), else [`CoreError::InvalidHashChar`] with the index of the first non-hex byte.
impl FromStr for Hash32 {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        let len = bytes.len();
        if len != 64 {
            return Err(CoreError::InvalidHashLength(len));
        }

        let (pairs, _) = bytes.as_chunks::<2>();
        let mut out = [0u8; 32];
        for (i, (slot, &[hi_byte, lo_byte])) in out.iter_mut().zip(pairs).enumerate() {
            let hi = hex_value(hi_byte).ok_or(CoreError::InvalidHashChar(2 * i))?;
            let lo = hex_value(lo_byte).ok_or(CoreError::InvalidHashChar(2 * i + 1))?;
            *slot = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

/// Writes 64 lowercase hex characters. Must not allocate: fill a `[u8; 64]` on the stack.
impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u8; 64];
        let (pairs, _) = buf.as_chunks_mut::<2>();
        for (pair, &b) in pairs.iter_mut().zip(self.0.iter()) {
            let [hi, lo] = pair;
            *hi = HEX[usize::from(b >> 4)];
            *lo = HEX[usize::from(b & 0x0f)];
        }
        let text = std::str::from_utf8(&buf).map_err(|_| fmt::Error)?;
        f.write_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequential() -> [u8; 32] {
        std::array::from_fn(|i| i as u8)
    }

    #[test]
    fn parses_lowercase() {
        let text: String = (0u8..32).map(|b| format!("{b:02x}")).collect();
        assert_eq!(text.parse::<Hash32>(), Ok(Hash32::from_bytes(sequential())));
    }

    #[test]
    fn parses_uppercase_and_formats_lowercase() {
        let upper = "AB".repeat(32);
        let parsed: Hash32 = upper.parse().unwrap();
        assert_eq!(parsed, Hash32::from_bytes([0xab; 32]));
        assert_eq!(parsed.to_string(), "ab".repeat(32));
    }

    #[test]
    fn display_round_trips() {
        for bytes in [sequential(), [0xff; 32], [0; 32]] {
            let h = Hash32::from_bytes(bytes);
            let text = h.to_string();
            assert_eq!(text.len(), 64);
            assert_eq!(text.parse::<Hash32>(), Ok(h));
        }
    }

    #[test]
    fn rejects_wrong_length() {
        assert_eq!("".parse::<Hash32>(), Err(CoreError::InvalidHashLength(0)));
        assert_eq!(
            "0".repeat(63).parse::<Hash32>(),
            Err(CoreError::InvalidHashLength(63))
        );
        assert_eq!(
            "0".repeat(65).parse::<Hash32>(),
            Err(CoreError::InvalidHashLength(65))
        );
    }

    #[test]
    fn rejects_non_hex_with_index() {
        let mut bad = "0".repeat(64);
        bad.replace_range(0..1, "g");
        assert_eq!(bad.parse::<Hash32>(), Err(CoreError::InvalidHashChar(0)));

        let mut bad = "0".repeat(64);
        bad.replace_range(63..64, "z");
        assert_eq!(bad.parse::<Hash32>(), Err(CoreError::InvalidHashChar(63)));

        let mut bad = "0".repeat(64);
        bad.replace_range(10..11, " ");
        assert_eq!(bad.parse::<Hash32>(), Err(CoreError::InvalidHashChar(10)));
    }

    #[test]
    fn length_is_checked_before_characters() {
        assert_eq!("zz".parse::<Hash32>(), Err(CoreError::InvalidHashLength(2)));
    }

    #[test]
    fn multibyte_input_is_rejected_not_panicking() {
        // 32 chars of 'é' are 64 bytes; the first byte (0xC3) is not a hex digit.
        let thirty_two = "é".repeat(32);
        assert_eq!(thirty_two.len(), 64);
        assert_eq!(
            thirty_two.parse::<Hash32>(),
            Err(CoreError::InvalidHashChar(0))
        );

        // 64 chars of 'é' are 128 bytes.
        let sixty_four = "é".repeat(64);
        assert_eq!(
            sixty_four.parse::<Hash32>(),
            Err(CoreError::InvalidHashLength(128))
        );
    }

    #[test]
    fn as_bytes_returns_input() {
        let x = sequential();
        assert_eq!(Hash32::from_bytes(x).as_bytes(), &x);
    }
}
