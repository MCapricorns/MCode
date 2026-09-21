//! OpenAI Responses wire protocol adapter.
//!
//! Targets the current Responses streaming shape: `response.output_text.delta`
//! for text, `response.function_call_arguments.delta` for tool arguments, and
//! `response.completed` for usage. Reasoning summaries stream as thinking
//! deltas when the endpoint provides them.

use serde_json::{Value, json};

use mycode_core::{AssistantMessage, ContentBlock, Message, StopReason, ToolSpec, Usage};
use mycode_core::{ReasoningLevel, Request, StreamEvent};

use crate::driver::FrameReducer;

/// Converts one provider-neutral request into a Responses body.
#[must_use]
pub(crate) fn build_body(model: &str, request: &Request) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        convert_message(message, &mut input);
    }
    let tools: Vec<Value> = request.tools.iter().map(convert_tool).collect();
    let mut body = json!({
        "model": model,
        "input": input,
        "stream": true,
    });
    if !request.system_prompt.is_empty() {
        body["instructions"] = json!(request.system_prompt.join("\n\n"));
    }
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
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.params_schema,
    })
}

fn convert_message(message: &Message, input: &mut Vec<Value>) {
    match message {
        Message::User(user) => {
            let text: String = user
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}],
            }));
        }
        Message::Assistant(assistant) => {
            for block in &assistant.blocks {
                match block {
                    ContentBlock::Text(text) => {
                        input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text.text}],
                        }));
                    }
                    // The Responses API owns its reasoning items; replaying
                    // signed thinking from other protocols is not possible.
                    ContentBlock::Thinking(_) => {}
                    ContentBlock::ToolCall(call) => {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        }));
                    }
                    ContentBlock::Image(_) => {}
                }
            }
        }
        Message::ToolResult(result) => {
            let output: String = result
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            input.push(json!({
                "type": "function_call_output",
                "call_id": result.tool_call_id,
                "output": output,
            }));
        }
        Message::Custom(_) => {}
    }
}

#[derive(Default)]
struct FunctionCallAccumulator {
    id: String,
    name: String,
    arguments: String,
    text_emitted: bool,
}

/// Accumulates Responses SSE events.
#[derive(Default)]
pub(crate) struct ResponsesReducer {
    thinking: String,
    text: String,
    function_calls: Vec<FunctionCallAccumulator>,
    usage: Option<Usage>,
    terminal_sent: bool,
}

impl ResponsesReducer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn assemble(&mut self) -> StreamEvent {
        self.terminal_sent = true;
        let mut blocks = Vec::new();
        if !self.thinking.is_empty() {
            blocks.push(ContentBlock::Thinking(mycode_core::ThinkingBlock::new(
                self.thinking.clone(),
            )));
        }
        if !self.text.is_empty() {
            blocks.push(ContentBlock::Text(mycode_core::TextBlock::new(
                self.text.clone(),
            )));
        }
        let tool_use = !self.function_calls.is_empty();
        for call in &self.function_calls {
            let arguments =
                serde_json::from_str::<Value>(&call.arguments).unwrap_or_else(|_| json!({}));
            blocks.push(ContentBlock::ToolCall(mycode_core::ToolCall::new(
                call.id.clone(),
                call.name.clone(),
                arguments,
            )));
        }
        StreamEvent::Done {
            message: AssistantMessage {
                blocks,
                usage: self.usage,
                stop_reason: if tool_use {
                    StopReason::ToolUse
                } else {
                    StopReason::Stop
                },
            },
        }
    }
}

impl FrameReducer for ResponsesReducer {
    fn feed(&mut self, data: &str) -> Vec<StreamEvent> {
        if self.terminal_sent {
            return Vec::new();
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            return vec![crate::driver::protocol_error("invalid responses frame")];
        };
        match event["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => {
                let part = event["delta"].as_str().unwrap_or_default();
                if !part.is_empty() {
                    self.text.push_str(part);
                    return vec![StreamEvent::TextDelta(part.to_owned())];
                }
            }
            "response.reasoning_summary_text.delta" => {
                let part = event["delta"].as_str().unwrap_or_default();
                if !part.is_empty() {
                    self.thinking.push_str(part);
                    return vec![StreamEvent::ThinkingDelta(part.to_owned())];
                }
            }
            "response.output_item.added" => {
                let item = &event["item"];
                if item["type"].as_str() == Some("function_call") {
                    self.function_calls.push(FunctionCallAccumulator {
                        id: item["call_id"].as_str().unwrap_or_default().to_owned(),
                        name: item["name"].as_str().unwrap_or_default().to_owned(),
                        arguments: String::new(),
                        text_emitted: false,
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let part = event["delta"].as_str().unwrap_or_default();
                if let Some(call) = self.function_calls.last_mut() {
                    call.arguments.push_str(part);
                    if !part.is_empty() && !call.id.is_empty() {
                        call.text_emitted = true;
                        return vec![StreamEvent::ToolCallDelta {
                            id: call.id.clone(),
                            partial_json: part.to_owned(),
                        }];
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                let usage = &event["response"]["usage"];
                self.usage = Some(Usage {
                    input_tokens: usage["input_tokens"].as_u64().unwrap_or_default(),
                    cache_read_tokens: usage["input_tokens_details"]["cached_tokens"].as_u64(),
                    output_tokens: usage["output_tokens"].as_u64().unwrap_or_default(),
                });
                return vec![self.assemble()];
            }
            "response.failed" | "error" => {
                self.terminal_sent = true;
                return vec![StreamEvent::Error(
                    mycode_core::ProviderError::with_message(
                        mycode_core::ProviderErrorKind::Rejected,
                        event["error"]["message"]
                            .as_str()
                            .or_else(|| event["response"]["error"]["message"].as_str())
                            .unwrap_or("responses stream failed"),
                    ),
                )];
            }
            _ => {}
        }
        Vec::new()
    }

    fn finish(&mut self) -> StreamEvent {
        if self.terminal_sent {
            return crate::driver::protocol_error("responses stream ended after terminal");
        }
        self.assemble()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mycode_core::{ToolResultMessage, UserMessage};

    #[test]
    fn body_converts_instructions_history_and_tools() {
        let request = Request::new()
            .with_system_prompt("sys")
            .with_message(Message::User(UserMessage::text("go")))
            .with_message(Message::Assistant(AssistantMessage {
                blocks: vec![ContentBlock::ToolCall(mycode_core::ToolCall::new(
                    "call-1",
                    "read",
                    json!({"a": 1}),
                ))],
                usage: None,
                stop_reason: StopReason::ToolUse,
            }))
            .with_message(Message::ToolResult(ToolResultMessage {
                tool_call_id: "call-1".into(),
                content: vec![ContentBlock::Text(mycode_core::TextBlock::new("ok"))],
                is_error: false,
                details: None,
            }))
            .with_tool(ToolSpec {
                name: "read".into(),
                description: "read".into(),
                params_schema: json!({"type": "object"}),
            });
        let body = build_body("gpt-x", &request);
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][1]["type"], "function_call");
        assert_eq!(body["input"][2]["type"], "function_call_output");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[test]
    fn reducer_streams_text_tool_args_and_usage() {
        let mut reducer = ResponsesReducer::new();
        let mut events = Vec::new();
        for data in [
            json!({"type": "response.output_item.added", "item": {
                "type": "function_call", "call_id": "call-4", "name": "read"}})
            .to_string(),
            json!({"type": "response.output_text.delta", "delta": "partial"}).to_string(),
            json!({"type": "response.function_call_arguments.delta", "delta": "{\"x\":"})
                .to_string(),
            json!({"type": "response.function_call_arguments.delta", "delta": "1}"}).to_string(),
            json!({"type": "response.completed", "response": {"usage": {
                "input_tokens": 9, "output_tokens": 2}}})
            .to_string(),
        ] {
            events.extend(reducer.feed(&data));
        }
        assert!(matches!(&events[0], StreamEvent::TextDelta(t) if t == "partial"));
        assert!(matches!(&events[1], StreamEvent::ToolCallDelta { id, .. } if id == "call-4"));
        let StreamEvent::Done { message } = events.pop().expect("terminal") else {
            panic!("done required");
        };
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert_eq!(
            message.usage,
            Some(Usage {
                input_tokens: 9,
                output_tokens: 2,
                cache_read_tokens: None
            })
        );
        let call = match &message.blocks[1] {
            ContentBlock::ToolCall(call) => call,
            other => panic!("tool call required: {other:?}"),
        };
        assert_eq!(call.arguments, json!({"x": 1}));
    }

    #[test]
    fn failed_response_maps_to_rejected_error() {
        let mut reducer = ResponsesReducer::new();
        let events = reducer.feed(
            &json!({"type": "response.failed", "error": {"message": "bad model"}}).to_string(),
        );
        assert!(
            matches!(&events[0], StreamEvent::Error(e) if e.kind() == mycode_core::ProviderErrorKind::Rejected)
        );
    }
}
