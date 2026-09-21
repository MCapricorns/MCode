//! Helpers shared by the wire-protocol adapters.
//!
//! One place for the fragments every OpenAI-family adapter repeats: the
//! reasoning-effort body fields, text-block concatenation, and terminal
//! block assembly. Adapter-specific shapes (Anthropic's budgeted thinking,
//! per-index block accumulators) stay with their adapters.

use serde_json::{Value, json};

use mycode_core::{ContentBlock, ReasoningLevel, TextBlock, ThinkingBlock, ToolCall};

/// Applies the requested reasoning effort to an OpenAI-style body.
pub(crate) fn apply_reasoning_effort(body: &mut Value, level: ReasoningLevel) {
    match level {
        ReasoningLevel::Off => {
            body["reasoning_effort"] = json!("none");
            body["thinking"] = json!({ "type": "disabled" });
        }
        ReasoningLevel::On => {
            body["thinking"] = json!({ "type": "enabled" });
        }
        other => {
            if let Some(token) = other.effort_token() {
                body["reasoning_effort"] = json!(token);
            }
        }
    }
}

/// Concatenates the text of every text block, in order.
pub(crate) fn join_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Assembles terminal content blocks: optional thinking, optional text, then
/// one tool-call block per stitched call. Arguments parse as JSON and default
/// to an empty object when a vendor streams an invalid fragment.
pub(crate) fn assemble_blocks<'a>(
    thinking: &str,
    text: &str,
    calls: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>,
) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if !thinking.is_empty() {
        blocks.push(ContentBlock::Thinking(ThinkingBlock::new(thinking)));
    }
    if !text.is_empty() {
        blocks.push(ContentBlock::Text(TextBlock::new(text)));
    }
    for (id, name, arguments) in calls {
        let arguments = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
        blocks.push(ContentBlock::ToolCall(ToolCall::new(id, name, arguments)));
    }
    blocks
}
