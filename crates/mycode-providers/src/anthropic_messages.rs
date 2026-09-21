//! Anthropic Messages wire protocol adapter.
//!
//! Covers Anthropic and Anthropic-compatible gateways (Z.AI GLM coding plans,
//! custom relays). Thinking signatures round-trip verbatim, including
//! signature-only blocks whose reasoning text is empty; the adapter never
//! enables thinking explicitly, so default-thinking models keep their own
//! configuration.

use serde_json::{Value, json};

use mycode_core::{
    AssistantMessage, ContentBlock, Message, StopReason, ThinkingBlock, ToolSpec, Usage,
};
use mycode_core::{ProviderError, ProviderErrorKind, ReasoningLevel, Request, StreamEvent};

use crate::driver::FrameReducer;

/// Output ceiling sent with every request; the Messages API requires it.
pub const MAX_TOKENS_DEFAULT: u64 = 4096;

/// Converts one provider-neutral request into a Messages body.
#[must_use]
pub(crate) fn build_body(model: &str, request: &Request) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        convert_message(message, &mut messages);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS_DEFAULT,
        "messages": messages,
        "stream": true,
    });
    if !request.system_prompt.is_empty() {
        body["system"] = json!(request.system_prompt.join("\n\n"));
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    if let Some(level) = request.reasoning {
        match level {
            ReasoningLevel::Off => {
                body["thinking"] = json!({ "type": "disabled" });
            }
            level => {
                // Thinking budget must stay below max_tokens; raise the cap
                // so the budget always fits. Rungs follow models.dev effort
                // tokens rather than a hardcoded three-step list.
                let budget = match level {
                    ReasoningLevel::Minimal | ReasoningLevel::Low => 1_024,
                    ReasoningLevel::On | ReasoningLevel::Medium => 4_096,
                    ReasoningLevel::High => 16_384,
                    ReasoningLevel::Xhigh | ReasoningLevel::Max => 32_768,
                    ReasoningLevel::Off => 0,
                };
                let max_tokens = body["max_tokens"].as_u64().unwrap_or(MAX_TOKENS_DEFAULT);
                if max_tokens <= budget {
                    body["max_tokens"] = json!(budget + MAX_TOKENS_DEFAULT);
                }
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            }
        }
    }
    body
}

fn convert_tool(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.params_schema,
    })
}

fn convert_message(message: &Message, messages: &mut Vec<Value>) {
    match message {
        Message::User(user) => {
            messages.push(json!({"role": "user", "content": block_content(&user.content)}));
        }
        Message::Assistant(assistant) => {
            let content: Vec<Value> = assistant
                .blocks
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(json!({
                        "type": "text",
                        "text": text.text,
                    })),
                    // Signatures replay verbatim; empty thinking text is kept
                    // whenever a signature exists.
                    ContentBlock::Thinking(thinking) => {
                        let signature = thinking.signature.as_deref()?;
                        Some(json!({
                            "type": "thinking",
                            "thinking": thinking.text,
                            "signature": signature,
                        }))
                    }
                    ContentBlock::ToolCall(call) => Some(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    })),
                    ContentBlock::Image(_) => None,
                })
                .collect();
            if !content.is_empty() {
                messages.push(json!({"role": "assistant", "content": content}));
            }
        }
        Message::ToolResult(result) => {
            let content = crate::wire_common::join_text(&result.content);
            messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": result.tool_call_id,
                    "content": content,
                    "is_error": result.is_error,
                }],
            }));
        }
        Message::Custom(_) => {}
    }
}

fn block_content(content: &[ContentBlock]) -> Value {
    let parts: Vec<Value> = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(json!({"type": "text", "text": text.text})),
            ContentBlock::Image(image) => Some(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime_type,
                    "data": image.data,
                },
            })),
            _ => None,
        })
        .collect();
    json!(parts)
}

/// One content block being assembled from deltas.
#[derive(Default)]
enum BlockAccumulator {
    #[default]
    Empty,
    Thinking {
        text: String,
        signature: Option<String>,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        arguments: String,
    },
}

/// Accumulates Messages SSE events.
#[derive(Default)]
pub(crate) struct MessagesReducer {
    blocks: Vec<BlockAccumulator>,
    current: usize,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: Option<u64>,
    stop_reason: Option<StopReason>,
    message_stopped: bool,
    terminal_sent: bool,
    /// Extracts `<tool_call>` markup some endpoints stream as plain text.
    xml: crate::xml_tool_calls::XmlToolCallParser,
    /// Counter for synthetic ids minted by the XML filter.
    xml_calls: usize,
}

impl MessagesReducer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn current_id(&self) -> Option<String> {
        match self.blocks.get(self.current)? {
            BlockAccumulator::ToolUse { id, .. } => Some(id.clone()),
            _ => None,
        }
    }

    fn assemble(&mut self) -> StreamEvent {
        self.terminal_sent = true;
        // Trailing text still held by the XML filter joins the message.
        for piece in self.xml.finish() {
            if let crate::xml_tool_calls::XmlPiece::Text(text) = piece {
                match self.blocks.get_mut(self.current) {
                    Some(BlockAccumulator::Text { text: block }) => block.push_str(&text),
                    _ if !text.is_empty() => {
                        self.blocks.push(BlockAccumulator::Text { text });
                    }
                    _ => {}
                }
            }
        }
        let mut blocks = Vec::new();
        for block in &self.blocks {
            match block {
                BlockAccumulator::Thinking { text, signature } => {
                    let mut thinking = ThinkingBlock::new(text.clone());
                    thinking.signature = signature.clone();
                    blocks.push(ContentBlock::Thinking(thinking));
                }
                BlockAccumulator::Text { text } => {
                    blocks.push(ContentBlock::Text(mycode_core::TextBlock::new(
                        text.clone(),
                    )));
                }
                BlockAccumulator::ToolUse {
                    id,
                    name,
                    arguments,
                } => {
                    let arguments =
                        serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
                    blocks.push(ContentBlock::ToolCall(mycode_core::ToolCall::new(
                        id.clone(),
                        name.clone(),
                        arguments,
                    )));
                }
                BlockAccumulator::Empty => {}
            }
        }
        // XML-filtered calls arrive with an `end_turn` stop reason; any
        // dispatched call set must read as tool use (length stays).
        let has_calls = self
            .blocks
            .iter()
            .any(|block| matches!(block, BlockAccumulator::ToolUse { .. }));
        let stop_reason = if has_calls && self.stop_reason != Some(StopReason::Length) {
            StopReason::ToolUse
        } else {
            self.stop_reason.unwrap_or(StopReason::Stop)
        };
        StreamEvent::Done {
            message: AssistantMessage {
                blocks,
                usage: Some(Usage {
                    input_tokens: self.input_tokens,
                    output_tokens: self.output_tokens,
                    cache_read_tokens: self.cache_read_tokens,
                }),
                stop_reason,
            },
        }
    }
}

impl FrameReducer for MessagesReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            return vec![crate::driver::protocol_error("invalid messages frame")];
        };
        let event_type = event["type"].as_str().unwrap_or_default();
        match event_type {
            "message_start" => {
                let usage = &event["message"]["usage"];
                self.input_tokens = usage["input_tokens"].as_u64().unwrap_or_default();
                self.cache_read_tokens = usage["cache_read_input_tokens"].as_u64();
            }
            "content_block_start" => {
                let index = event["index"].as_u64().unwrap_or_default() as usize;
                let block = &event["content_block"];
                let accumulator = match block["type"].as_str().unwrap_or_default() {
                    "thinking" | "redacted_thinking" => BlockAccumulator::Thinking {
                        text: block["thinking"].as_str().unwrap_or_default().to_owned(),
                        signature: block["signature"].as_str().map(str::to_owned),
                    },
                    "tool_use" => BlockAccumulator::ToolUse {
                        id: block["id"].as_str().unwrap_or_default().to_owned(),
                        name: block["name"].as_str().unwrap_or_default().to_owned(),
                        arguments: String::new(),
                    },
                    _ => BlockAccumulator::Text {
                        text: block["text"].as_str().unwrap_or_default().to_owned(),
                    },
                };
                while self.blocks.len() <= index {
                    self.blocks.push(BlockAccumulator::Empty);
                }
                self.blocks[index] = accumulator;
                self.current = index;
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let part = delta["text"].as_str().unwrap_or_default();
                        if part.is_empty() {
                            return Vec::new();
                        }
                        let mut events = Vec::new();
                        for piece in self.xml.feed(part) {
                            match piece {
                                crate::xml_tool_calls::XmlPiece::Text(text) => {
                                    if let BlockAccumulator::Text { text: block } = self
                                        .blocks
                                        .get_mut(self.current)
                                        .unwrap_or(&mut BlockAccumulator::Empty)
                                    {
                                        block.push_str(&text);
                                    }
                                    events.push(StreamEvent::TextDelta(text));
                                }
                                crate::xml_tool_calls::XmlPiece::ToolCall { name, arguments } => {
                                    self.xml_calls += 1;
                                    let id = format!("toolu-xml-{}", self.xml_calls);
                                    events.push(StreamEvent::ToolCallDelta {
                                        id: id.clone(),
                                        partial_json: arguments.clone(),
                                    });
                                    self.blocks.push(BlockAccumulator::ToolUse {
                                        id,
                                        name,
                                        arguments,
                                    });
                                }
                            }
                        }
                        return events;
                    }
                    "thinking_delta" => {
                        if let BlockAccumulator::Thinking { text, .. } = self
                            .blocks
                            .get_mut(self.current)
                            .unwrap_or(&mut BlockAccumulator::Empty)
                        {
                            let part = delta["thinking"].as_str().unwrap_or_default();
                            text.push_str(part);
                            if !part.is_empty() {
                                return vec![StreamEvent::ThinkingDelta(part.to_owned())];
                            }
                        }
                    }
                    "signature_delta" => {
                        if let BlockAccumulator::Thinking { signature, .. } = self
                            .blocks
                            .get_mut(self.current)
                            .unwrap_or(&mut BlockAccumulator::Empty)
                            && let Some(value) = delta["signature"].as_str()
                        {
                            *signature = Some(value.to_owned());
                        }
                    }
                    "input_json_delta" => {
                        let part = delta["partial_json"].as_str().unwrap_or_default();
                        if let Some(id) = self.current_id() {
                            if let BlockAccumulator::ToolUse { arguments, .. } =
                                &mut self.blocks[self.current]
                            {
                                arguments.push_str(part);
                            }
                            if !part.is_empty() {
                                return vec![StreamEvent::ToolCallDelta {
                                    id,
                                    partial_json: part.to_owned(),
                                }];
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {}
            "message_delta" => {
                if let Some(stop) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(match stop {
                        "tool_use" => StopReason::ToolUse,
                        _ => StopReason::Stop,
                    });
                }
                if let Some(usage) = event["usage"].as_object() {
                    self.output_tokens = usage["output_tokens"]
                        .as_u64()
                        .unwrap_or(self.output_tokens);
                }
            }
            "message_stop" => {
                self.message_stopped = true;
                return vec![self.assemble()];
            }
            "error" => {
                self.terminal_sent = true;
                return vec![StreamEvent::Error(ProviderError::with_message(
                    ProviderErrorKind::Rejected,
                    event["error"]["message"]
                        .as_str()
                        .unwrap_or("provider error frame"),
                ))];
            }
            "ping" => {}
            _ => {}
        }
        Vec::new()
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("messages stream ended after terminal");
        }
        if self.message_stopped || self.stop_reason.is_some() {
            return self.assemble();
        }
        crate::driver::protocol_error("messages stream ended before message_stop")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mycode_core::{TextBlock, ToolResultMessage, UserMessage};

    #[test]
    fn body_converts_system_tools_and_history_with_signatures() {
        let mut signed = ThinkingBlock::new("");
        signed.signature = Some("sig-1".into());
        let request = Request::new()
            .with_system_prompt("be brief")
            .with_message(Message::User(UserMessage::text("hi")))
            .with_message(Message::Assistant(AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking(signed),
                    ContentBlock::Thinking(ThinkingBlock::new("unsigned")),
                    ContentBlock::ToolCall(mycode_core::ToolCall::new(
                        "toolu_1",
                        "read",
                        json!({"path": "x"}),
                    )),
                ],
                usage: None,
                stop_reason: StopReason::ToolUse,
            }))
            .with_message(Message::ToolResult(ToolResultMessage {
                tool_call_id: "toolu_1".into(),
                content: vec![ContentBlock::Text(TextBlock::new("done"))],
                is_error: false,
                details: None,
            }))
            .with_tool(ToolSpec {
                name: "read".into(),
                description: "read".into(),
                params_schema: json!({"type": "object"}),
            });
        let body = build_body("claude-x", &request);
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["max_tokens"], MAX_TOKENS_DEFAULT);
        assert_eq!(body["tools"][0]["name"], "read");
        let assistant = &body["messages"][1]["content"];
        assert_eq!(
            assistant[0],
            json!({"type": "thinking", "thinking": "", "signature": "sig-1"}),
            "signature-only thinking replays verbatim"
        );
        assert_eq!(
            assistant.as_array().map(Vec::len),
            Some(2),
            "unsigned thinking must not replay"
        );
        assert_eq!(assistant[1]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn reducer_streams_thinking_signature_tools_and_usage() {
        let mut reducer = MessagesReducer::new();
        let mut events = Vec::new();
        for data in [
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 7, "cache_read_input_tokens": 5}}}).to_string(),
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "thinking", "thinking": ""}})
            .to_string(),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "thinking_delta", "thinking": "why"}})
            .to_string(),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "signature_delta", "signature": "sig-9"}})
            .to_string(),
            json!({"type": "content_block_start", "index": 1,
                   "content_block": {"type": "text", "text": ""}})
            .to_string(),
            json!({"type": "content_block_delta", "index": 1,
                   "delta": {"type": "text_delta", "text": "hi"}})
            .to_string(),
            json!({"type": "content_block_start", "index": 2,
                   "content_block": {"type": "tool_use", "id": "toolu_2", "name": "read"}})
            .to_string(),
            json!({"type": "content_block_delta", "index": 2,
                   "delta": {"type": "input_json_delta", "partial_json": "{\"p\":"}})
            .to_string(),
            json!({"type": "content_block_delta", "index": 2,
                   "delta": {"type": "input_json_delta", "partial_json": "1}"}})
            .to_string(),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                   "usage": {"output_tokens": 4}})
            .to_string(),
            json!({"type": "message_stop"}).to_string(),
        ] {
            events.extend(reducer.feed(&data));
        }

        assert!(matches!(&events[0], StreamEvent::ThinkingDelta(t) if t == "why"));
        assert!(matches!(&events[1], StreamEvent::TextDelta(t) if t == "hi"));
        assert!(
            matches!(&events[2], StreamEvent::ToolCallDelta { id, partial_json } if id == "toolu_2" && partial_json == "{\"p\":")
        );
        let StreamEvent::Done { message } = events.pop().expect("terminal") else {
            panic!("done required");
        };
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert_eq!(
            message.usage,
            Some(Usage {
                input_tokens: 7,
                output_tokens: 4,
                cache_read_tokens: Some(5)
            })
        );
        let ContentBlock::Thinking(thinking) = &message.blocks[0] else {
            panic!("thinking first");
        };
        assert_eq!(thinking.text, "why");
        assert_eq!(thinking.signature.as_deref(), Some("sig-9"));
        let ContentBlock::ToolCall(call) = &message.blocks[2] else {
            panic!("tool call third");
        };
        assert_eq!(call.arguments, json!({"p": 1}));
    }

    #[test]
    fn eof_after_stop_reason_assembles_and_without_it_fails() {
        let mut reducer = MessagesReducer::new();
        reducer.feed(
            &json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}).to_string(),
        );
        assert!(matches!(reducer.finish(), StreamEvent::Done { .. }));

        let mut early = MessagesReducer::new();
        assert!(matches!(early.finish(), StreamEvent::Error(_)));
    }

    #[test]
    fn xml_tool_call_text_becomes_a_real_tool_call() {
        let mut reducer = MessagesReducer::new();
        let mut events = Vec::new();
        for data in [
            json!({"type": "content_block_start", "index": 0,
                   "content_block": {"type": "text"}}).to_string(),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": "checking. <tool_call>{\"name\": \"web_search\","}}).to_string(),
            json!({"type": "content_block_delta", "index": 0,
                   "delta": {"type": "text_delta", "text": " \"arguments\": {\"query\": \"rust\"}}</tool_call> found it"}}).to_string(),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}).to_string(),
            json!({"type": "message_stop"}).to_string(),
        ] {
            events.extend(reducer.feed(&data));
        }
        assert!(
            matches!(&events[0], StreamEvent::TextDelta(t) if t == "checking. "),
            "text before the call streams normally"
        );
        let StreamEvent::Done { message } = events.pop().expect("terminal") else {
            panic!("done required");
        };
        // end_turn is overridden because the assembled message carries calls.
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert!(
            matches!(&message.blocks[0], ContentBlock::Text(t) if t.text == "checking.  found it")
        );
        let ContentBlock::ToolCall(call) = &message.blocks[1] else {
            panic!("tool call second: {:?}", message.blocks);
        };
        assert_eq!(call.name, "web_search");
        assert_eq!(call.arguments, json!({"query": "rust"}));
        assert!(call.id.starts_with("toolu-xml-"));
    }
}
