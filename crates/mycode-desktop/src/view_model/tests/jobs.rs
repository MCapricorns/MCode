//! Live subagent cards fed by `task` tool progress.
use super::*;

#[test]
fn task_progress_fills_the_subagent_panel() {
    let mut state = opened_conversation();
    reduce(
        &mut state,
        DesktopAction::ToolStarted {
            call_id: "call-task".to_owned(),
            name: "task".to_owned(),
            target: String::new(),
        },
    );
    reduce(
        &mut state,
        DesktopAction::ToolProgress {
            call_id: "call-task".to_owned(),
            name: String::new(),
            message: "task|scout|queued|audit the parser".to_owned(),
        },
    );
    reduce(
        &mut state,
        DesktopAction::ToolProgress {
            call_id: "call-task".to_owned(),
            name: String::new(),
            message: "task|scout|tool|read".to_owned(),
        },
    );
    assert_eq!(state.live_jobs.len(), 1);
    assert_eq!(state.live_jobs[0].role, "scout");
    assert_eq!(state.live_jobs[0].label, "audit the parser");
    assert_eq!(state.live_jobs[0].step, "running read");
    assert!(!state.live_jobs[0].done);
    assert_eq!(
        state
            .active
            .as_ref()
            .and_then(|conversation| conversation.streaming.as_ref())
            .map(|streaming| streaming.status.as_str()),
        Some("scout · running read")
    );
    reduce(
        &mut state,
        DesktopAction::ToolResultAppended(ConversationEntry {
            event_id: "evt-1".to_owned(),
            kind: EntryKind::ToolResult,
            text: "done".into(),
            call_id: Some("call-task".to_owned()),
            thinking: String::new(),
        }),
    );
    assert!(state.live_jobs[0].done);
    reduce(
        &mut state,
        DesktopAction::ChatDone {
            head: "head-2".to_owned(),
            entry: ConversationEntry {
                event_id: "evt-2".to_owned(),
                kind: EntryKind::AssistantMessage,
                text: "ok".into(),
                call_id: None,
                thinking: String::new(),
            },
        },
    );
    assert!(state.live_jobs.is_empty());
}

#[test]
fn concurrent_task_progress_keeps_one_card_per_call() {
    let mut state = opened_conversation();
    for (call_id, label) in [
        ("call-a", "build"),
        ("call-b", "framework"),
        ("call-c", "gaming_plugins"),
        ("call-d", "secommon"),
    ] {
        reduce(
            &mut state,
            DesktopAction::ToolStarted {
                call_id: call_id.to_owned(),
                name: "task".to_owned(),
                target: String::new(),
            },
        );
        reduce(
            &mut state,
            DesktopAction::ToolProgress {
                call_id: call_id.to_owned(),
                name: String::new(),
                message: format!("task|scout|queued|{label}"),
            },
        );
    }
    assert_eq!(state.live_jobs.len(), 4);
    assert_eq!(
        state
            .live_jobs
            .iter()
            .map(|job| job.label.as_str())
            .collect::<Vec<_>>(),
        vec!["build", "framework", "gaming_plugins", "secommon"]
    );
    assert!(
        state
            .live_jobs
            .iter()
            .all(|job| !job.done && job.role == "scout")
    );
}
