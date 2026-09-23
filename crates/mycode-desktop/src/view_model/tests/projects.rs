//! Project-centric behavior: session bindings, grouping, and parking a chat
//! that belongs to another project.
use super::*;

#[test]
fn session_project_bound_is_what_groups_this_project() {
    let mut state = WorkspaceState {
        sessions: vec![summary("ses-a", 0), summary("ses-b", 0)],
        ..WorkspaceState::default()
    };
    let project = if cfg!(windows) {
        r"D:\my_private_pro\MCode"
    } else {
        "/work/MCode"
    };
    let same_project = if cfg!(windows) {
        r"D:\my_private_pro\Mcode"
    } else {
        "/work/MCode"
    };
    reduce(&mut state, DesktopAction::ProjectOpened(project.to_owned()));
    let grouped = group_sessions(
        &state.sessions,
        &state.session_projects,
        state.project_dir.as_deref(),
    );
    assert!(grouped.current.is_empty(), "ProjectOpened does not bind");
    assert_eq!(grouped.unbound.len(), 2);

    reduce(
        &mut state,
        DesktopAction::SessionProjectBound {
            session_id: "ses-a".to_owned(),
            project: project.to_owned(),
        },
    );
    let grouped = group_sessions(&state.sessions, &state.session_projects, Some(same_project));
    assert_eq!(grouped.current.len(), 1);
    assert_eq!(grouped.current[0].session_id, "ses-a");
    assert_eq!(grouped.unbound.len(), 1);

    let other = if cfg!(windows) {
        r"D:\other\repo"
    } else {
        "/other/repo"
    };
    reduce(
        &mut state,
        DesktopAction::SessionProjectBound {
            session_id: "ses-a".to_owned(),
            project: other.to_owned(),
        },
    );
    assert_eq!(
        project_of_session(&state.session_projects, "ses-a"),
        Some(project),
        "a session does not move to a second folder"
    );

    reduce(
        &mut state,
        DesktopAction::WorkspaceRootAdded(project.to_owned()),
    );
    reduce(
        &mut state,
        DesktopAction::WorkspaceRootAdded(other.to_owned()),
    );
    reduce(
        &mut state,
        DesktopAction::WorkspaceFolderFocused {
            session_id: "ses-a".to_owned(),
            project: other.to_owned(),
        },
    );
    assert_eq!(
        project_of_session(&state.session_projects, "ses-a"),
        Some(other),
        "focusing a workspace folder moves the open chat"
    );
    assert!(state.workspace_roots.iter().any(|root| root == project));
    assert_eq!(state.project_dir.as_deref(), Some(other));
}

#[test]
fn parking_hides_another_projects_chat() {
    let mut state = WorkspaceState {
        sessions: vec![summary("ses-a", 2)],
        active: Some(ActiveConversation {
            session_id: "ses-a".to_owned(),
            branch_id: "br-ses-a".to_owned(),
            head: "evt".to_owned(),
            entries: vec![ConversationEntry {
                event_id: "evt".to_owned(),
                kind: EntryKind::UserMessage,
                text: "work in A".into(),
                call_id: None,
                thinking: String::new(),
            }],
            streaming: None,
        }),
        sending: true,
        queued: vec!["follow up".to_owned()],
        todo_rows: vec![("plan".to_owned(), "pending".to_owned())],
        composer_draft: "draft".to_owned(),
        ..WorkspaceState::default()
    };
    state.sessions[0].active = true;
    reduce(&mut state, DesktopAction::ConversationParked);
    assert!(state.active.is_none());
    assert!(!state.sessions[0].active);
    assert!(!state.sending);
    assert!(state.queued.is_empty());
    assert!(state.todo_rows.is_empty());
    assert!(state.composer_draft.is_empty());
}
