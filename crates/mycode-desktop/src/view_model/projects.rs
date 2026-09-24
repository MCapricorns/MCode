//! Project- and workspace-shaped projections: path comparison, session
//! bindings, and the active-workspace view of the sidebar.

use mycode_app::SessionSummary;

use super::state::WorkspaceState;

/// The workspace the sidebar shows. A stale active id falls back to the
/// first workspace, which is also where unbound sessions live.
#[must_use]
pub fn active_workspace(state: &WorkspaceState) -> Option<&mycode_config::WorkspaceDef> {
    match state.active_workspace.as_deref() {
        Some(id) => state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .or_else(|| state.workspaces.first()),
        None => state.workspaces.first(),
    }
}

/// The workspace a session belongs to; a session without a binding predates
/// named workspaces and belongs to the first one.
#[must_use]
pub fn workspace_of_session<'a>(
    state: &'a WorkspaceState,
    session_id: &str,
) -> Option<&'a mycode_config::WorkspaceDef> {
    let bound = state
        .session_workspaces
        .iter()
        .find(|(existing, _)| existing == session_id)
        .map(|(_, workspace)| workspace.as_str());
    match bound {
        Some(id) => state.workspaces.iter().find(|workspace| workspace.id == id),
        None => state.workspaces.first(),
    }
}

/// Task cards belong to the open session's project. Leaving that project
/// hides them instead of leaving another project's work on screen.
#[must_use]
pub(crate) fn task_surface_visible(state: &WorkspaceState) -> bool {
    let Some(session_id) = state
        .active
        .as_ref()
        .map(|conversation| conversation.session_id.as_str())
    else {
        return false;
    };
    let Some(project) = state.project_dir.as_deref() else {
        return false;
    };
    state
        .session_projects
        .iter()
        .any(|(id, bound)| id == session_id && same_project_path(bound, project))
}

/// Compare project paths the way the sidebar groups them.
#[must_use]
pub(crate) fn same_project_path(left: &str, right: &str) -> bool {
    normalize_project_key(left) == normalize_project_key(right)
}

fn normalize_project_key(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        trimmed.replace('/', "\\").to_ascii_lowercase()
    } else {
        trimmed.to_owned()
    }
}

/// The folder bound to one session, if it has one.
#[must_use]
pub(crate) fn project_of_session<'a>(
    bindings: &'a [(String, String)],
    session_id: &str,
) -> Option<&'a str> {
    bindings
        .iter()
        .find(|(id, _)| id == session_id)
        .map(|(_, project)| project.as_str())
}

/// Newest session already bound to `project`. `sessions` is newest-first.
#[must_use]
pub(crate) fn newest_session_in_project<'a>(
    sessions: &'a [SessionSummary],
    bindings: &[(String, String)],
    project: &str,
) -> Option<&'a str> {
    sessions.iter().find_map(|session| {
        let bound = project_of_session(bindings, &session.session_id)?;
        same_project_path(bound, project).then_some(session.session_id.as_str())
    })
}
