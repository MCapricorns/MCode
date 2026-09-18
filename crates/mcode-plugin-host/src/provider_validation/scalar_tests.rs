//! Scalar and text validator tests.
use super::scalar::{digest, label, safe, stamp, tracking_id, visible_ascii};

#[test]
fn safe_text_accepts_allowed_whitespace_and_rejects_controls_and_bidi() {
    assert!(safe("line\tline\n雪", 64, false).is_ok());
    for invalid in ["\0", "\r", "\u{7f}", "\u{85}", "\u{061c}", "\u{202e}"] {
        assert!(safe(invalid, 64, false).is_err(), "{invalid:?}");
    }
    assert!(safe("", 0, false).is_ok());
    assert!(safe("", 1, true).is_err());
    assert!(safe("abcd", 3, false).is_err());
}

#[test]
fn labels_and_visible_ascii_enforce_independent_grammars() {
    assert!(label("Tool Name", 128).is_ok());
    assert!(label("tab\tname", 128).is_err());
    assert!(label("line\nname", 128).is_err());
    assert!(visible_ascii("!model~", 256).is_ok());
    assert!(visible_ascii("model name", 256).is_err());
}

#[test]
fn canonical_tracking_digest_and_stamp_boundaries_are_exact() {
    assert!(tracking_id("r").is_ok());
    assert!(tracking_id(&format!("r{}z", "-".repeat(126))).is_ok());
    assert!(tracking_id(&format!("r{}z", "-".repeat(127))).is_err());
    assert!(tracking_id("bad/id").is_err());

    assert!(digest(super::test_support::DIGEST).is_ok());
    assert!(digest(&super::test_support::DIGEST.to_uppercase()).is_err());
    assert!(stamp("img1-0123456789abcdef0123456789abcdef", "img1-").is_ok());
    assert!(stamp("img1-0123456789ABCDEF0123456789ABCDEF", "img1-").is_err());
}
