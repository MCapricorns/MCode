//! OpenAI Chat Completions wire protocol adapter.
//!
//! Covers every OpenAI-compatible endpoint (OpenAI, DeepSeek, Kimi/Moonshot,
//! Z.AI gateways, custom `…/v1` bases). Vendor differences are data; this
//! adapter only owns the wire shape.

use serde_json::{Value, json};

use mycode_core::{ContentBlock, Message, StopReason, ToolSpec, Usage};
use mycode_core::{Request, StreamEvent};

use crate::driver::FrameReducer;
use crate::wire_common::{
    apply_reasoning_effort, assemble_blocks, join_text, merge_usage, usage_from_value,
};

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
        apply_reasoning_effort(&mut body, level);
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
            let text = join_text(&assistant.blocks);
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
            // A thinking-only assistant becomes `{"role":"assistant"}` with
            // no content; MiniMax and similar gateways reject that as
            // "unrecognized chat message".
            if text.is_empty() && tool_calls.is_empty() {
                return;
            }
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
            let content = join_text(&result.content);
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
        return json!(join_text(content));
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
    /// Extracts `<tool_call>` markup some endpoints stream as plain text.
    xml: crate::xml_tool_calls::XmlToolCallParser,
    /// Counter for synthetic ids minted by the XML filter.
    xml_calls: usize,
}

impl CompletionsReducer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn assemble(&mut self) -> StreamEvent {
        for piece in self.xml.finish() {
            if let crate::xml_tool_calls::XmlPiece::Text(text) = piece {
                self.text.push_str(&text);
            }
        }
        let blocks = assemble_blocks(
            &self.thinking,
            &self.text,
            self.tool_calls.iter().map(|call| {
                (
                    call.id.as_deref().unwrap_or_default(),
                    call.name.as_str(),
                    call.arguments.as_str(),
                )
            }),
        );
        // XML-filtered calls arrive without a `tool_calls` finish reason;
        // any dispatched call set must read as tool use (length stays).
        let stop_reason =
            if !self.tool_calls.is_empty() && self.stop_reason != Some(StopReason::Length) {
                StopReason::ToolUse
            } else {
                self.stop_reason.unwrap_or(StopReason::Stop)
            };
        StreamEvent::Done {
            message: mycode_core::AssistantMessage {
                blocks,
                usage: self.usage,
                stop_reason,
            },
        }
    }

    /// Runs one streamed content fragment through the XML tool-call filter.
    fn absorb_text(&mut self, text: &str, events: &mut Vec<StreamEvent>) {
        for piece in self.xml.feed(text) {
            match piece {
                crate::xml_tool_calls::XmlPiece::Text(text) => {
                    self.text.push_str(&text);
                    events.push(StreamEvent::TextDelta(text));
                }
                crate::xml_tool_calls::XmlPiece::ToolCall { name, arguments } => {
                    self.xml_calls += 1;
                    let id = format!("call-xml-{}", self.xml_calls);
                    events.push(StreamEvent::ToolCallDelta {
                        id: id.clone(),
                        partial_json: arguments.clone(),
                    });
                    self.tool_calls.push(ToolCallAccumulator {
                        id: Some(id),
                        name,
                        arguments,
                        pending: String::new(),
                    });
                }
            }
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
                self.absorb_text(text, &mut events);
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
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = Some(merge_usage(self.usage, usage_from_value(usage)));
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
    use mycode_core::{ReasoningLevel, TextBlock, ToolResultMessage, UserMessage};

    fn frame(delta: Value) -> String {
        json!({"choices": [{"delta": delta}]}).to_string()
    }

    #[test]
    fn body_converts_system_history_tools_and_images() {
        let request = Request::new()
            .with_system_prompt("one")
            .with_system_prompt("two")
            .with_message(Message::User(UserMessage::text("hello")))
            .with_message(Message::Assistant(mycode_core::AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking(mycode_core::ThinkingBlock::new("hmm")),
                    ContentBlock::Text(TextBlock::new("checking")),
                    ContentBlock::ToolCall(mycode_core::ToolCall::new(
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
    fn reasoning_tokens_follow_the_catalog_level() {
        let off = build_body(
            "test-model",
            &Request::new().with_reasoning(ReasoningLevel::Off),
        );
        assert_eq!(off["reasoning_effort"], "none");
        assert_eq!(off["thinking"]["type"], "disabled");

        let on = build_body(
            "test-model",
            &Request::new().with_reasoning(ReasoningLevel::On),
        );
        assert_eq!(on["thinking"]["type"], "enabled");
        assert!(on.get("reasoning_effort").is_none());

        let high = build_body(
            "test-model",
            &Request::new().with_reasoning(ReasoningLevel::High),
        );
        assert_eq!(high["reasoning_effort"], "high");
        let xhigh = build_body(
            "test-model",
            &Request::new().with_reasoning(ReasoningLevel::Xhigh),
        );
        assert_eq!(xhigh["reasoning_effort"], "xhigh");
    }

    #[test]
    fn thinking_only_assistant_is_omitted_from_completions_history() {
        let request =
            Request::new().with_message(Message::Assistant(mycode_core::AssistantMessage {
                blocks: vec![ContentBlock::Thinking(mycode_core::ThinkingBlock::new(
                    "hmm",
                ))],
                usage: None,
                stop_reason: StopReason::Stop,
            }));
        let body = build_body("test-model", &request);
        let messages = body["messages"].as_array().expect("messages");
        assert!(
            messages.is_empty(),
            "empty assistant messages are unrecognized chat messages: {messages:?}"
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
    fn usage_accepts_responses_names_and_ignores_a_later_zero() {
        let mut reducer = CompletionsReducer::new();
        let _ = reducer.feed(
            &json!({"choices": [{"delta": {}}], "usage": {"input_tokens": "11", "output_tokens": 2}})
                .to_string(),
        );
        let _ = reducer.feed(
            &json!({"choices": [], "usage": {"prompt_tokens": 0, "completion_tokens": 0}})
                .to_string(),
        );
        let events = reducer.feed("[DONE]");
        let StreamEvent::Done { message } = events.last().expect("terminal") else {
            panic!("done required");
        };
        assert_eq!(
            message.usage,
            Some(Usage {
                input_tokens: 11,
                output_tokens: 2,
                cache_read_tokens: None,
            })
        );
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

    #[test]
    fn xml_tool_call_text_becomes_a_real_tool_call() {
        let mut reducer = CompletionsReducer::new();
        let mut events = Vec::new();
        for data in [
            frame(json!({"content": "I'll check. <tool_call><function=read><para"})),
            frame(
                json!({"content": "meter=path>src/main.rs</parameter></function></tool_call> Done."}),
            ),
            "[DONE]".to_owned(),
        ] {
            events.extend(reducer.feed(&data));
        }
        assert_eq!(
            events[0],
            StreamEvent::TextDelta("I'll check. ".to_owned()),
            "text before the call streams normally"
        );
        let StreamEvent::Done { message } = events.pop().expect("terminal") else {
            panic!("done required");
        };
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert!(
            matches!(&message.blocks[0], ContentBlock::Text(t) if t.text == "I'll check.  Done.")
        );
        let ContentBlock::ToolCall(call) = &message.blocks[1] else {
            panic!("tool call required: {:?}", message.blocks);
        };
        assert_eq!(call.name, "read");
        assert_eq!(call.arguments, json!({"path": "src/main.rs"}));
        assert!(
            call.id.starts_with("call-xml-"),
            "synthetic id: {}",
            call.id
        );
    }
}
