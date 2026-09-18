//! Pure validation of untrusted Provider DTO values.
//!
//! This module performs only local, deterministic validation. It has no
//! Store, resource, route, credential, transport, cache, or network state.
#![expect(
    dead_code,
    reason = "T7 pure validators are private foundations consumed by the T8 Provider runtime"
)]

mod adapter;
mod catalog;
mod charge;
mod context_counter;
pub(crate) mod decoder;
mod prepare;
mod scalar;
mod wire_json;

#[cfg(test)]
mod adapter_tests;
#[cfg(test)]
mod catalog_tests;
#[cfg(test)]
mod charge_tests;
#[cfg(test)]
mod context_counter_tests;
#[cfg(test)]
mod decoder_event_tests;
#[cfg(test)]
mod decoder_protocol_tests;
#[cfg(test)]
mod decoder_test_support;
#[cfg(test)]
mod decoder_tests;
#[cfg(test)]
mod decoder_tool_json_tests;
#[cfg(test)]
mod prepare_tests;
#[cfg(test)]
mod scalar_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod wire_json_tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValidationError {
    InvalidArgument,
    Limit,
}

pub(crate) type ValidationResult<T = ()> = Result<T, ValidationError>;
