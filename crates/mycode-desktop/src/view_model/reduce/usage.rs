//! Token accounting: the rebuild that replays durable usage records when a
//! recovered conversation opens.

use crate::view_model::{EntryKind, TurnStats, UsageTotal, WorkspaceState, parse_usage_text};

pub(super) fn rebuild_session_usage(state: &mut WorkspaceState) {
    let Some(entries) = state
        .active
        .as_ref()
        .map(|conversation| &conversation.entries)
    else {
        state.usage_totals.clear();
        state.last_turn = None;
        return;
    };
    let mut totals: Vec<UsageTotal> = Vec::new();
    let mut last = None;
    for entry in entries {
        if entry.kind != EntryKind::Usage {
            continue;
        }
        let Some((model, input, output)) = parse_usage_text(&entry.text) else {
            continue;
        };
        if let Some(row) = totals.iter_mut().find(|row| row.key == model) {
            row.input = row.input.saturating_add(input);
            row.output = row.output.saturating_add(output);
            row.requests = row.requests.saturating_add(1);
        } else {
            totals.push(UsageTotal {
                key: model.clone(),
                input,
                output,
                requests: 1,
                ..UsageTotal::default()
            });
        }
        last = Some(TurnStats {
            model,
            input,
            output,
            cache: None,
            elapsed_ms: 0,
        });
    }
    state.usage_totals = totals;
    state.last_turn = last;
}
