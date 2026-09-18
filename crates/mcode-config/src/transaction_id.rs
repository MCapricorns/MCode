//! Defines Host-generated staging transaction identifiers.
//!
//! Transaction identifiers contain 128 operating-system CSPRNG bits and have
//! one persistent lowercase hexadecimal spelling. This module intentionally
//! exposes generation and formatting but no public parsing or raw construction.

use std::fmt::{self, Display, Formatter};

use crate::{ConfigError, ConfigErrorKind};

const RANDOM_BYTES: usize = 16;
const TRANSACTION_ID_PREFIX: &str = "tx1-";
const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

/// Identifies one Host-created staging transaction.
///
/// Values are generated from 128 operating-system CSPRNG bits and always use
/// the spelling `tx1-` followed by exactly 32 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransactionId(String);

impl TransactionId {
    /// Generates a transaction identifier from the operating-system CSPRNG.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::Random`] when the operating-system random
    /// source cannot fill the required 16-byte buffer.
    pub fn generate() -> Result<Self, ConfigError> {
        let mut random = [0_u8; RANDOM_BYTES];
        getrandom::fill(&mut random).map_err(|_| ConfigError::new(ConfigErrorKind::Random))?;

        let mut spelling = String::with_capacity(TRANSACTION_ID_PREFIX.len() + RANDOM_BYTES * 2);
        spelling.push_str(TRANSACTION_ID_PREFIX);
        for byte in random {
            spelling.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
            spelling.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
        }
        Ok(Self(spelling))
    }

    /// Returns the canonical persistent spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(any(unix, windows, test))]
    pub(crate) fn parse_persistent(value: &str) -> Option<Self> {
        let suffix = value.strip_prefix(TRANSACTION_ID_PREFIX)?;
        if suffix.len() != RANDOM_BYTES * 2
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return None;
        }
        Some(Self(value.to_owned()))
    }
}

impl Display for TransactionId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::TransactionId;

    #[test]
    fn persistent_parser_accepts_only_the_canonical_spelling() {
        let valid = "tx1-0123456789abcdef0123456789abcdef";
        assert_eq!(
            TransactionId::parse_persistent(valid)
                .expect("canonical transaction ID")
                .as_str(),
            valid
        );
        for invalid in [
            "0123456789abcdef0123456789abcdef",
            "tx1-0123456789abcdef0123456789abcde",
            "tx1-0123456789abcdef0123456789abcdef0",
            "tx1-0123456789abcdef0123456789abcdeg",
            "tx1-0123456789abcdef0123456789abcdeF",
            "tx2-0123456789abcdef0123456789abcdef",
        ] {
            assert!(
                TransactionId::parse_persistent(invalid).is_none(),
                "{invalid}"
            );
        }
    }
}
