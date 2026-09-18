//! Checked WIT logical-charge accounting.
use super::{ValidationError, ValidationResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LogicalCharge {
    value: u64,
    limit: u64,
}

impl LogicalCharge {
    pub(super) const fn new(limit: u64) -> Self {
        Self { value: 0, limit }
    }

    pub(super) fn add(&mut self, value: u64) -> ValidationResult {
        self.value = self
            .value
            .checked_add(value)
            .ok_or(ValidationError::Limit)?;
        if self.value > self.limit {
            return Err(ValidationError::Limit);
        }
        Ok(())
    }

    pub(super) fn string(&mut self, value: &str) -> ValidationResult {
        self.add(4)?;
        self.add(checked_len(value.len())?)
    }

    pub(super) const fn value(self) -> u64 {
        self.value
    }
}

pub(super) fn checked_len(value: usize) -> ValidationResult<u64> {
    u64::try_from(value).map_err(|_| ValidationError::Limit)
}

pub(super) fn checked_u32_len(value: usize) -> ValidationResult<u32> {
    u32::try_from(value).map_err(|_| ValidationError::Limit)
}
