//! Session-family typed identifiers with frozen persistent grammars.
//!
//! Every identifier carries 128 operating-system CSPRNG bits in one canonical
//! spelling: a fixed ASCII prefix followed by exactly 32 lowercase hexadecimal
//! digits. Parsing accepts only that spelling, so an accepted identifier can
//! never contain a path separator, traversal, or non-portable byte.

// Rust guideline compliant 2026-09-18.

use std::fmt::{self, Display, Formatter};

const RANDOM_BYTES: usize = 16;
const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

/// Spelled byte length of one event identifier.
pub const EVENT_ID_LEN: usize = "evt1-".len() + RANDOM_BYTES * 2;
/// Spelled byte length of one call identifier.
pub const CALL_ID_LEN: usize = "call1-".len() + RANDOM_BYTES * 2;

macro_rules! session_id_type {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Generates a fresh identifier from the operating-system CSPRNG.
            ///
            /// # Errors
            ///
            /// Returns `None` when the operating-system random source cannot
            /// fill the required 16-byte buffer. Callers must treat failure as
            /// fail-closed and must not fall back to a weaker source.
            pub fn generate() -> Option<Self> {
                let mut random = [0_u8; RANDOM_BYTES];
                getrandom::fill(&mut random).ok()?;
                let mut spelling = String::with_capacity($prefix.len() + RANDOM_BYTES * 2);
                spelling.push_str($prefix);
                for byte in random {
                    spelling.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
                    spelling.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
                }
                Some(Self(spelling))
            }

            /// Parses the one canonical persistent spelling.
            #[must_use]
            pub fn parse(value: &str) -> Option<Self> {
                let suffix = value.strip_prefix($prefix)?;
                if suffix.len() != RANDOM_BYTES * 2
                    || !suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                {
                    return None;
                }
                Some(Self(value.to_owned()))
            }

            /// Returns the canonical persistent spelling.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

session_id_type!(
    SessionId,
    "ses1-",
    "Identifies one durable session ledger (`ses1-[0-9a-f]{32}`)."
);
session_id_type!(
    BranchId,
    "br1-",
    "Identifies one branch of a session ledger (`br1-[0-9a-f]{32}`)."
);
session_id_type!(
    SessionEventId,
    "evt1-",
    "Identifies one appended session event (`evt1-[0-9a-f]{32}`)."
);
session_id_type!(
    SessionCallId,
    "call1-",
    "Binds one tool-call/tool-result event pair (`call1-[0-9a-f]{32}`)."
);
session_id_type!(
    BranchReservationId,
    "sbr1-",
    "Identifies one single-use branch mutation reservation (`sbr1-[0-9a-f]{32}`)."
);

#[cfg(test)]
mod tests {
    use super::{
        BranchId, BranchReservationId, LOWER_HEX, RANDOM_BYTES, SessionCallId, SessionEventId,
        SessionId,
    };

    #[test]
    fn generated_ids_use_the_canonical_spelling() {
        let session = SessionId::generate().expect("session id");
        let branch = BranchId::generate().expect("branch id");
        let event = SessionEventId::generate().expect("event id");
        let call = SessionCallId::generate().expect("call id");
        let reservation = BranchReservationId::generate().expect("reservation id");
        for (value, prefix) in [
            (session.as_str(), "ses1-"),
            (branch.as_str(), "br1-"),
            (event.as_str(), "evt1-"),
            (call.as_str(), "call1-"),
            (reservation.as_str(), "sbr1-"),
        ] {
            let spelling = value;
            let suffix = spelling.strip_prefix(prefix).expect("fixed prefix");
            assert_eq!(suffix.len(), RANDOM_BYTES * 2);
            assert!(
                suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
            assert_eq!(
                SessionId::parse(spelling).is_some(),
                prefix == "ses1-",
                "{prefix} cross-type parse"
            );
        }
        assert!(SessionId::generate().expect("id") != SessionId::generate().expect("id"));
        assert_eq!(LOWER_HEX.len(), 16);
    }

    #[test]
    fn parse_accepts_only_the_exact_spelling() {
        let valid = "ses1-0123456789abcdef0123456789abcdef";
        assert!(SessionId::parse(valid).is_some());
        for (label, value) in [
            ("empty", ""),
            ("prefix only", "ses1-"),
            ("short", "ses1-0123456789abcdef0123456789abcde"),
            ("long", "ses1-0123456789abcdef0123456789abcdef0"),
            ("upper hex", "ses1-0123456789ABCDEF0123456789ABCDEF"),
            ("non hex", "ses1-0123456789abcdeg0123456789abcdeg"),
            ("wrong prefix", "ses2-0123456789abcdef0123456789abcdef"),
            ("no separator", "ses10123456789abcdef0123456789abcdef"),
            ("interior newline", "ses1-0123456789abcde\n0123456789abcdef"),
        ] {
            assert!(SessionId::parse(value).is_none(), "{label}: {value:?}");
            assert!(BranchId::parse(value).is_none(), "branch {label}");
            assert!(SessionEventId::parse(value).is_none(), "event {label}");
            assert!(SessionCallId::parse(value).is_none(), "call {label}");
            assert!(
                BranchReservationId::parse(value).is_none(),
                "reservation {label}"
            );
        }
        assert!(BranchId::parse("br1-0123456789abcdef0123456789abcdef").is_some());
        assert!(SessionEventId::parse("evt1-0123456789abcdef0123456789abcdef").is_some());
        assert!(SessionCallId::parse("call1-0123456789abcdef0123456789abcdef").is_some());
        assert!(BranchReservationId::parse("sbr1-0123456789abcdef0123456789abcdef").is_some());
    }
}
