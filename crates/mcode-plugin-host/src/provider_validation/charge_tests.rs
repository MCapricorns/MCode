//! Checked logical-charge tests.
use super::ValidationError;
use super::charge::LogicalCharge;

#[test]
fn charge_accepts_exact_limit_and_rejects_first_byte_over() {
    let mut charge = LogicalCharge::new(8);
    assert!(charge.add(8).is_ok());
    assert_eq!(charge.add(1), Err(ValidationError::Limit));
}

#[test]
fn charge_rejects_checked_u64_overflow() {
    let mut charge = LogicalCharge::new(u64::MAX);
    assert!(charge.add(u64::MAX).is_ok());
    assert_eq!(charge.add(1), Err(ValidationError::Limit));
}

#[test]
fn string_charge_includes_u32_length_prefix() {
    let mut charge = LogicalCharge::new(7);
    assert!(charge.string("abc").is_ok());
    assert_eq!(charge.value(), 7);

    let mut too_small = LogicalCharge::new(6);
    assert_eq!(too_small.string("abc"), Err(ValidationError::Limit));
}
