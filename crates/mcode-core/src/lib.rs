//! `mcode-core` — core types for MCode: messages, events, ids, errors.
//!
//! Leaf crate with no business dependencies; everything here is plain data
//! with serde support so values can flow through Agent event broadcasts and
//! model payloads. See `docs/design/01-agent-core.md`.

pub mod error;
pub mod events;
pub mod ids;
pub mod message;
pub mod tool;

pub use error::McodeError;
pub use events::{AgentEvent, MessageDelta, TurnOutcome};
pub use ids::CallId;
pub use message::{
    AssistantMessage, BinaryData, ContentBlock, CustomMessage, Message, StopReason, TextBlock,
    ThinkingBlock, ToolCall, ToolResultMessage, Usage, UserMessage,
};
pub use tool::ToolSpec;
