//! The provider port: what the agent loop asks of a wire adapter.
//!
//! [`Provider`] is the single seam between [`crate`] and any model backend.
//! The request shape, the bounded [`EventStream`] a response arrives on, and
//! the [`ProviderError`] taxonomy are contracts; no wire protocol, transport,
//! credential handling, or model-selection policy lives here.

mod error;
mod stream;

#[doc(inline)]
pub use error::{ProviderError, ProviderErrorKind};
#[doc(inline)]
pub use stream::{EVENT_STREAM_CAPACITY, EventStream, EventStreamSender, MAX_EVENT_ENCODED_BYTES};

use crate::{AssistantMessage, Message, ToolSpec};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Maximum JSON-encoded size accepted for one provider request.
///
/// Eight MiB bounds the Agent-to-Host handoff while leaving room for normal
/// conversation history and tool schemas. The agent validates this limit after
/// hook transformation and before invoking a provider.
pub const MAX_REQUEST_ENCODED_BYTES: usize = 8 * 1_024 * 1_024;

/// Requested reasoning effort for models that support it.
///
/// The spellings match models.dev `reasoning_options` effort values plus
/// `on` / `off` for catalog toggle rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    /// Disable reasoning when the vendor accepts an off switch.
    Off,
    /// Shortest advertised effort.
    Minimal,
    /// Brief reasoning; fastest discrete rung.
    #[default]
    Low,
    /// Balanced reasoning.
    Medium,
    /// Deep reasoning.
    High,
    /// Above high, when the catalog advertises `xhigh`.
    Xhigh,
    /// Vendor maximum effort.
    Max,
    /// Enable reasoning on toggle-only models.
    On,
}

impl ReasoningLevel {
    /// Parses a catalog or settings spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "off" | "none" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            "on" => Some(Self::On),
            _ => None,
        }
    }

    /// Settings and OpenAI-style effort spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
            Self::On => "on",
        }
    }

    /// `reasoning_effort` wire token; `None` for toggle-on (no effort field).
    #[must_use]
    pub const fn effort_token(self) -> Option<&'static str> {
        match self {
            Self::Off => Some("none"),
            Self::On => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh => Some("xhigh"),
            Self::Max => Some("max"),
        }
    }
}

/// A provider-neutral completion request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Ordered system prompt parts.
    pub system_prompt: Vec<String>,
    /// Conversation history.
    pub messages: Vec<Message>,
    /// Tools available for the response.
    pub tools: Vec<ToolSpec>,
    /// Reasoning effort; `None` leaves the provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
}

impl Request {
    /// Creates an empty request.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one system prompt part.
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt.push(prompt.into());
        self
    }

    /// Appends one conversation message.
    #[must_use]
    pub fn with_message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Appends one tool specification.
    #[must_use]
    pub fn with_tool(mut self, tool: ToolSpec) -> Self {
        self.tools.push(tool);
        self
    }

    /// Sets the requested reasoning effort.
    #[must_use]
    pub fn with_reasoning(mut self, level: ReasoningLevel) -> Self {
        self.reasoning = Some(level);
        self
    }

    /// Validates the encoded request size.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when JSON encoding fails, or a rejected error
    /// when the encoded request exceeds [`MAX_REQUEST_ENCODED_BYTES`].
    pub fn validate(&self) -> Result<(), ProviderError> {
        let encoded = serde_json::to_vec(self).map_err(|_| {
            ProviderError::with_message(
                ProviderErrorKind::Protocol,
                "provider request could not be encoded",
            )
        })?;
        if encoded.len() > MAX_REQUEST_ENCODED_BYTES {
            return Err(ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "provider request exceeds the encoded size limit",
            ));
        }
        Ok(())
    }
}

/// One event emitted while an assistant message streams.
///
/// A stream ends with exactly one [`Self::Done`] or [`Self::Error`] event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StreamEvent {
    /// Incremental user-visible text.
    TextDelta(String),
    /// Incremental model reasoning text.
    ThinkingDelta(String),
    /// Incremental tool-call arguments.
    ToolCallDelta {
        /// Opaque tool-call identifier.
        id: String,
        /// A JSON fragment that need not be valid by itself.
        partial_json: String,
    },
    /// The complete assistant message.
    Done {
        /// Fully assembled message, including complete tool calls.
        message: AssistantMessage,
    },
    /// The terminal provider failure.
    Error(ProviderError),
}

impl StreamEvent {
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Error(_))
    }
}

/// Streams provider-neutral completions for the agent.
///
/// Production implementations belong in future Host adapters. Tests may inject
/// test-local implementations directly. Producers must honor both `cancel` and
/// [`EventStreamSender::closed`](EventStreamSender::closed), stop
/// producing promptly, and release all upstream resources.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Starts one completion stream.
    ///
    /// # Errors
    ///
    /// Returns a bounded [`ProviderError`] when setup fails before a stream is
    /// available. Streaming failures are terminal [`StreamEvent::Error`] items.
    async fn stream(
        &self,
        request: &Request,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError>;
}

#[cfg(test)]
mod tests {
    use crate::{ContentBlock, MycodeError, StopReason, ToolSpec, UserMessage};

    use super::*;

    fn tool() -> ToolSpec {
        ToolSpec {
            name: "read".into(),
            description: "read a file".into(),
            params_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn request_roundtrips_and_exposes_only_neutral_fields() {
        let request = Request::new()
            .with_system_prompt("be concise")
            .with_message(Message::User(UserMessage::text("hello")))
            .with_tool(tool());
        request.validate().expect("small request must validate");

        let encoded = serde_json::to_value(&request).expect("request must encode");
        assert_eq!(
            encoded.as_object().expect("request object").keys().count(),
            3
        );
        assert!(encoded.get("system_prompt").is_some());
        assert!(encoded.get("messages").is_some());
        assert!(encoded.get("tools").is_some());
        assert_eq!(
            serde_json::from_value::<Request>(encoded).expect("request must decode"),
            request
        );
    }

    #[test]
    fn request_rejects_legacy_fields() {
        for field in ["model", "thinking"] {
            let mut encoded = serde_json::to_value(Request::new()).expect("request must encode");
            encoded
                .as_object_mut()
                .expect("request object")
                .insert(field.into(), serde_json::json!("legacy"));
            assert!(
                serde_json::from_value::<Request>(encoded).is_err(),
                "legacy {field} field must be rejected"
            );
        }
    }

    #[test]
    fn request_rejects_removed_nested_message_wire_shapes() {
        let fixtures = [
            (
                "assistant text phase",
                serde_json::json!({
                    "Assistant": {
                        "blocks": [{
                            "Text": {"text": "checking", "phase": "commentary"}
                        }],
                        "usage": null,
                        "stop_reason": "Stop"
                    }
                }),
            ),
            (
                "thinking replay",
                serde_json::json!({
                    "Assistant": {
                        "blocks": [{
                            "Thinking": {
                                "text": "summary",
                                "replay": {
                                    "wire": "open_ai_responses",
                                    "provider": "openai",
                                    "endpoint": "https://api.openai.com",
                                    "data": "{}"
                                }
                            }
                        }],
                        "usage": null,
                        "stop_reason": "Stop"
                    }
                }),
            ),
            (
                "tool call item id",
                serde_json::json!({
                    "Assistant": {
                        "blocks": [{
                            "ToolCall": {
                                "id": "call-1",
                                "name": "read",
                                "arguments": {},
                                "item_id": "legacy-item"
                            }
                        }],
                        "usage": null,
                        "stop_reason": "ToolUse"
                    }
                }),
            ),
        ];

        for (name, message) in fixtures {
            let encoded = serde_json::json!({
                "system_prompt": [],
                "messages": [message],
                "tools": []
            });
            assert!(
                serde_json::from_value::<Request>(encoded).is_err(),
                "removed nested {name} shape must be rejected"
            );
        }
    }

    #[test]
    fn provider_boundaries_reject_nested_core_unknown_fields() {
        let request = serde_json::json!({
            "system_prompt": [],
            "messages": [{
                "User": {
                    "content": [{"Text": "hello"}],
                    "unknown": true
                }
            }],
            "tools": []
        });
        assert!(serde_json::from_value::<Request>(request).is_err());

        let event = serde_json::json!({
            "Done": {
                "message": {
                    "blocks": [{"Text": "done"}],
                    "usage": null,
                    "stop_reason": "Stop",
                    "unknown": true
                }
            }
        });
        assert!(serde_json::from_value::<StreamEvent>(event).is_err());
    }

    #[test]
    fn request_size_limit_is_enforced() {
        let request = Request::new().with_system_prompt("x".repeat(MAX_REQUEST_ENCODED_BYTES));
        let error = request.validate().expect_err("oversized request must fail");
        assert_eq!(error.kind(), ProviderErrorKind::Rejected);

        let request =
            Request::new().with_system_prompt("x".repeat(MAX_REQUEST_ENCODED_BYTES - 1_024));
        request
            .validate()
            .expect("request below the limit must pass");
    }

    #[test]
    fn stream_events_roundtrip() {
        let events = [
            StreamEvent::TextDelta("text".into()),
            StreamEvent::ThinkingDelta("thought".into()),
            StreamEvent::ToolCallDelta {
                id: "call-1".into(),
                partial_json: "{\"path\":".into(),
            },
            StreamEvent::Done {
                message: AssistantMessage {
                    blocks: vec![ContentBlock::Text("done".into())],
                    usage: None,
                    stop_reason: StopReason::Stop,
                },
            },
            StreamEvent::Error(ProviderError::new(ProviderErrorKind::Timeout)),
        ];
        for event in events {
            let encoded = serde_json::to_string(&event).expect("event must encode");
            let decoded = serde_json::from_str(&encoded).expect("event must decode");
            assert_eq!(event, decoded);
        }
    }

    #[test]
    fn stream_event_rejects_unknown_struct_variant_fields() {
        let encoded = serde_json::json!({
            "ToolCallDelta": {
                "id": "call-1",
                "partial_json": "{}",
                "item_id": "legacy-item"
            }
        });
        assert!(
            serde_json::from_value::<StreamEvent>(encoded).is_err(),
            "unknown stream event fields must be rejected"
        );
    }

    #[test]
    fn provider_error_conversion_stays_sanitized() {
        const SENTINEL: &str = "dummy-credential-sentinel";
        let error = ProviderError::with_message(
            ProviderErrorKind::Unavailable,
            format!("api_key={SENTINEL}"),
        );
        let converted = MycodeError::from(error);
        let rendered = format!("{converted:?} {converted}");
        assert!(!rendered.contains(SENTINEL));
        assert!(rendered.contains("REDACTED"));
    }
}
