//! Token-usage projections and snapshots.
use super::*;

#[test]
fn switching_models_filters_inspector_usage() {
    let mut state = WorkspaceState {
        selected_model: Some("o3".to_owned()),
        usage_totals: vec![UsageTotal {
            key: "openai/gpt-4.1".to_owned(),
            input: 9,
            output: 1,
            requests: 1,
            ..UsageTotal::default()
        }],
        last_turn: Some(TurnStats {
            model: "gpt-4.1".to_owned(),
            input: 9,
            output: 1,
            cache: None,
            elapsed_ms: 1,
        }),
        ..WorkspaceState::default()
    };
    assert!(!usage_key_matches("openai/gpt-4.1", "o3"));
    reduce(
        &mut state,
        DesktopAction::UsageSnapshot {
            model: "o3".to_owned(),
            input: 20,
            output: 3,
            cache: None,
            elapsed_ms: 4,
        },
    );
    assert_eq!(state.live_turn.as_ref().map(|turn| turn.input), Some(20));
    assert_eq!(
        parse_usage_text("o3: 20 in / 3 out"),
        Some(("o3".to_owned(), 20, 3))
    );
}
