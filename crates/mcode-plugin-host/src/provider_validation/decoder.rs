//! Pure decoder validation, protocol, event, and backpressure reducers.
mod events;
pub(crate) mod protocol;
pub(super) mod tool_json;

use std::collections::BTreeSet;

use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CompletionTerminal, DecoderPull, NormalizedEvent, ProviderError, ResponseFrame, Usage,
};

use super::charge::{LogicalCharge, checked_len};
use super::scalar::{self, KIB};
use super::{ValidationError, ValidationResult};

#[derive(Debug, Clone)]
pub(crate) struct DecoderPolicy {
    proof_supported: bool,
    tool_names: BTreeSet<String>,
    reported_models: BTreeSet<String>,
}

impl DecoderPolicy {
    pub(crate) fn new(
        proof_supported: bool,
        tool_names: impl IntoIterator<Item = String>,
        reported_models: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            proof_supported,
            tool_names: tool_names.into_iter().collect(),
            reported_models: reported_models.into_iter().collect(),
        }
    }

    pub(crate) fn empty() -> Self {
        Self::new(false, [], [])
    }
}

const MAX_PULL_LIMIT: u8 = 16;
const MAX_FRAME_DATA_BYTES: usize = 64 * 1_024;
const MAX_DELTA_BYTES: usize = 64 * 1_024;
const MAX_SINGLE_EVENT_CHARGE: u64 = 128 * KIB;
const MAX_BATCH_CHARGE: u64 = 1_024 * KIB;
const MAX_USAGE: u64 = i64::MAX as u64;

/// Reduces one non-2xx failed batch from seeded cumulative usage.
///
/// # Errors
///
/// Returns the same validation error as the production event reducer.
///
/// # Panics
///
/// Panics when either seed exceeds its production bound.
#[cfg(test)]
pub(super) fn reduce_failed_batch_with_cumulative_usage_for_test(
    event_count: u64,
    event_charge: u64,
) -> ValidationResult {
    let policy = DecoderPolicy::empty();
    let mut reducer = events::EventReducer::new(&policy);
    reducer.set_cumulative_usage_for_test(event_count, event_charge);
    reducer
        .reduce_batch(
            vec![NormalizedEvent::Failed(ProviderError::Unavailable)],
            1,
            true,
            false,
        )
        .map(|_| ())
}

pub(super) fn validate_pull_limit(limit: u8) -> ValidationResult {
    if !(1..=MAX_PULL_LIMIT).contains(&limit) {
        return Err(ValidationError::Limit);
    }
    Ok(())
}

pub(super) fn validate_response_frame(frame: &ResponseFrame) -> ValidationResult {
    match frame {
        ResponseFrame::Head(head) => {
            if !(200..=599).contains(&head.status) {
                return Err(ValidationError::InvalidArgument);
            }
            Ok(())
        }
        ResponseFrame::Data(data) => {
            if data.is_empty() {
                return Err(ValidationError::InvalidArgument);
            }
            if data.len() > MAX_FRAME_DATA_BYTES {
                return Err(ValidationError::Limit);
            }
            Ok(())
        }
        ResponseFrame::End => Ok(()),
    }
}

pub(super) fn validate_decoder_pull(pull: &DecoderPull, limit: u8) -> ValidationResult {
    let DecoderPull::Events(events) = pull else {
        return validate_pull_limit(limit);
    };
    validate_event_batch(events, limit)
}

pub(super) fn validate_event_batch(events: &[NormalizedEvent], limit: u8) -> ValidationResult {
    validate_pull_limit(limit)?;
    if events.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    if events.len() > usize::from(limit) {
        return Err(ValidationError::InvalidArgument);
    }
    let mut batch = LogicalCharge::new(MAX_BATCH_CHARGE);
    batch.add(4)?;
    for event in events {
        let charge = validate_normalized_event(event)?;
        batch.add(charge)?;
    }
    Ok(())
}

pub(super) fn validate_normalized_event(event: &NormalizedEvent) -> ValidationResult<u64> {
    let mut charge = LogicalCharge::new(MAX_SINGLE_EVENT_CHARGE);
    charge.add(4)?;
    match event {
        NormalizedEvent::TextDelta(delta) => {
            validate_content_index(delta.content_index)?;
            charge.add(1)?;
            validate_delta(&delta.text, &mut charge)?;
        }
        NormalizedEvent::ReasoningDelta(delta) => {
            validate_content_index(delta.content_index)?;
            charge.add(1)?;
            charge.add(4)?;
            validate_delta(&delta.text, &mut charge)?;
        }
        NormalizedEvent::ReasoningProof(proof) => {
            validate_content_index(proof.content_index)?;
            charge.add(1)?;
            charge.add(4)?;
            if proof.proof.is_empty() {
                return Err(ValidationError::InvalidArgument);
            }
            if proof.proof.len() > MAX_DELTA_BYTES {
                return Err(ValidationError::Limit);
            }
            charge.add(4)?;
            charge.add(checked_len(proof.proof.len())?)?;
        }
        NormalizedEvent::ToolCallStart(start) => {
            validate_content_index(start.content_index)?;
            scalar::tracking_id(&start.call_id)?;
            scalar::label(&start.name, 128)?;
            charge.add(1)?;
            charge.string(&start.call_id)?;
            charge.string(&start.name)?;
        }
        NormalizedEvent::ToolArgumentsDelta(delta) => {
            validate_content_index(delta.content_index)?;
            scalar::tracking_id(&delta.call_id)?;
            charge.add(1)?;
            charge.string(&delta.call_id)?;
            validate_delta(&delta.delta, &mut charge)?;
        }
        NormalizedEvent::ToolCallEnd(end) => {
            validate_content_index(end.content_index)?;
            scalar::tracking_id(&end.call_id)?;
            charge.add(1)?;
            charge.string(&end.call_id)?;
        }
        NormalizedEvent::Completed(terminal) => validate_terminal(terminal, &mut charge)?,
        NormalizedEvent::Failed(error) => validate_stable_error(error, &mut charge)?,
    }
    Ok(charge.value())
}

fn validate_delta(value: &str, charge: &mut LogicalCharge) -> ValidationResult {
    scalar::safe(value, MAX_DELTA_BYTES, true)?;
    charge.string(value)
}

fn validate_content_index(index: u8) -> ValidationResult {
    if index > 63 {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

fn validate_terminal(
    terminal: &CompletionTerminal,
    charge: &mut LogicalCharge,
) -> ValidationResult {
    charge.add(4)?;
    charge.add(4)?;
    if let Some(model) = &terminal.reported_model {
        scalar::model_id(model)?;
        charge.string(model)?;
    }
    validate_usage(&terminal.usage, charge)
}

fn validate_usage(usage: &Usage, charge: &mut LogicalCharge) -> ValidationResult {
    validate_usage_value(usage.input_tokens, charge)?;
    validate_usage_value(usage.output_tokens, charge)?;
    validate_usage_value(usage.cache_read_tokens, charge)?;
    validate_usage_value(usage.cache_write_tokens, charge)
}

fn validate_usage_value(value: Option<u64>, charge: &mut LogicalCharge) -> ValidationResult {
    charge.add(4)?;
    if let Some(value) = value {
        if value > MAX_USAGE {
            return Err(ValidationError::Limit);
        }
        charge.add(8)?;
    }
    Ok(())
}

fn validate_stable_error(error: &ProviderError, charge: &mut LogicalCharge) -> ValidationResult {
    charge.add(4)?;
    match error {
        ProviderError::InvalidArgument
        | ProviderError::Limit
        | ProviderError::Unavailable
        | ProviderError::Cancelled
        | ProviderError::Failed => {}
        ProviderError::UnsupportedFlow(_) => charge.add(4)?,
    }
    Ok(())
}
