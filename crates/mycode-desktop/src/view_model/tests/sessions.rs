//! Session lifecycle: the sidebar list, opening and switching conversations.
use super::*;

#[test]
fn session_list_marks_the_open_session() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::SessionsLoaded(vec![summary("ses1-a", 1), summary("ses1-b", 2)]),
    );
    assert!(!state.sessions[0].active);

    reduce(
        &mut state,
        DesktopAction::SessionCreated(summary("ses1-a", 0)),
    );
    assert!(state.sessions[0].active);
    assert_eq!(state.sessions.len(), 2);
    assert_eq!(
        state.active.as_ref().map(|c| c.head.as_str()),
        Some("empty")
    );

    reduce(
        &mut state,
        DesktopAction::SessionsLoaded(vec![summary("ses1-a", 1), summary("ses1-b", 2)]),
    );
    assert!(state.sessions[0].active, "reload keeps the open mark");
    assert!(!state.sessions[1].active);
}

#[test]
fn revealing_earlier_messages_resets_when_the_session_changes() {
    let mut state = WorkspaceState::default();
    reduce(&mut state, DesktopAction::TranscriptRevealMore);
    assert_eq!(state.transcript_extra, TRANSCRIPT_PAGE);
    reduce(
        &mut state,
        DesktopAction::SessionCreated(summary("ses1-a", 0)),
    );
    assert_eq!(state.transcript_extra, 0);
    reduce(&mut state, DesktopAction::TranscriptRevealMore);
    reduce(
        &mut state,
        DesktopAction::ConversationOpened(ActiveConversation {
            session_id: "ses1-b".to_owned(),
            branch_id: "br-b".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            streaming: None,
        }),
    );
    assert_eq!(state.transcript_extra, 0);
}

#[test]
fn switching_sessions_drops_the_follow_up_queue() {
    let mut state = opened_conversation();
    state.queued.push("later".to_owned());
    reduce(
        &mut state,
        DesktopAction::ConversationOpened(ActiveConversation {
            session_id: "ses2-b".to_owned(),
            branch_id: "br2-b".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            streaming: None,
        }),
    );
    assert!(state.queued.is_empty());
    assert!(!state.sending);
}
