//! Defines the provider-neutral Agent-to-Host Rust port.
//!
//! This crate contains only the request, error, provider, and bounded stream
//! contracts used between `mcode-agent` and a future Host adapter. It is not a
//! ProviderPack world or product extension surface, and it contains no provider
//! implementation, selection policy, profile, wire protocol, or transport.

mod error;
mod provider;
mod stream;

#[doc(inline)]
pub use error::{ProviderError, ProviderErrorKind};
#[doc(inline)]
pub use provider::{MAX_REQUEST_ENCODED_BYTES, Provider, ReasoningLevel, Request, StreamEvent};
#[doc(inline)]
pub use stream::{EVENT_STREAM_CAPACITY, EventStream, EventStreamSender, MAX_EVENT_ENCODED_BYTES};
