//! The `todo_write` tool: durable task-list updates.
//!
//! The tool validates the incoming task list, hands it to the host-supplied
//! [`TodoStore`] (which persists it under CAS and appends a durable Task
//! event), and reports the stored state back to the model.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Maximum tasks accepted in one write.
pub const MAX_WRITE_TASKS: usize = 128;

/// Host side of todo persistence.
#[async_trait]
pub trait TodoStore: Send + Sync + 'static {
    /// Persists the full task list and returns the stored rendering.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Execution`] when persistence fails.
    async fn store(&self, tasks: &[TodoWireTask]) -> Result<String, ToolError>;
}

/// One task as written by the model (ids are minted host-side when absent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TodoWireTask {
    /// Stable id; omit to mint a fresh one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What to do.
    pub content: String,
    /// `pending`, `in_progress`, or `completed`.
    pub status: String,
    /// Ids of tasks that must complete first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
}

/// The built-in `todo_write` tool.
pub struct TodoWriteTool {
    store: Arc<dyn TodoStore>,
}

impl TodoWriteTool {
    /// Binds one persistence backend.
    pub fn new(store: Arc<dyn TodoStore>) -> Self {
        Self { store }
    }
}

/// Wire shape of the tool arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TodoWriteArgs {
    /// The complete task list; replaces the previous one.
    pub tasks: Vec<TodoWireTask>,
}

#[async_trait]
impl Tool for TodoWriteTool {
    type Args = TodoWriteArgs;
    type Output = ();

    fn name(&self) -> &str {
        "todo_write"
    }

    fn description(&self) -> &str {
        "Replace the durable task list with a complete, validated plan. \
         Keep at most one task in_progress; list dependencies with blockedBy."
    }

    fn concurrency(&self) -> crate::tool::Concurrency {
        crate::tool::Concurrency::Exclusive
    }

    fn mutates_fs(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        if args.tasks.len() > MAX_WRITE_TASKS {
            return Err(ToolError::InvalidArgs(format!(
                "at most {MAX_WRITE_TASKS} tasks"
            )));
        }
        for task in &args.tasks {
            if task.content.trim().is_empty() {
                return Err(ToolError::InvalidArgs("task content is required".into()));
            }
            if !matches!(
                task.status.as_str(),
                "pending" | "in_progress" | "completed"
            ) {
                return Err(ToolError::InvalidArgs(format!(
                    "unknown status: {}",
                    task.status
                )));
            }
        }
        let rendered = self.store.store(&args.tasks).await?;
        Ok(ToolResult::text(rendered))
    }
}
