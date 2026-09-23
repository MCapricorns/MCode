//! Side-panel and composer surfaces: todos, asks, the Copilot sign-in, the
//! skills list, mention triggers, and the transcript window.
use super::*;
use crate::view_model::chat::TRANSCRIPT_TAIL;

#[test]
fn transcript_start_keeps_a_fixed_tail() {
    assert_eq!(transcript_start(10, 0), 0);
    assert_eq!(transcript_start(TRANSCRIPT_TAIL, 0), 0);
    assert_eq!(transcript_start(TRANSCRIPT_TAIL + 7, 0), 7);
    assert_eq!(transcript_start(TRANSCRIPT_TAIL + 7, TRANSCRIPT_PAGE), 0);
}

#[test]
fn todo_updates_drop_completed_rows() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::TodoUpdated(vec![
            ("plan".to_owned(), "in progress".to_owned()),
            ("ship".to_owned(), "done".to_owned()),
            ("review".to_owned(), "pending".to_owned()),
        ]),
    );
    assert_eq!(
        state.todo_rows,
        vec![
            ("plan".to_owned(), "in progress".to_owned()),
            ("review".to_owned(), "pending".to_owned()),
        ]
    );
    reduce(
        &mut state,
        DesktopAction::TodoUpdated(vec![("plan".to_owned(), "done".to_owned())]),
    );
    assert!(state.todo_rows.is_empty());
}

#[test]
fn ask_picks_accumulate_until_submit() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::AskRequested(vec![
            ("one?".to_owned(), vec!["a".to_owned()], false),
            ("two?".to_owned(), Vec::new(), true),
        ]),
    );
    assert_eq!(state.ask_answers, vec!["", ""]);
    reduce(
        &mut state,
        DesktopAction::AskChoicePicked {
            index: 0,
            answer: "a".to_owned(),
        },
    );
    assert_eq!(state.ask_answers[0], "a");
    reduce(&mut state, DesktopAction::AskAnswered);
    assert!(state.pending_ask.is_none());
    assert!(state.ask_answers.is_empty());
}

#[test]
fn copilot_sign_in_lifecycle_shows_code_then_error_or_clear() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::CopilotSignInStarted(CopilotSignIn {
            user_code: "AB12-CD34".to_owned(),
            verification_uri: "https://github.com/login/device".to_owned(),
        }),
    );
    assert_eq!(
        state
            .copilot_sign_in
            .as_ref()
            .map(|sign_in| sign_in.user_code.as_str()),
        Some("AB12-CD34")
    );
    assert!(state.copilot_error.is_none());

    reduce(
        &mut state,
        DesktopAction::CopilotSignInFinished(Err("the request was denied on GitHub".to_owned())),
    );
    assert!(state.copilot_sign_in.is_none());
    assert_eq!(
        state.copilot_error.as_deref(),
        Some("the request was denied on GitHub")
    );

    reduce(
        &mut state,
        DesktopAction::CopilotSignInStarted(CopilotSignIn {
            user_code: "ZZ99".to_owned(),
            verification_uri: "https://github.com/login/device".to_owned(),
        }),
    );
    reduce(&mut state, DesktopAction::CopilotSignInFinished(Ok(())));
    assert!(state.copilot_sign_in.is_none());
    assert!(state.copilot_error.is_none(), "success clears the error");
}

#[test]
fn composer_is_bounded_by_characters() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::ComposerChanged("你好".repeat(40_000)),
    );
    assert_eq!(
        state.composer_draft.chars().count(),
        MAX_COMPOSER_CHARS,
        "CJK input is truncated by chars, not bytes"
    );
}

#[test]
fn at_trigger_indexes_cwd_even_with_an_empty_fragment() {
    let mut state = WorkspaceState::default();
    reduce(&mut state, DesktopAction::ComposerChanged("@".to_owned()));
    let mention = state.mention.expect("mention");
    assert_eq!(mention.kind, MentionKind::File);
    assert!(mention.fragment.is_empty());
}

#[test]
fn skills_panel_keeps_discovered_slash_commands() {
    let mut state = WorkspaceState::default();
    reduce(
        &mut state,
        DesktopAction::SkillsLoaded(vec![SkillEntry {
            slug: "review".to_owned(),
            title: "Review".to_owned(),
            path: "/tmp/review/SKILL.md".to_owned(),
            global: true,
        }]),
    );
    assert_eq!(state.skills.len(), 1);
    assert_eq!(state.skills[0].slug, "review");
    assert!(state.skills[0].global);
}
