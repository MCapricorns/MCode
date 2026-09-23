//! Message model — the core vocabulary exchanged between user, model, tools,
//! and plugins (design doc `01-agent-core.md` §1).
//!
//! Serde uses the default externally-tagged representation here. Provider wire
//! handling belongs to a future signed Provider Pack behind the Host
//! `ProviderPackService`; durable Session encoding belongs to the future signed
//! Session Pack behind `SessionPackService`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A message in the conversation tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Message {
    /// A message authored by the user (prompts).
    User(UserMessage),
    /// A message produced by the model.
    Assistant(AssistantMessage),
    /// The result of executing a tool call.
    ToolResult(ToolResultMessage),
    /// Plugin-defined message. The `data` payload passes through
    /// serialization untouched so plugins can persist arbitrary state —
    /// the Rust replacement for pi's declaration merging.
    Custom(CustomMessage),
}

/// A user-authored message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserMessage {
    pub content: Vec<ContentBlock>,
}

impl UserMessage {
    /// Builds a plain-text user message.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextBlock::new(text))],
        }
    }
}

/// A model-produced message: ordered content blocks plus turn metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    pub blocks: Vec<ContentBlock>,
    /// Token usage as reported by the provider, when available.
    pub usage: Option<Usage>,
    pub stop_reason: StopReason,
}

impl AssistantMessage {
    /// Concatenated text of all text content blocks.
    #[must_use]
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A single unit of message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ContentBlock {
    /// Plain text.
    Text(TextBlock),
    /// Model reasoning ("thinking") content.
    Thinking(ThinkingBlock),
    /// A request to invoke a tool.
    ToolCall(ToolCall),
    /// Binary payload (currently only used for images).
    Image(BinaryData),
}

/// Plain text content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBlock {
    /// The text itself.
    pub text: String,
}

impl TextBlock {
    /// Creates plain text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

impl From<String> for TextBlock {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for TextBlock {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl Serialize for TextBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.text.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TextBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// Human-visible model reasoning or summary text.
///
/// The optional `signature` carries provider reasoning-integrity fields (e.g.
/// Anthropic thinking signatures). Adapters must preserve it verbatim across
/// round-trips, including signature-only blocks whose `text` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThinkingBlock {
    /// The reasoning text.
    pub text: String,
    /// Provider reasoning-integrity signature, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl ThinkingBlock {
    /// Creates reasoning text without a signature.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            signature: None,
        }
    }

    /// Attaches a provider signature.
    #[must_use]
    pub fn with_signature(mut self, signature: impl Into<String>) -> Self {
        self.signature = Some(signature.into());
        self
    }
}

impl From<String> for ThinkingBlock {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for ThinkingBlock {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Provider-assigned call id (opaque string; matched by
    /// [`ToolResultMessage::tool_call_id`]).
    ///
    /// The value is not a packed encoding. Adapters must not split or parse it
    /// to recover other identifiers.
    pub id: String,
    /// Name of the tool to invoke.
    pub name: String,
    /// Arguments as raw JSON; validated against the tool's schema at dispatch
    /// time (`mycode-tools`).
    pub arguments: serde_json::Value,
}

impl ToolCall {
    /// Short target shown next to the tool name: a path, query, or command.
    #[must_use]
    pub fn target(&self) -> String {
        tool_target(&self.name, &self.arguments)
    }

    /// Creates a tool call with an opaque provider-assigned id.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// One-line target for a tool call, taken from the arguments the model sent.
///
/// The UI shows this beside the tool name so a `read` is a path, not a black box.
#[must_use]
pub fn tool_target(name: &str, arguments: &serde_json::Value) -> String {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let joined = match name {
        "read" | "write" | "edit" => text("path"),
        "grep" | "find" => join_target(&text("pattern"), &text("path")),
        "shell" => text("command"),
        "exec" => {
            let args = arguments
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            join_target(&text("program"), &args)
        }
        "web_search" => text("query"),
        "fetch_content" => arguments
            .get("urls")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned(),
        "search_tool" | "use_tool" => text("name"),
        "task" => join_target(&text("agent"), &text("description")),
        _ => text("path"),
    };
    let flat = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(160).collect()
}

/// Display label: `read  src/main.rs`. The name stays the first token.
#[must_use]
pub fn tool_label(name: &str, target: &str) -> String {
    let target = target.trim();
    if target.is_empty() {
        name.to_owned()
    } else {
        format!("{name}  {target}")
    }
}

fn join_target(left: &str, right: &str) -> String {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => String::new(),
        (false, true) => left.to_owned(),
        (true, false) => right.to_owned(),
        (false, false) => format!("{left}  {right}"),
    }
}

/// The outcome of executing a tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResultMessage {
    /// Id of the [`ToolCall`] this answers.
    pub tool_call_id: String,
    /// Content visible to the model.
    pub content: Vec<ContentBlock>,
    pub is_error: bool,
    /// Structured details for the UI layer only — never enters LLM context
    /// (structured diffs, cwd, …). Splitting `details` from `content` keeps
    /// tokens out of the model loop (pi's ToolResult pattern).
    pub details: Option<serde_json::Value>,
}

/// A plugin-defined message; serialized transparently.
///
/// The `data` field passes through verbatim to preserve plugin state such as
/// plan trackers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomMessage {
    /// Plugin-scoped kind discriminator, e.g. `"plugin:plan"`.
    pub kind: String,
    /// Arbitrary plugin payload, preserved verbatim.
    pub data: serde_json::Value,
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StopReason {
    /// Natural end of turn.
    Stop,
    /// The model wants to call tools.
    ToolUse,
    /// Output was cut off by a length/token limit.
    Length,
    /// Generation ended because of an error.
    Error,
}

/// Token usage reported by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Prompt tokens served from the provider cache, when reported.
    #[serde(default)]
    pub cache_read_tokens: Option<u64>,
}

/// Binary content (base64) with its MIME type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryData {
    /// Base64-encoded bytes, as expected by provider image APIs.
    pub data: String,
    /// MIME type, e.g. `"image/png"`.
    pub mime_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    fn assert_roundtrip<T>(value: &T)
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, value);
    }

    fn assert_rejects_unknown_field<T>(value: &T)
    where
        T: Serialize + DeserializeOwned,
    {
        let mut encoded = serde_json::to_value(value).expect("serialize");
        encoded
            .as_object_mut()
            .expect("DTO must serialize as an object")
            .insert("unknown".into(), Value::Bool(true));
        assert!(
            serde_json::from_value::<T>(encoded).is_err(),
            "unknown outer field must be rejected"
        );
    }

    fn sample_tool_call() -> ToolCall {
        ToolCall::new(
            "call_abc123",
            "read",
            json!({"path": "Cargo.toml", "offset": 1}),
        )
    }

    #[test]
    fn assistant_text_concatenates_text_blocks_only() {
        let message = AssistantMessage {
            blocks: vec![
                ContentBlock::Thinking(ThinkingBlock::new("reasoning")),
                ContentBlock::Text(TextBlock::new("one")),
                ContentBlock::ToolCall(sample_tool_call()),
                ContentBlock::Text(TextBlock::new("two")),
            ],
            usage: None,
            stop_reason: StopReason::Stop,
        };
        assert_eq!(message.text(), "onetwo");
        assert_eq!(
            AssistantMessage {
                blocks: Vec::new(),
                usage: None,
                stop_reason: StopReason::Stop,
            }
            .text(),
            ""
        );
    }

    #[test]
    fn normal_messages_roundtrip() {
        assert_roundtrip(&Message::User(UserMessage::text("hello")));
        assert_roundtrip(&Message::User(UserMessage {
            content: vec![
                ContentBlock::Text("describe this:".into()),
                ContentBlock::Image(BinaryData {
                    data: "aGVsbG8=".into(),
                    mime_type: "image/png".into(),
                }),
            ],
        }));

        for stop_reason in [
            StopReason::Stop,
            StopReason::ToolUse,
            StopReason::Length,
            StopReason::Error,
        ] {
            assert_roundtrip(&Message::Assistant(AssistantMessage {
                blocks: vec![
                    ContentBlock::Thinking("let me think".into()),
                    ContentBlock::Text("reading the file".into()),
                    ContentBlock::ToolCall(sample_tool_call()),
                    ContentBlock::Image(BinaryData {
                        data: "AAEC".into(),
                        mime_type: "image/jpeg".into(),
                    }),
                ],
                usage: Some(Usage {
                    input_tokens: 1_200,
                    output_tokens: 42,
                    cache_read_tokens: None,
                }),
                stop_reason,
            }));
        }
    }

    #[test]
    fn text_is_plain_string_and_thinking_is_closed_object_json() {
        let text = TextBlock::new("hello");
        let thinking = ThinkingBlock::new("considering");
        assert_eq!(serde_json::to_string(&text).unwrap(), r#""hello""#);
        assert_eq!(
            serde_json::to_value(&thinking).unwrap(),
            json!({"text": "considering"})
        );
        assert_roundtrip(&text);
        assert_roundtrip(&thinking);

        let signed = ThinkingBlock::new("").with_signature("sig-1");
        assert_eq!(
            serde_json::to_value(&signed).unwrap(),
            json!({"text": "", "signature": "sig-1"})
        );
        assert_roundtrip(&signed);
        assert_eq!(
            serde_json::from_value::<ThinkingBlock>(json!({"text": "", "signature": "sig-1"}))
                .unwrap(),
            signed,
            "signature-only thinking must survive replay"
        );

        assert!(
            serde_json::from_value::<TextBlock>(json!({
                "text": "checking the file",
                "phase": "commentary"
            }))
            .is_err(),
            "old phased text objects must be rejected"
        );
        assert!(
            serde_json::from_value::<ThinkingBlock>(json!({
                "text": "summary",
                "replay": {
                    "wire": "open_ai_responses",
                    "provider": "openai",
                    "endpoint": "https://api.openai.com",
                    "data": "{}"
                }
            }))
            .is_err(),
            "old rich reasoning objects must be rejected"
        );
    }

    #[test]
    fn tool_call_shape_is_closed_and_arguments_stay_arbitrary() {
        let arguments = json!({"nested": {"list": [1, 2.5, null, true, "x"], "obj": {}}});
        let call = ToolCall::new("v1:1:1:ab", "read", arguments.clone());
        let value = serde_json::to_value(&call).unwrap();
        let object = value.as_object().expect("tool call object");
        assert_eq!(object.len(), 3);
        assert_eq!(value["id"], "v1:1:1:ab");
        assert_eq!(value["name"], "read");
        assert_eq!(value["arguments"], arguments);
        assert_roundtrip(&call);

        for field in ["item_id", "unknown"] {
            let mut old = value.clone();
            old.as_object_mut()
                .expect("tool call object")
                .insert(field.into(), json!("legacy"));
            assert!(
                serde_json::from_value::<ToolCall>(old).is_err(),
                "tool call field {field} must be rejected"
            );
        }
    }

    #[test]
    fn tool_result_roundtrip_preserves_arbitrary_details() {
        let details = json!({"cwd": "/tmp", "diff": {"added": 3, "removed": 1}});
        let result = ToolResultMessage {
            tool_call_id: "call_abc123".into(),
            content: vec![ContentBlock::Text("file contents".into())],
            is_error: true,
            details: Some(details.clone()),
        };
        assert_roundtrip(&Message::ToolResult(result.clone()));
        assert_eq!(result.details, Some(details));
    }

    #[test]
    fn custom_message_preserves_arbitrary_json() {
        let data = json!({
            "plan": [
                {"step": 1, "title": "探索实现方案", "done": true},
                {"step": 2, "title": "write code", "done": false, "notes": null}
            ],
            "meta": {"progress": 0.5, "tags": [], "owner": Value::Null}
        });
        let custom = CustomMessage {
            kind: "plugin:plan".into(),
            data: data.clone(),
        };
        assert_roundtrip(&Message::Custom(custom.clone()));
        assert_eq!(custom.data, data);
    }

    #[test]
    fn core_struct_dtos_reject_unknown_outer_fields() {
        assert_rejects_unknown_field(&UserMessage::text("hello"));
        assert_rejects_unknown_field(&AssistantMessage {
            blocks: vec![ContentBlock::Text("done".into())],
            usage: None,
            stop_reason: StopReason::Stop,
        });
        assert_rejects_unknown_field(&sample_tool_call());
        assert_rejects_unknown_field(&ToolResultMessage {
            tool_call_id: "call_1".into(),
            content: vec![ContentBlock::Text("done".into())],
            is_error: false,
            details: Some(json!({"nested": {"unknown": true}})),
        });
        assert_rejects_unknown_field(&CustomMessage {
            kind: "plugin:test".into(),
            data: json!({"unknown": true}),
        });
        assert_rejects_unknown_field(&Usage {
            input_tokens: 7,
            output_tokens: 9,
            cache_read_tokens: None,
        });
        assert_rejects_unknown_field(&BinaryData {
            data: "Zm9v".into(),
            mime_type: "application/octet-stream".into(),
        });
    }

    #[test]
    fn tool_target_keeps_the_path_beside_the_name() {
        let call = ToolCall::new("c1", "read", json!({"path": "src/main.rs"}));
        assert_eq!(call.target(), "src/main.rs");
        assert_eq!(tool_label("read", &call.target()), "read  src/main.rs");
        assert_eq!(
            tool_target("grep", &json!({"pattern": "foo", "path": "src"})),
            "foo src"
        );
    }
}
