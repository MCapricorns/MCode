//! Project and workspace transitions: session-to-project bindings, recents,
//! and the workspace folder list.

use crate::view_model::{WorkspaceState, same_project_path};

pub(super) fn bind_session_project(
    state: &mut WorkspaceState,
    session_id: String,
    project: String,
) {
    state
        .session_projects
        .retain(|(existing, _)| existing != &session_id);
    state.session_projects.insert(0, (session_id, project));
    state
        .session_projects
        .truncate(mycode_config::MAX_SESSION_PROJECTS);
}

/// The `WorkspaceRootAdded` transition: a folder was added to the workspace.
pub(super) fn workspace_root_added(state: &mut WorkspaceState, project: String) {
    if !project.trim().is_empty() {
        state
            .workspace_roots
            .retain(|existing| !same_project_path(existing, &project));
        state.workspace_roots.insert(0, project.clone());
        state
            .workspace_roots
            .truncate(mycode_config::MAX_WORKSPACE_ROOTS);
        state.recents.retain(|existing| existing != &project);
        state.recents.insert(0, project.clone());
        state.recents.truncate(mycode_config::MAX_RECENT_PROJECTS);
        if state.project_dir.is_none() {
            state.project_dir = Some(project);
        }
    }
}

/// The `SessionProjectBound` transition: a session was bound to a project in
/// the durable map.
pub(super) fn session_project_bound(
    state: &mut WorkspaceState,
    session_id: String,
    project: String,
) {
    // A session keeps the first folder it was bound to. Opening
    // another folder starts a different session instead of moving
    // this conversation's tool directory.
    let locked = state
        .session_projects
        .iter()
        .find(|(existing, _)| existing == &session_id)
        .is_some_and(|(_, existing)| !same_project_path(existing, &project));
    if !locked {
        bind_session_project(state, session_id, project);
    }
}
