//! Project-centric grouping: path comparison, session bindings, and the
//! sidebar's grouped-session projection.

use mycode_app::SessionSummary;

use super::state::WorkspaceState;

/// Sidebar grouping of sessions relative to the active project filter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupedSessions {
    /// Sessions bound to the active project.
    pub current: Vec<SessionSummary>,
    /// Sessions with no project binding.
    pub unbound: Vec<SessionSummary>,
    /// Sessions bound to some other project, grouped by that path.
    pub others: Vec<(String, Vec<SessionSummary>)>,
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

/// Groups sessions for the project-centric sidebar.
#[must_use]
pub fn group_sessions(
    sessions: &[SessionSummary],
    bindings: &[(String, String)],
    active_project: Option<&str>,
) -> GroupedSessions {
    let mut grouped = GroupedSessions::default();
    for session in sessions {
        let project = bindings
            .iter()
            .find(|(id, _)| id == &session.session_id)
            .map(|(_, project)| project.as_str());
        match project {
            Some(project)
                if active_project.is_some_and(|active| same_project_path(active, project)) =>
            {
                grouped.current.push(session.clone());
            }
            Some(project) => match grouped
                .others
                .iter_mut()
                .find(|(key, _)| same_project_path(key, project))
            {
                Some((_, rows)) => rows.push(session.clone()),
                None => grouped
                    .others
                    .push((project.to_owned(), vec![session.clone()])),
            },
            None => grouped.unbound.push(session.clone()),
        }
    }
    grouped
}
