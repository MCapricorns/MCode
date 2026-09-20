//! Streaming filter for XML `<tool_call>` blocks emitted as plain text.
//!
//! Some OpenAI/Anthropic-compatible endpoints (MiniMax M2, StepFun step-3.5,
//! self-hosted Qwen) never map tool calling onto native `tool_use` deltas and
//! instead stream the model's raw tool-call markup inside the text channel.
//! This parser intercepts that markup mid-stream so the reducers can surface
//! real tool calls instead of leaking XML into the visible reply.
//!
//! Two payload shapes are accepted inside `<tool_call>…</tool_call>`:
//!
//! * JSON — `{"name": "read", "arguments": {"path": "a.rs"}}`
//!   (also `parameters`/`params` for the `arguments` key)
//! * XML — `<function=read><parameter=path>src/a.rs</parameter></function>`
//!   with one `<parameter=key>value</parameter>` pair per argument

/// Cap on one buffered `<tool_call>` body; oversized bodies fall back to text.
const MAX_CALL_BODY_CHARS: usize = 32 * 1024;

const OPEN_TAG: &str = "<tool_call>";
const CLOSE_TAG: &str = "</tool_call>";

/// One parsed piece of a streamed text channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum XmlPiece {
    /// Visible text that is not part of a tool call.
    Text(String),
    /// One complete tool call: tool name and its JSON-encoded arguments.
    ToolCall { name: String, arguments: String },
}

/// Incremental `<tool_call>` extractor over a text stream.
#[derive(Default)]
pub(crate) struct XmlToolCallParser {
    /// Text not yet safe to classify: a possible partial tag or an open call.
    buffer: String,
    inside: bool,
}

impl XmlToolCallParser {
    /// Feeds one text delta and returns the pieces that are now final.
    pub(crate) fn feed(&mut self, text: &str) -> Vec<XmlPiece> {
        self.buffer.push_str(text);
        let mut pieces = Vec::new();
        loop {
            if self.inside {
                let Some(end) = self.buffer.find(CLOSE_TAG) else {
                    if self.buffer.chars().count() > MAX_CALL_BODY_CHARS {
                        // A runaway body without a closing tag degrades to
                        // visible text instead of unbounded buffering.
                        let raw = std::mem::take(&mut self.buffer);
                        self.inside = false;
                        pieces.push(XmlPiece::Text(format!("{OPEN_TAG}{raw}")));
                    }
                    break;
                };
                let body = self.buffer[..end].to_owned();
                self.buffer.replace_range(..end + CLOSE_TAG.len(), "");
                self.inside = false;
                match parse_call_body(&body) {
                    Some((name, arguments)) => pieces.push(XmlPiece::ToolCall { name, arguments }),
                    // Unparseable bodies surface as text; swallowing model
                    // output silently would hide real content.
                    None => pieces.push(XmlPiece::Text(format!("{OPEN_TAG}{body}{CLOSE_TAG}"))),
                }
            } else {
                let Some(start) = self.buffer.find(OPEN_TAG) else {
                    let text = flush_safe_text(&mut self.buffer);
                    if !text.is_empty() {
                        pieces.push(XmlPiece::Text(text));
                    }
                    break;
                };
                if start > 0 {
                    pieces.push(XmlPiece::Text(self.buffer[..start].to_owned()));
                    self.buffer.replace_range(..start, "");
                }
                self.buffer.replace_range(..OPEN_TAG.len(), "");
                self.inside = true;
            }
        }
        pieces
    }

    /// Flushes at end-of-stream: trailing text is emitted, an unterminated
    /// call body is dropped (it never completed, so it is not a call).
    pub(crate) fn finish(&mut self) -> Vec<XmlPiece> {
        let inside = self.inside;
        let rest = std::mem::take(&mut self.buffer);
        self.inside = false;
        if rest.is_empty() || inside {
            Vec::new()
        } else {
            vec![XmlPiece::Text(rest)]
        }
    }
}

/// Emits text that cannot be part of a future opening tag, keeping a
/// possible partial-tag suffix buffered for the next delta. Only a suffix
/// ending at the buffer's end can still grow into `<tool_call>`, and every
/// prefix of that tag starts with `<`, so the decision hangs on the last `<`.
fn flush_safe_text(buffer: &mut String) -> String {
    let Some(position) = buffer.rfind('<') else {
        return std::mem::take(buffer);
    };
    let tail = &buffer[position..];
    if OPEN_TAG.starts_with(tail) && tail.len() < OPEN_TAG.len() {
        let drained = buffer[..position].to_owned();
        buffer.replace_range(..position, "");
        drained
    } else {
        std::mem::take(buffer)
    }
}

/// Parses one `<tool_call>` body into (name, JSON arguments).
fn parse_call_body(body: &str) -> Option<(String, String)> {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        let name = value
            .get("name")
            .or_else(|| value.get("tool"))
            .and_then(serde_json::Value::as_str)?
            .to_owned();
        let arguments = value
            .get("arguments")
            .or_else(|| value.get("parameters"))
            .or_else(|| value.get("params"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        return Some((name, arguments.to_string()));
    }
    if trimmed.starts_with('<') {
        return parse_xml_body(trimmed);
    }
    None
}

/// Parses `<function=name><parameter=key>value</parameter>…</function>`.
fn parse_xml_body(body: &str) -> Option<(String, String)> {
    let function_start = body.find("<function")?;
    let name_start = body[function_start..].find('=')? + function_start + 1;
    let name_end = body[name_start..].find('>')? + name_start;
    let name = body[name_start..name_end].trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let mut arguments = serde_json::Map::new();
    let mut cursor = name_end + 1;
    while let Some(param_offset) = body[cursor..].find("<parameter") {
        let absolute = cursor + param_offset;
        let key_start = body[absolute..].find('=')? + absolute + 1;
        let key_end = body[key_start..].find('>')? + key_start;
        let key = body[key_start..key_end].trim().to_owned();
        let value_start = key_end + 1;
        let value_end = body[value_start..].find("</parameter>")? + value_start;
        let value = body[value_start..value_end].to_owned();
        arguments.insert(key, serde_json::Value::String(value));
        cursor = value_end + "</parameter>".len();
    }
    Some((name, serde_json::Value::Object(arguments).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(pieces: &[XmlPiece]) -> String {
        pieces
            .iter()
            .map(|piece| match piece {
                XmlPiece::Text(text) => text.clone(),
                XmlPiece::ToolCall { .. } => String::new(),
            })
            .collect()
    }

    #[test]
    fn json_call_streams_across_split_deltas() {
        let mut parser = XmlToolCallParser::default();
        let mut all = Vec::new();
        for delta in [
            "before <tool",
            "_call>{\"na",
            "me\": \"web_search\", \"arguments\": {\"query\": \"rust\"",
            "}}</tool_c",
            "all> after",
        ] {
            all.extend(parser.feed(delta));
        }
        all.extend(parser.finish());
        assert_eq!(texts(&all), "before  after");
        assert_eq!(
            all,
            vec![
                XmlPiece::Text("before ".to_owned()),
                XmlPiece::ToolCall {
                    name: "web_search".to_owned(),
                    arguments: "{\"query\":\"rust\"}".to_owned(),
                },
                XmlPiece::Text(" after".to_owned()),
            ]
        );
    }

    #[test]
    fn xml_parameter_call_parses() {
        let mut parser = XmlToolCallParser::default();
        let pieces = parser.feed(
            "<tool_call><function=read><parameter=path>src/main.rs</parameter></function></tool_call>",
        );
        assert_eq!(
            pieces,
            vec![XmlPiece::ToolCall {
                name: "read".to_owned(),
                arguments: "{\"path\":\"src/main.rs\"}".to_owned(),
            }]
        );
    }

    #[test]
    fn multiple_calls_and_parameters() {
        let mut parser = XmlToolCallParser::default();
        let pieces = parser.feed(
            "<tool_call><function=edit><parameter=path>a</parameter><parameter=old>b</parameter></function></tool_call>\
             mid\
             <tool_call>{\"name\":\"shell\",\"parameters\":{\"cmd\":\"ls\"}}</tool_call>",
        );
        assert_eq!(pieces.len(), 3, "{pieces:?}");
        assert!(matches!(&pieces[0], XmlPiece::ToolCall { name, arguments }
            if name == "edit" && arguments.contains("\"old\":\"b\"")));
        assert_eq!(pieces[1], XmlPiece::Text("mid".to_owned()));
        assert!(matches!(&pieces[2], XmlPiece::ToolCall { name, .. } if name == "shell"));
    }

    #[test]
    fn unterminated_call_is_dropped_and_garbage_falls_back_to_text() {
        let mut parser = XmlToolCallParser::default();
        let pieces = parser.feed("partial <tool_call>{\"name\"");
        assert_eq!(
            pieces,
            vec![XmlPiece::Text("partial ".to_owned())],
            "text before the tag still streams"
        );
        assert!(
            parser.finish().is_empty(),
            "unterminated calls vanish at end-of-stream"
        );

        let mut parser = XmlToolCallParser::default();
        let pieces = parser.feed("<tool_call>not a call</tool_call>");
        assert_eq!(
            pieces,
            vec![XmlPiece::Text(
                "<tool_call>not a call</tool_call>".to_owned()
            )],
            "unparseable bodies stay visible"
        );
    }

    #[test]
    fn plain_text_with_angles_passes_through() {
        let mut parser = XmlToolCallParser::default();
        let mut pieces = parser.feed("a < b and </closing> tags");
        pieces.extend(parser.finish());
        assert_eq!(texts(&pieces), "a < b and </closing> tags");
    }

    #[test]
    fn partial_tag_suffix_is_held_back() {
        let mut parser = XmlToolCallParser::default();
        assert_eq!(
            parser.feed("hello <tool"),
            vec![XmlPiece::Text("hello ".to_owned())],
            "only the tag candidate stays buffered"
        );
        assert_eq!(
            parser.feed("x> world"),
            vec![XmlPiece::Text("<toolx> world".to_owned())],
            "a tag look-alike is not a call and flows through once disproven"
        );
    }
}
