//! Auto-compaction: Codex-style checkpoint summaries before each provider request.
//!
//! Trigger is 90% of the usable context window (95% of the model window, same
//! safety margin as Codex). The tail keeps the last ~20k tokens of messages
//! without splitting a tool-call pair. The head is summarized with a handoff
//! prompt (goals, files, decisions, errors, next steps). The session ledger
//! is never rewritten; only the in-memory request history shrinks.

use mycode_core::{Message, Provider as _, Request, StreamEvent};
use mycode_providers::WireProvider;
use tokio_util::sync::CancellationToken;

/// Fallback trigger when the catalog/settings have no context window.
const DEFAULT_THRESHOLD_TOKENS: usize = 48_000;
/// Codex keeps roughly this many recent tokens after the summary.
const TAIL_TOKEN_BUDGET: usize = 20_000;
/// Usable window as a percent of the raw model context (Codex 95%).
const USABLE_WINDOW_PERCENT: u64 = 95;
/// Auto-compact trigger as a percent of the usable window (Codex 90%).
const TRIGGER_PERCENT: u64 = 90;
const SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
const TRANSCRIPT_CAP_CHARS: usize = 300_000;
const EXCERPT_CHARS: usize = 4_000;
const SUMMARY_PREFIX: &str = "COMPACTION SUMMARY";

/// Tokens that fire auto-compaction for this model window.
#[must_use]
pub(crate) fn compaction_threshold(context_window: u64) -> usize {
    if context_window == 0 {
        return DEFAULT_THRESHOLD_TOKENS;
    }
    let usable = context_window.saturating_mul(USABLE_WINDOW_PERCENT) / 100;
    (usable.saturating_mul(TRIGGER_PERCENT) / 100) as usize
}

/// Inputs that stay constant for one turn's compaction attempts.
pub(crate) struct CompactScope<'a> {
    pub home: &'a mycode_config::HomeLayout,
    pub wire: &'a WireProvider,
    pub model: &'a str,
    pub session_id: &'a str,
    pub branch_id: &'a str,
    pub head: &'a str,
    pub context_window: u64,
}

/// Compacts history before a provider request. Failures degrade to the
/// original history so a turn never dies on housekeeping.
pub(crate) async fn compact_history(
    scope: &CompactScope<'_>,
    history: Vec<Message>,
) -> Vec<Message> {
    let threshold = compaction_threshold(scope.context_window);
    let Some(head_end) = compaction_split(&history, threshold, TAIL_TOKEN_BUDGET) else {
        return history;
    };
    let prior = mycode_config::read_compaction(scope.home, scope.session_id)
        .ok()
        .flatten()
        .filter(|checkpoint| checkpoint.branch_id == scope.branch_id);
    let prior_summary = prior.as_ref().map(|checkpoint| checkpoint.summary.as_str());
    let transcript = compaction_transcript(prior_summary, &history[..head_end]);
    let summarized = tokio::time::timeout(
        SUMMARY_TIMEOUT,
        summarize_transcript(scope.wire, &transcript),
    )
    .await;
    let summary = match summarized {
        Ok(Ok(summary)) => summary,
        Ok(Err(message)) => {
            eprintln!("[mycode-compaction] skipped: {message}");
            return history;
        }
        Err(_) => {
            eprintln!("[mycode-compaction] skipped: summary timed out");
            return history;
        }
    };
    let checkpoint = mycode_config::CompactionCheckpoint {
        format_version: mycode_config::COMPACTION_FORMAT_VERSION,
        kind: mycode_config::COMPACTION_KIND.to_owned(),
        session_id: scope.session_id.to_owned(),
        branch_id: scope.branch_id.to_owned(),
        covered_head: scope.head.to_owned(),
        covered_messages: head_end,
        summary: summary.clone(),
        model: scope.model.to_owned(),
        created_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
    };
    if let Err(error) = mycode_config::write_compaction(scope.home, scope.session_id, &checkpoint) {
        eprintln!("[mycode-compaction] checkpoint write failed: {error:?}");
        return history;
    }
    let mut compacted = Vec::with_capacity(history.len() - head_end + 1);
    compacted.push(Message::User(mycode_core::UserMessage::text(format!(
        "{SUMMARY_PREFIX}\n\n{summary}"
    ))));
    compacted.extend(history[head_end..].iter().cloned());
    eprintln!(
        "[mycode-compaction] replaced {head_end} messages with a checkpoint ({} remain)",
        compacted.len()
    );
    compacted
}

fn message_tokens(message: &Message) -> usize {
    mycode_config::estimate_tokens(&message_text(message))
}

fn message_text(message: &Message) -> String {
    match message {
        Message::User(user) => blocks_text(&user.content),
        Message::Assistant(assistant) => blocks_text(&assistant.blocks),
        Message::ToolResult(result) => {
            let body = blocks_text(&result.content);
            format!("tool_result {} {body}", result.tool_call_id)
        }
        Message::Custom(custom) => format!("custom {}", custom.kind),
    }
}

fn blocks_text(blocks: &[mycode_core::ContentBlock]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            mycode_core::ContentBlock::Text(text) => parts.push(text.text.clone()),
            mycode_core::ContentBlock::ToolCall(call) => {
                parts.push(format!("tool_call {} {}", call.name, call.arguments));
            }
            mycode_core::ContentBlock::Thinking(_) | mycode_core::ContentBlock::Image(_) => {}
        }
    }
    parts.join("\n")
}

fn is_tool_result(message: &Message) -> bool {
    matches!(message, Message::ToolResult(_))
}

fn is_summary_message(message: &Message) -> bool {
    matches!(message, Message::User(user) if blocks_text(&user.content).starts_with(SUMMARY_PREFIX))
}

/// Returns the split index when compaction is due: everything before it is
/// summarized, everything from it on stays verbatim.
fn compaction_split(history: &[Message], threshold: usize, tail_budget: usize) -> Option<usize> {
    if history.len() < 2 {
        return None;
    }
    let estimate: usize = history.iter().map(message_tokens).sum();
    if estimate <= threshold {
        return None;
    }
    let mut used = 0usize;
    let mut tail_start = history.len();
    while tail_start > 0 {
        let tokens = message_tokens(&history[tail_start - 1]);
        if used > 0 && used.saturating_add(tokens) > tail_budget {
            break;
        }
        used = used.saturating_add(tokens);
        tail_start -= 1;
    }
    while tail_start > 0 && is_tool_result(&history[tail_start]) {
        tail_start -= 1;
    }
    if tail_start == 0 || (tail_start == 1 && is_summary_message(&history[0])) {
        return None;
    }
    Some(tail_start)
}

fn compaction_transcript(prior_summary: Option<&str>, head: &[Message]) -> String {
    let mut transcript = String::new();
    if let Some(summary) = prior_summary {
        transcript.push_str("Previous checkpoint:\n");
        transcript.push_str(summary);
        transcript.push_str("\n\n");
    }
    for message in head {
        if is_summary_message(message) {
            continue;
        }
        let role = match message {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "tool",
            Message::Custom(_) => "custom",
        };
        let text = message_text(message);
        let cut = text
            .char_indices()
            .nth(EXCERPT_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(text.len());
        transcript.push_str(&format!("[{role}] {}\n\n", &text[..cut]));
    }
    let count = transcript.chars().count();
    if count > TRANSCRIPT_CAP_CHARS {
        let skip = transcript
            .char_indices()
            .nth(count - TRANSCRIPT_CAP_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(0);
        format!("...earlier content elided...\n{}", &transcript[skip..])
    } else {
        transcript
    }
}

async fn summarize_transcript(wire: &WireProvider, transcript: &str) -> Result<String, String> {
    let request = Request::new()
        .with_system_prompt(
            "You are performing a CONTEXT CHECKPOINT COMPACTION. Create a \
handoff summary for another coding-agent model that will resume the task.\n\
Write dense factual prose, no preamble. Cover:\n\
- User goals and constraints\n\
- Decisions made, and why\n\
- Files and paths touched (created, edited, read)\n\
- Commands run and their outcomes\n\
- Current work, open tasks, and unresolved errors\n\
- Clear next steps\n\
Preserve names, paths, and error text. Do not invent work that did not happen.",
        )
        .with_message(Message::User(mycode_core::UserMessage::text(format!(
            "Summarize the following conversation for continuation:\n\n{transcript}"
        ))));
    let cancel = CancellationToken::new();
    let mut stream = wire
        .stream(&request, cancel)
        .await
        .map_err(|error| format!("summary request failed: {error:?}"))?;
    let mut summary = String::new();
    loop {
        let Some(event) = stream.next().await else {
            return Err("summary stream ended without completion".to_owned());
        };
        match event {
            StreamEvent::TextDelta(delta) => summary.push_str(&delta),
            StreamEvent::Done { .. } => break,
            StreamEvent::Error(error) => return Err(format!("summary stream failed: {error:?}")),
            _ => {}
        }
    }
    let chars: Vec<char> = summary.chars().collect();
    if chars.len() > mycode_config::MAX_SUMMARY_CHARS {
        summary = chars[..mycode_config::MAX_SUMMARY_CHARS].iter().collect();
    }
    if summary.trim().is_empty() {
        return Err("summary was empty".to_owned());
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(text: &str) -> Message {
        Message::User(mycode_core::UserMessage::text(text))
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant(mycode_core::AssistantMessage {
            blocks: vec![mycode_core::ContentBlock::Text(
                mycode_core::TextBlock::new(text),
            )],
            usage: None,
            stop_reason: mycode_core::StopReason::Stop,
        })
    }

    fn tool_result(id: &str, text: &str) -> Message {
        Message::ToolResult(mycode_core::ToolResultMessage {
            tool_call_id: id.to_owned(),
            content: vec![mycode_core::ContentBlock::Text(
                mycode_core::TextBlock::new(text),
            )],
            is_error: false,
            details: None,
        })
    }

    #[test]
    fn threshold_uses_ninety_percent_of_usable_window() {
        // 128k * 0.95 * 0.90 = 109440
        assert_eq!(compaction_threshold(128_000), 109_440);
        assert_eq!(compaction_threshold(0), DEFAULT_THRESHOLD_TOKENS);
    }

    #[test]
    fn compaction_split_skips_small_history() {
        let history: Vec<Message> = (0..20)
            .map(|index| user_msg(&format!("message {index}")))
            .collect();
        assert!(compaction_split(&history, DEFAULT_THRESHOLD_TOKENS, TAIL_TOKEN_BUDGET).is_none());
    }

    #[test]
    fn compaction_split_keeps_a_token_budget_tail() {
        let history: Vec<Message> = (0..40)
            .map(|index| {
                let text = format!("message {index}: {}", "x".repeat(6_000));
                if index % 2 == 0 {
                    user_msg(&text)
                } else {
                    assistant_msg(&text)
                }
            })
            .collect();
        let split = compaction_split(&history, DEFAULT_THRESHOLD_TOKENS, TAIL_TOKEN_BUDGET)
            .expect("compaction due");
        assert!(split > 0);
        assert!(split < history.len());
        let tail_tokens: usize = history[split..].iter().map(message_tokens).sum();
        assert!(tail_tokens <= TAIL_TOKEN_BUDGET + message_tokens(&history[split]));
    }

    #[test]
    fn compaction_split_never_drops_a_single_turn() {
        let history = vec![user_msg(&format!("huge {}", "x".repeat(100_000)))];
        assert!(compaction_split(&history, 1, TAIL_TOKEN_BUDGET).is_none());
    }

    #[test]
    fn compaction_split_does_not_cut_a_tool_pair() {
        let mut history = vec![user_msg(&format!("start {}", "x".repeat(80_000)))];
        history.push(Message::Assistant(mycode_core::AssistantMessage {
            blocks: vec![mycode_core::ContentBlock::ToolCall(
                mycode_core::ToolCall::new("call-1", "read", serde_json::json!({"path": "a.rs"})),
            )],
            usage: None,
            stop_reason: mycode_core::StopReason::ToolUse,
        }));
        history.push(tool_result("call-1", &"y".repeat(4_000)));
        let split = compaction_split(&history, 1_000, 8_000).expect("compaction due");
        assert!(
            !is_tool_result(&history[split]),
            "tail must start on the assistant tool call, not the result"
        );
    }

    #[test]
    fn compaction_counts_tool_results() {
        fn call(id: &str) -> Message {
            Message::Assistant(mycode_core::AssistantMessage {
                blocks: vec![mycode_core::ContentBlock::ToolCall(
                    mycode_core::ToolCall::new(id, "read", serde_json::json!({"path": "a.rs"})),
                )],
                usage: None,
                stop_reason: mycode_core::StopReason::ToolUse,
            })
        }
        let history = vec![
            user_msg("look"),
            call("c1"),
            tool_result("c1", &"z".repeat(120_000)),
            call("c2"),
            tool_result("c2", &"z".repeat(120_000)),
        ];
        assert!(
            compaction_split(&history, DEFAULT_THRESHOLD_TOKENS, TAIL_TOKEN_BUDGET).is_some(),
            "tool-heavy turns must be eligible for compaction"
        );
    }

    #[test]
    fn compaction_transcript_includes_prior_summary_and_roles() {
        let head = vec![user_msg("hello"), assistant_msg("hi there")];
        let transcript = compaction_transcript(Some("prior digest"), &head);
        assert!(transcript.contains("prior digest"));
        assert!(transcript.contains("[user] hello"));
        assert!(transcript.contains("[assistant] hi there"));
    }

    #[test]
    fn compaction_transcript_caps_total_chars() {
        let head: Vec<Message> = (0..200)
            .map(|index| user_msg(&format!("{index}: {}", "y".repeat(4_000))))
            .collect();
        let transcript = compaction_transcript(None, &head);
        assert!(transcript.starts_with("...earlier content elided..."));
        assert!(transcript.chars().count() <= TRANSCRIPT_CAP_CHARS + 64);
    }
}
