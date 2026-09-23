//! Agent loop fan-out events.
//!
//! `AgentEvent`, `MessageDelta`, and `TurnOutcome` form the live Agent loop
//! fan-out protocol. `mycode-agent` distributes `AgentEvent` through
//! `tokio::broadcast`, so the event type must stay `Clone`.

use serde::{Deserialize, Serialize};

use crate::error::MycodeError;
use crate::ids::CallId;
use crate::message::{Message, ToolResultMessage};

/// Events emitted by the Agent loop.
///
/// Subscribers receive them via `tokio::broadcast`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum AgentEvent {
    /// A new turn started processing.
    TurnStarted,
    /// Incremental delta while an assistant message streams in.
    MessageDelta(MessageDelta),
    /// A complete message was appended to the Agent history.
    MessageAdded(Message),
    /// A tool call started executing.
    ToolStarted {
        call_id: CallId,
        name: String,
        /// Path, query, or command the call is acting on. Empty when unknown.
        target: String,
    },
    /// Progress update from a running tool.
    ToolProgress { call_id: CallId, message: String },
    /// A tool call finished.
    ToolCompleted {
        call_id: CallId,
        result: ToolResultMessage,
    },
    /// The current turn ended.
    TurnEnded(TurnOutcome),
    /// An error occurred within the Agent loop.
    Error(MycodeError),
}

/// Incremental assistant content while streaming (mirrors the provider
/// stream events of design doc `01-agent-core.md` §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MessageDelta {
    /// Partial text content.
    TextDelta(String),
    /// Partial thinking content.
    ThinkingDelta(String),
    /// Partial tool-call arguments (raw JSON fragment).
    ToolCallDelta { id: String, partial_json: String },
}

/// Why a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOutcome {
    /// The model stopped without further tool calls.
    Completed,
    /// The turn was aborted via cancellation.
    Aborted,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, UserMessage};

    fn assert_roundtrip<T>(value: &T)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, value);
    }

    fn sample_events() -> Vec<AgentEvent> {
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::MessageDelta(MessageDelta::TextDelta("partial".into())),
            AgentEvent::MessageDelta(MessageDelta::ThinkingDelta("hmm".into())),
            AgentEvent::MessageDelta(MessageDelta::ToolCallDelta {
                id: "call_1".into(),
                partial_json: "{\"path\":".into(),
            }),
            AgentEvent::MessageAdded(Message::User(UserMessage::text("hello"))),
            AgentEvent::ToolStarted {
                call_id: CallId::from("call_1"),
                name: "read".into(),
                target: "src/main.rs".into(),
            },
            AgentEvent::ToolProgress {
                call_id: CallId::from("call_1"),
                message: "1024 bytes".into(),
            },
            AgentEvent::ToolCompleted {
                call_id: CallId::from("call_1"),
                result: ToolResultMessage {
                    tool_call_id: "call_1".into(),
                    content: vec![ContentBlock::Text("done".into())],
                    is_error: false,
                    details: None,
                },
            },
            AgentEvent::TurnEnded(TurnOutcome::Completed),
            AgentEvent::TurnEnded(TurnOutcome::Aborted),
            AgentEvent::Error(MycodeError::Tool("boom".into())),
        ]
    }

    #[test]
    fn agent_event_roundtrip_all_variants() {
        for event in sample_events() {
            assert_roundtrip(&event);
        }
    }

    #[test]
    fn message_delta_and_turn_outcome_roundtrip() {
        assert_roundtrip(&MessageDelta::TextDelta("x".into()));
        assert_roundtrip(&TurnOutcome::Aborted);
    }

    #[test]
    fn struct_variants_reject_unknown_fields() {
        for mut encoded in [
            serde_json::json!({"ToolStarted": {"call_id": "call_1", "name": "read"}}),
            serde_json::json!({"ToolProgress": {"call_id": "call_1", "message": "half"}}),
            serde_json::json!({
                "ToolCompleted": {
                    "call_id": "call_1",
                    "result": {
                        "tool_call_id": "call_1",
                        "content": [{"Text": "done"}],
                        "is_error": false,
                        "details": null
                    }
                }
            }),
        ] {
            encoded
                .as_object_mut()
                .and_then(|event| event.values_mut().next())
                .and_then(serde_json::Value::as_object_mut)
                .expect("struct variant object")
                .insert("unknown".into(), serde_json::Value::Bool(true));
            assert!(
                serde_json::from_value::<AgentEvent>(encoded).is_err(),
                "AgentEvent struct variant must reject unknown fields"
            );
        }

        let delta = serde_json::json!({
            "ToolCallDelta": {
                "id": "call_1",
                "partial_json": "{}",
                "unknown": true
            }
        });
        assert!(
            serde_json::from_value::<MessageDelta>(delta).is_err(),
            "MessageDelta struct variant must reject unknown fields"
        );
    }

    #[test]
    fn events_are_clone_for_broadcast() {
        // tokio::broadcast requires Clone; guard against accidental removal.
        fn require_clone<T: Clone>(_: &T) {}
        for event in sample_events() {
            require_clone(&event);
        }
    }
}
