//! Reducer and projection tests, kept beside the declarations they exercise.
//! The mod file holds the shared fixtures; each topical submodule covers one
//! slice of the behavior.
use super::reduce::{selected_model_supports_reasoning, selected_reasoning_levels};
use super::*;

mod chat;
mod jobs;
mod models;
mod panels;
mod projects;
mod sessions;
mod settings;
mod usage;

fn summary(id: &str, count: u64) -> SessionSummary {
    SessionSummary {
        session_id: id.to_owned(),
        root_branch_id: format!("br-{id}"),
        title: format!("chat {id}"),
        event_count: count,
        active: false,
    }
}

fn opened_conversation() -> WorkspaceState {
    WorkspaceState {
        active: Some(ActiveConversation {
            session_id: "ses1-a".to_owned(),
            branch_id: "br1-a".to_owned(),
            head: "empty".to_owned(),
            entries: Vec::new(),
            streaming: None,
        }),
        sending: true,
        ..WorkspaceState::default()
    }
}
