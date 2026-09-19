//! OpenAI Chat Completions wire protocol adapter.
//!
//! Covers every OpenAI-compatible endpoint (OpenAI, DeepSeek, Kimi/Moonshot,
//! Z.AI gateways, custom `…/v1` bases). Vendor differences are data; this
//! adapter only owns the wire shape.

use serde_json::{Value, json};

use mcode_core::{ContentBlock, Message, StopReason, ToolSpec, Usage};
use mcode_provider_api::{ReasoningLevel, Request, StreamEvent};

use crate::driver::FrameReducer;

/// Concatenation separator for multi-part system prompts.
const SYSTEM_JOIN: &str = "\n\n";

/// Converts one provider-neutral request into a completions body.
#[must_use]
pub(crate) fn build_body(model: &str, request: &Request) -> Value {
    let mut messages = Vec::new();
    if !request.system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": request.system_prompt.join(SYSTEM_JOIN),
        }));
    }
    for message in &request.messages {
        convert_message(message, &mut messages);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(level) = request.reasoning {
        body["reasoning_effort"] = json!(match level {
            ReasoningLevel::Low => "low",
            ReasoningLevel::Medium => "medium",
            ReasoningLevel::High => "high",
        });
    }
    body
}

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.params_schema,
        },
    })
}

fn convert_message(message: &Message, messages: &mut Vec<Value>) {
    match message {
        Message::User(user) => {
            messages.push(json!({"role": "user", "content": user_content(&user.content)}));
        }
        Message::Assistant(assistant) => {
            let text: String = assistant
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            let tool_calls: Vec<Value> = assistant
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolCall(call) => Some(json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        },
                    })),
                    _ => None,
                })
                .collect();
            // Thinking blocks have no completions replay channel; providers
            // that need them expose their own reasoning fields.
            let mut wire = json!({"role": "assistant"});
            if !text.is_empty() {
                wire["content"] = json!(text);
            }
            if !tool_calls.is_empty() {
                wire["tool_calls"] = json!(tool_calls);
            }
            messages.push(wire);
        }
        Message::ToolResult(result) => {
            let content: String = result
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            messages.push(json!({
                "role": "tool",
                "tool_call_id": result.tool_call_id,
                "content": content,
            }));
        }
        Message::Custom(_) => {}
    }
}

fn user_content(content: &[ContentBlock]) -> Value {
    let has_image = content
        .iter()
        .any(|block| matches!(block, ContentBlock::Image(_)));
    if !has_image {
        let text: String = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        return json!(text);
    }
    let parts: Vec<Value> = content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(text) => json!({"type": "text", "text": text.text}),
            ContentBlock::Image(image) => json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{};base64,{}", image.mime_type, image.data)},
            }),
            _ => json!({"type": "text", "text": ""}),
        })
        .collect();
    json!(parts)
}

/// One streaming tool call being stitched from argument fragments.
#[derive(Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: String,
    arguments: String,
    /// Argument bytes held while the call id is still unknown.
    pending: String,
}

/// Accumulates Chat Completions stream chunks.
#[derive(Default)]
pub(crate) struct CompletionsReducer {
    thinking: String,
    text: String,
    tool_calls: Vec<ToolCallAccumulator>,
    usage: Option<Usage>,
    stop_reason: Option<StopReason>,
    terminal_sent: bool,
}

impl CompletionsReducer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn assemble(&self) -> StreamEvent {
        let mut blocks = Vec::new();
        if !self.thinking.is_empty() {
            blocks.push(ContentBlock::Thinking(mcode_core::ThinkingBlock::new(
                self.thinking.clone(),
            )));
        }
        if !self.text.is_empty() {
            blocks.push(ContentBlock::Text(mcode_core::TextBlock::new(
                self.text.clone(),
            )));
        }
        for call in &self.tool_calls {
            let arguments =
                serde_json::from_str::<Value>(&call.arguments).unwrap_or_else(|_| json!({}));
            blocks.push(ContentBlock::ToolCall(mcode_core::ToolCall::new(
                call.id.clone().unwrap_or_default(),
                call.name.clone(),
                arguments,
            )));
        }
        StreamEvent::Done {
            message: mcode_core::AssistantMessage {
                blocks,
                usage: self.usage,
                stop_reason: self.stop_reason.unwrap_or(StopReason::Stop),
            },
        }
    }
}

impl FrameReducer for CompletionsReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        if data.trim() == "[DONE]" {
            self.terminal_sent = true;
            return vec![self.assemble()];
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return vec![crate::driver::protocol_error("invalid completions frame")];
        };
        let mut events = Vec::new();
        if let Some(choice) = chunk["choices"].get(0) {
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str()
                && !text.is_empty()
            {
                self.text.push_str(text);
                events.push(StreamEvent::TextDelta(text.to_owned()));
            }
            let reasoning = delta["reasoning_content"]
                .as_str()
                .or_else(|| delta["reasoning"].as_str());
            if let Some(text) = reasoning
                && !text.is_empty()
            {
                self.thinking.push_str(text);
                events.push(StreamEvent::ThinkingDelta(text.to_owned()));
            }
            if let Some(fragments) = delta["tool_calls"].as_array() {
                for fragment in fragments {
                    self.absorb_tool_fragment(fragment, &mut events);
                }
            }
            if let Some(finish) = choice["finish_reason"].as_str() {
                self.stop_reason = Some(match finish {
                    "tool_calls" | "function_call" => StopReason::ToolUse,
                    _ => StopReason::Stop,
                });
            }
        }
        if let Some(usage) = chunk["usage"].as_object() {
            self.usage = Some(Usage {
                input_tokens: usage["prompt_tokens"].as_u64().unwrap_or_default(),
                cache_read_tokens: usage
                    .get("prompt_tokens_details")
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(serde_json::Value::as_u64),
                output_tokens: usage["completion_tokens"].as_u64().unwrap_or_default(),
            });
        }
        events
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("completions stream ended after terminal");
        }
        self.terminal_sent = true;
        self.assemble()
    }
}

impl CompletionsReducer {
    fn absorb_tool_fragment(&mut self, fragment: &Value, events: &mut Vec<StreamEvent>) {
        let index = fragment["index"].as_u64().unwrap_or_default() as usize;
        while self.tool_calls.len() <= index {
            self.tool_calls.push(ToolCallAccumulator::default());
        }
        let call = &mut self.tool_calls[index];
        if let Some(id) = fragment["id"].as_str()
            && call.id.is_none()
        {
            call.id = Some(id.to_owned());
            if !call.pending.is_empty() {
                let pending = std::mem::take(&mut call.pending);
                call.arguments.push_str(&pending);
                events.push(StreamEvent::ToolCallDelta {
                    id: id.to_owned(),
                    partial_json: pending,
                });
            }
        }
        if let Some(name) = fragment["function"]["name"].as_str() {
            call.name.push_str(name);
        }
        if let Some(arguments) = fragment["function"]["arguments"].as_str()
            && !arguments.is_empty()
        {
            match &call.id {
                Some(id) => {
                    call.arguments.push_str(arguments);
                    events.push(StreamEvent::ToolCallDelta {
                        id: id.clone(),
                        partial_json: arguments.to_owned(),
                    });
                }
                None => call.pending.push_str(arguments),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcode_core::{TextBlock, ToolResultMessage, UserMessage};

    fn frame(delta: Value) -> String {
        json!({"choices": [{"delta": delta}]}).to_string()
    }

    #[test]
    fn body_converts_system_history_tools_and_images() {
        let request = Request::new()
            .with_system_prompt("one")
            .with_system_prompt("two")
            .with_message(Message::User(UserMessage::text("hello")))
            .with_message(Message::Assistant(mcode_core::AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking(mcode_core::ThinkingBlock::new("hmm")),
                    ContentBlock::Text(TextBlock::new("checking")),
                    ContentBlock::ToolCall(mcode_core::ToolCall::new(
                        "call-1",
                        "read",
                        json!({"path": "a.txt"}),
                    )),
                ],
                usage: None,
                stop_reason: StopReason::ToolUse,
            }))
            .with_message(Message::ToolResult(ToolResultMessage {
                tool_call_id: "call-1".into(),
                content: vec![ContentBlock::Text(TextBlock::new("data"))],
                is_error: false,
                details: None,
            }))
            .with_tool(ToolSpec {
                name: "read".into(),
                description: "read".into(),
                params_schema: json!({"type": "object"}),
            });
        let body = build_body("test-model", &request);
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "one\n\ntwo");
        assert_eq!(messages[1]["role"], "user");
        let assistant = &messages[2];
        assert_eq!(assistant["content"], "checking");
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "read");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(
            body["tools"][0]["function"]["name"], "read",
            "thinking block must not enter completions history"
        );
    }

    #[test]
    fn reducer_streams_text_reasoning_and_stitched_tool_calls() {
        let mut reducer = CompletionsReducer::new();
        let mut events = Vec::new();
        for data in [
            frame(json!({"content": "he"})),
            frame(json!({"reasoning_content": "think"})),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call-9", "function": {"name": "read", "arguments": "{\"pa"}},
            ]}}]})
            .to_string(),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "th\":1}"}},
            ]}}]})
            .to_string(),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                   "usage": {"prompt_tokens": 3, "completion_tokens": 5}})
            .to_string(),
            "[DONE]".to_owned(),
        ] {
            events.extend(reducer.feed(&data));
        }

        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "he"));
        assert!(matches!(&events[1], StreamEvent::ThinkingDelta(t) if t == "think"));
        assert_eq!(events.len(), 5, "deltas then one terminal: {events:?}");
        let StreamEvent::Done { message } = events.pop().expect("terminal") else {
            panic!("done required");
        };
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert_eq!(
            message.usage,
            Some(Usage {
                input_tokens: 3,
                output_tokens: 5,
                cache_read_tokens: None
            })
        );
        let call = match &message.blocks[2] {
            ContentBlock::ToolCall(call) => call,
            other => panic!("tool call required: {other:?}"),
        };
        assert_eq!(call.id, "call-9");
        assert_eq!(call.arguments, json!({"path": 1}));
    }

    #[test]
    fn eof_without_done_still_assembles() {
        let mut reducer = CompletionsReducer::new();
        reducer.feed(&frame(json!({"content": "partial"})));
        let StreamEvent::Done { message } = reducer.finish() else {
            panic!("done required");
        };
        assert!(matches!(&message.blocks[0], ContentBlock::Text(t) if t.text == "partial"));
    }

    #[test]
    fn unknown_id_tool_fragments_buffer_until_id_arrives() {
        let mut reducer = CompletionsReducer::new();
        let events = reducer.feed(
            &json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"name": "read", "arguments": "{\"a\":"}},
            ]}}]})
            .to_string(),
        );
        assert!(events.is_empty(), "no id yet: no delta event");
        let events = reducer.feed(
            &json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "call-2", "function": {"arguments": "1,\"b\":2}"}},
            ]}}]})
            .to_string(),
        );
        assert!(matches!(
            &events[0],
            StreamEvent::ToolCallDelta { id, partial_json } if id == "call-2"
                && !partial_json.is_empty()
        ));
        let StreamEvent::Done { message } = reducer.finish() else {
            panic!("done required");
        };
        let ContentBlock::ToolCall(call) = &message.blocks[0] else {
            panic!("tool call required");
        };
        assert_eq!(call.arguments, json!({"a": 1, "b": 2}));
    }
}
