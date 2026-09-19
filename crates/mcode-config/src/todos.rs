//! Strict per-session todo documents: stable ids, dependency graph, CAS.
//!
//! `todos.json` under the session's durable area holds one task list. Task
//! ids are `todo-` + 16 hex chars, stable across revisions. Writes go
//! through the same owned-file transaction and revision CAS as settings;
//! the dependency graph validates closed references and acyclicity.

use serde::{Deserialize, Serialize};

use crate::authority::AuthorityRevision;
use crate::secure_fs::owned_file::{locked_update_owned_file, read_owned_file};
use crate::{ConfigError, ConfigErrorKind, HomeLayout};

/// Exact todo document path (relative to the session directory root caller).
pub const TODO_FORMAT_VERSION: u32 = 1;
/// Exact todo document kind.
pub const TODO_KIND: &str = "mcode-todo";
/// Maximum tasks in one document.
pub const MAX_TODO_TASKS: usize = 128;
/// Maximum content characters per task.
pub const MAX_TODO_CONTENT_CHARS: usize = 512;
/// Maximum dependencies per task.
pub const MAX_TODO_DEPS: usize = 16;

/// Task status vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    /// Not started.
    Pending,
    /// In progress; at most one per document.
    InProgress,
    /// Done.
    Completed,
}

/// One task with stable identity and dependencies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TodoTask {
    /// Stable `todo-` identity.
    pub id: String,
    /// What to do.
    pub content: String,
    /// Lifecycle status.
    pub status: TodoStatus,
    /// Ids of tasks that must complete first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

/// The complete task list document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TodoDocument {
    /// Ordered task list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TodoTask>,
}

impl TodoDocument {
    /// Validates ids, references, and the dependency graph.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigErrorKind::AuthorityValidation`] for any violation.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = || ConfigError::authority_rejection();
        if self.tasks.len() > MAX_TODO_TASKS {
            return Err(invalid());
        }
        let mut in_progress = 0usize;
        for (index, task) in self.tasks.iter().enumerate() {
            if !is_todo_id(&task.id)
                || task.content.is_empty()
                || task.content.chars().count() > MAX_TODO_CONTENT_CHARS
                || task.blocked_by.len() > MAX_TODO_DEPS
                || task.blocked_by.contains(&task.id)
            {
                return Err(invalid());
            }
            if matches!(task.status, TodoStatus::InProgress) {
                in_progress += 1;
            }
            for dep in &task.blocked_by {
                if !self.tasks[..index].iter().any(|other| other.id == *dep)
                    && !self.tasks[index + 1..].iter().any(|other| other.id == *dep)
                {
                    return Err(invalid());
                }
            }
            if self.tasks[..index].iter().any(|other| other.id == task.id) {
                return Err(invalid());
            }
        }
        if in_progress > 1 {
            return Err(invalid());
        }
        if has_cycle(&self.tasks) {
            return Err(invalid());
        }
        Ok(())
    }

    /// Serializes the document for the durable Task event payload.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when serialization fails.
    pub fn to_payload(&self) -> Result<Vec<u8>, ConfigError> {
        serde_json::to_vec(self).map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))
    }

    /// Deserializes a durable Task payload and validates it.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] for malformed or invalid payloads.
    pub fn from_payload(bytes: &[u8]) -> Result<Self, ConfigError> {
        let document: TodoDocument =
            serde_json::from_slice(bytes).map_err(|_| ConfigError::authority_rejection())?;
        document.validate()?;
        Ok(document)
    }
}

/// Generates a fresh stable task id.
#[must_use]
pub fn new_todo_id() -> Option<String> {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).ok()?;
    let mut id = String::with_capacity(5 + 16);
    id.push_str("todo-");
    for byte in random {
        id.push_str(&format!("{byte:02x}"));
    }
    Some(id)
}

fn is_todo_id(value: &str) -> bool {
    value.len() == 5 + 16
        && value.starts_with("todo-")
        && value[5..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn has_cycle(tasks: &[TodoTask]) -> bool {
    let ids: Vec<&str> = tasks.iter().map(|task| task.id.as_str()).collect();
    let mut color = vec![0_u8; tasks.len()];
    for start in 0..tasks.len() {
        if color[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        color[start] = 1;
        while let Some((node, next)) = stack.pop() {
            if next < tasks[node].blocked_by.len() {
                stack.push((node, next + 1));
                let dep = &tasks[node].blocked_by[next];
                if let Some(dep_index) = ids.iter().position(|id| *id == dep.as_str()) {
                    match color[dep_index] {
                        0 => {
                            color[dep_index] = 1;
                            stack.push((dep_index, 0));
                        }
                        1 => return true,
                        _ => {}
                    }
                }
            } else {
                color[node] = 2;
            }
        }
    }
    false
}

fn todo_path(session_id: &str) -> Result<String, ConfigError> {
    if session_id.is_empty() || session_id.contains(['/', '\\', '\0']) {
        return Err(ConfigError::authority_rejection());
    }
    Ok(format!("workspace/{session_id}/todos.json"))
}

/// Reads one session's todo document; a missing file yields an empty list.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or strict validation.
pub fn read_todo_document(
    home: &HomeLayout,
    session_id: &str,
) -> Result<TodoDocument, ConfigError> {
    let path = todo_path(session_id)?;
    let bytes = read_owned_file(home, &path, crate::MAX_SETTINGS_BYTES)?;
    let Some(bytes) = bytes else {
        return Ok(TodoDocument::default());
    };
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    #[allow(dead_code)]
    struct Wire {
        format_version: u32,
        kind: String,
        revision: u64,
        #[serde(default)]
        tasks: Vec<TodoTask>,
    }
    let wire: Wire =
        serde_json::from_slice(&bytes).map_err(|_| ConfigError::authority_rejection())?;
    if wire.format_version != TODO_FORMAT_VERSION || wire.kind != TODO_KIND {
        return Err(ConfigError::authority_rejection());
    }
    let document = TodoDocument { tasks: wire.tasks };
    document.validate()?;
    Ok(document)
}

/// Reads the current todo revision without validating the task list.
///
/// # Errors
///
/// Returns [`ConfigError`] for owned-path security or header corruption.
pub fn read_todo_revision(
    home: &HomeLayout,
    session_id: &str,
) -> Result<AuthorityRevision, ConfigError> {
    let path = todo_path(session_id)?;
    let bytes = read_owned_file(home, &path, crate::MAX_SETTINGS_BYTES)?;
    let Some(bytes) = bytes else {
        return Ok(AuthorityRevision::ABSENT);
    };
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    #[allow(dead_code)]
    struct Header {
        format_version: u32,
        kind: String,
        revision: u64,
        #[serde(default)]
        tasks: Vec<serde_json::Value>,
    }
    let header: Header =
        serde_json::from_slice(&bytes).map_err(|_| ConfigError::authority_rejection())?;
    if header.format_version != TODO_FORMAT_VERSION || header.kind != TODO_KIND {
        return Err(ConfigError::authority_rejection());
    }
    AuthorityRevision::new(header.revision)
}

/// Replaces one session's todo document under revision CAS.
///
/// # Errors
///
/// Returns [`ConfigErrorKind::RevisionConflict`] for stale expectations and
/// [`ConfigError`] for validation or transaction failures.
pub fn replace_todo_document(
    home: &HomeLayout,
    session_id: &str,
    expected_revision: AuthorityRevision,
    document: &TodoDocument,
) -> Result<AuthorityRevision, ConfigError> {
    document.validate()?;
    let path = todo_path(session_id)?;
    let mut published = None;
    locked_update_owned_file(home, &path, crate::MAX_SETTINGS_BYTES, |current| {
        let current_revision = match current {
            Some(bytes) => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                #[allow(dead_code)]
                struct Header {
                    format_version: u32,
                    kind: String,
                    revision: u64,
                    #[serde(default)]
                    tasks: Vec<serde_json::Value>,
                }
                let header: Header = serde_json::from_slice(bytes)
                    .map_err(|_| ConfigError::authority_rejection())?;
                if header.format_version != TODO_FORMAT_VERSION || header.kind != TODO_KIND {
                    return Err(ConfigError::authority_rejection());
                }
                AuthorityRevision::new(header.revision)?
            }
            None => AuthorityRevision::ABSENT,
        };
        if current_revision != expected_revision {
            return Err(ConfigError::new(ConfigErrorKind::RevisionConflict));
        }
        let revision = current_revision.checked_next()?;
        let mut wire = serde_json::Map::new();
        wire.insert("formatVersion".into(), TODO_FORMAT_VERSION.into());
        wire.insert("kind".into(), TODO_KIND.into());
        wire.insert("revision".into(), revision.get().into());
        wire.insert(
            "tasks".into(),
            serde_json::to_value(&document.tasks)
                .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?,
        );
        let mut bytes = serde_json::to_vec_pretty(&wire)
            .map_err(|_| ConfigError::new(ConfigErrorKind::Serialization))?;
        bytes.push(b'\n');
        published = Some(revision);
        Ok(bytes)
    })?;
    published.ok_or_else(|| ConfigError::new(ConfigErrorKind::Serialization))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    fn task(id: usize, status: TodoStatus) -> TodoTask {
        TodoTask {
            id: format!("todo-{id:016}"),
            content: format!("task {id}"),
            status,
            blocked_by: Vec::new(),
        }
    }

    #[test]
    fn cas_roundtrip_and_validation() {
        let (_parent, home) = layout();
        let document = TodoDocument {
            tasks: vec![
                task(1, TodoStatus::InProgress),
                task(2, TodoStatus::Pending),
            ],
        };
        let first = replace_todo_document(&home, "ses1-x", AuthorityRevision::ABSENT, &document)
            .expect("publish");
        assert_eq!(first.get(), 1);

        let read = read_todo_document(&home, "ses1-x").expect("read");
        assert_eq!(read, document);

        let stale = replace_todo_document(&home, "ses1-x", AuthorityRevision::ABSENT, &document);
        assert_eq!(
            stale.expect_err("stale").kind(),
            ConfigErrorKind::RevisionConflict
        );

        let mut blocked = document.clone();
        blocked.tasks[1].blocked_by = vec![blocked.tasks[0].id.clone()];
        replace_todo_document(&home, "ses1-x", first, &blocked).expect("publish v2");
        let read = read_todo_document(&home, "ses1-x").expect("read v2");
        assert_eq!(read.tasks[1].blocked_by, vec![read.tasks[0].id.clone()]);
    }

    #[test]
    fn graphs_cycles_duplicate_ids_and_double_in_progress_fail() {
        let mut a = task(1, TodoStatus::Pending);
        let mut b = task(2, TodoStatus::Pending);
        a.blocked_by = vec![b.id.clone()];
        b.blocked_by = vec![a.id.clone()];
        assert!(
            TodoDocument { tasks: vec![a, b] }.validate().is_err(),
            "cycle"
        );

        let mut doc = TodoDocument {
            tasks: vec![task(1, TodoStatus::Pending), task(2, TodoStatus::Pending)],
        };
        doc.tasks[1].id = "todo-000000000000000z".to_owned();
        assert!(doc.validate().is_err(), "bad id fails closed");

        assert!(
            TodoDocument {
                tasks: vec![
                    task(1, TodoStatus::InProgress),
                    task(2, TodoStatus::InProgress)
                ],
            }
            .validate()
            .is_err(),
            "at most one in progress"
        );

        let mut dangling = task(3, TodoStatus::Pending);
        dangling.blocked_by = vec!["todo-ffffffffffffffff".to_owned()];
        assert!(
            TodoDocument {
                tasks: vec![dangling]
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn payload_roundtrip() {
        let document = TodoDocument {
            tasks: vec![task(9, TodoStatus::Completed)],
        };
        let payload = document.to_payload().expect("payload");
        assert_eq!(
            TodoDocument::from_payload(&payload).expect("back"),
            document
        );
    }
}
