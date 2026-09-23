//! Turn lifecycle: sending, streaming buffers, failures, and the queued
//! follow-up list.
use super::*;

#[test]
fn message_send_appends_and_clears_the_draft() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::SessionCreated(summary("ses1-a", 0)),
    );
    reduce(
        &mut state,
        DesktopAction::ComposerChanged("hello".to_owned()),
    );
    reduce(
        &mut state,
        DesktopAction::MessageSent {
            head: "evt1-1".to_owned(),
            entry: ConversationEntry {
                event_id: "evt1-1".to_owned(),
                kind: EntryKind::UserMessage,
                text: "hello".into(),
                call_id: None,
                thinking: String::new(),
            },
        },
    );
    let conversation = state.active.as_ref().expect("open");
    assert_eq!(conversation.head, "evt1-1");
    assert_eq!(conversation.entries.len(), 1);
    assert!(state.composer_draft.is_empty());
    assert!(!state.sending);
}

#[test]
fn turn_armed_shows_working_status_before_the_first_token() {
    let mut state = opened_conversation();
    state.sending = false;
    reduce(&mut state, DesktopAction::TurnArmed);
    assert!(state.sending);
    let streaming = state
        .active
        .as_ref()
        .expect("conversation")
        .streaming
        .as_ref()
        .expect("streaming");
    assert_eq!(streaming.status, "Waiting for the model");
    assert!(streaming.text.is_empty());
    assert!(streaming.thinking.is_empty());
}

#[test]
fn chat_stream_buffers_then_commits() {
    let mut state = opened_conversation();
    reduce(&mut state, DesktopAction::ChatDelta("hel".to_owned()));
    reduce(&mut state, DesktopAction::ChatDelta("lo".to_owned()));
    reduce(
        &mut state,
        DesktopAction::ChatThinkingDelta("why".to_owned()),
    );
    let conversation = state.active.as_ref().expect("conversation");
    assert_eq!(
        conversation.streaming.as_ref().expect("streaming").text,
        "hello"
    );
    assert_eq!(
        conversation.streaming.as_ref().expect("streaming").thinking,
        "why"
    );
    assert_eq!(
        conversation.streaming.as_ref().expect("streaming").status,
        "Thinking"
    );
    assert!(state.sending, "sending until the turn ends");

    reduce(
        &mut state,
        DesktopAction::ChatDone {
            head: "evt1-x".to_owned(),
            entry: ConversationEntry {
                event_id: "evt1-x".to_owned(),
                kind: EntryKind::AssistantMessage,
                text: "hello".into(),
                call_id: None,
                thinking: "why".into(),
            },
        },
    );
    let conversation = state.active.as_ref().expect("conversation");
    assert_eq!(conversation.head, "evt1-x");
    assert!(conversation.streaming.is_none());
    assert_eq!(conversation.entries.len(), 1);
    assert_eq!(conversation.entries[0].thinking, "why");
    assert!(!state.sending);
}

#[test]
fn chat_failure_clears_streaming_and_flags_error() {
    let mut state = opened_conversation();
    reduce(&mut state, DesktopAction::ChatDelta("partial".to_owned()));
    reduce(
        &mut state,
        DesktopAction::ChatFailed("provider down".to_owned()),
    );
    assert!(
        state
            .active
            .as_ref()
            .expect("conversation")
            .streaming
            .is_none()
    );
    assert_eq!(state.error.as_deref(), Some("provider down"));
    assert!(!state.sending);
}

#[test]
fn chat_cancel_resets_the_turn_without_an_error_banner() {
    let mut state = opened_conversation();
    reduce(&mut state, DesktopAction::ChatDelta("partial".to_owned()));
    reduce(
        &mut state,
        DesktopAction::ChatFailed(CHAT_CANCELLED.to_owned()),
    );
    assert!(
        state
            .active
            .as_ref()
            .expect("conversation")
            .streaming
            .is_none()
    );
    assert_eq!(state.error, None, "a cancel is not a failure");
    assert!(!state.sending);
}

#[test]
fn chat_deltas_are_dropped_without_a_conversation() {
    let mut state = WorkspaceState::default();
    reduce(&mut state, DesktopAction::ChatDelta("ignored".to_owned()));
    assert!(state.active.is_none());
    assert!(!state.sending);
}

#[test]
fn follow_ups_queue_while_a_turn_is_in_flight() {
    let mut state = opened_conversation();
    reduce(
        &mut state,
        DesktopAction::MessageQueued("then check tests".to_owned()),
    );
    reduce(
        &mut state,
        DesktopAction::MessageQueued("then commit".to_owned()),
    );
    assert_eq!(
        state.queued,
        vec!["then check tests".to_owned(), "then commit".to_owned()]
    );
    assert!(state.composer_draft.is_empty());
    reduce(&mut state, DesktopAction::QueuedMessageTaken);
    assert_eq!(state.queued, vec!["then commit".to_owned()]);
    reduce(&mut state, DesktopAction::QueuedMessageRemoved(0));
    assert!(state.queued.is_empty());
}

#[test]
fn a_full_queue_rejects_another_follow_up() {
    let mut state = opened_conversation();
    for index in 0..MAX_QUEUED_MESSAGES {
        reduce(
            &mut state,
            DesktopAction::MessageQueued(format!("item-{index}")),
        );
    }
    reduce(
        &mut state,
        DesktopAction::MessageQueued("overflow".to_owned()),
    );
    assert_eq!(state.queued.len(), MAX_QUEUED_MESSAGES);
    assert!(!state.queued.iter().any(|item| item == "overflow"));
}
