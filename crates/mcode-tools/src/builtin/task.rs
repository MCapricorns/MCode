//! The `task` tool: delegate one scoped unit of work to a subagent.
//!
//! The tool serializes the delegation through the host-supplied
//! [`TaskHost`], forwards short progress lines onto its own tool stream,
//! and returns the subagent's final answer as the tool result. The channel
//! seam keeps the tool free of provider and runtime dependencies; hosts run
//! the nested agent, tests replay canned answers.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ctx::ToolCtx;
use crate::stream::ToolStream;
use crate::tool::{Tool, ToolError, ToolResult};

/// Maximum characters accepted for the delegated prompt.
pub const MAX_TASK_PROMPT_CHARS: usize = 16_000;
/// Maximum characters of the subagent answer kept in the tool result.
pub const MAX_TASK_ANSWER_CHARS: usize = 24_000;

/// One delegated unit of work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentRequest {
    /// The full task brief handed to the subagent.
    pub prompt: String,
    /// Short human-facing label for progress lines.
    pub description: String,
    /// Whether the subagent runs in a disposable git worktree lease.
    pub worktree: bool,
}

/// Host side of the delegation.
#[async_trait]
pub trait TaskHost: Send + Sync + 'static {
    /// Runs one subagent to completion and returns its final answer.
    ///
    /// Implementations must honor `cancel` and report short status lines
    /// through `progress`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Execution`] when the subagent failed, timed out,
    /// or was cancelled.
    async fn run_subagent(
        &self,
        request: SubagentRequest,
        progress: &ToolStream,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<String, ToolError>;
}

/// The built-in `task` tool.
pub struct TaskTool {
    host: Arc<dyn TaskHost>,
}

impl TaskTool {
    /// Binds one delegation host.
    pub fn new(host: Arc<dyn TaskHost>) -> Self {
        Self { host }
    }
}

/// Wire shape of the tool arguments.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct TaskArgs {
    /// Complete, self-contained instructions for the subagent. Include the
    /// goal, relevant paths, and the expected form of the answer.
    pub prompt: String,
    /// One-line label shown while the subagent runs.
    #[serde(default)]
    pub description: Option<String>,
    /// Run in a disposable git worktree lease instead of the shared tree.
    #[serde(default)]
    pub worktree: bool,
}

#[async_trait]
impl Tool for TaskTool {
    type Args = TaskArgs;
    type Output = ();

    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Delegate one scoped unit of work to a subagent that runs the same \
         tools and returns its final answer. Use for independent research, \
         audits, or multi-file questions you do not need to drive yourself."
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some(
            "task: delegate self-contained work with a complete brief; the \
             subagent cannot ask you questions, so include every path and \
             constraint it needs.",
        )
    }

    fn concurrency(&self) -> crate::tool::Concurrency {
        crate::tool::Concurrency::Parallel
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let prompt = args.prompt.trim().to_owned();
        if prompt.is_empty() {
            return Err(ToolError::InvalidArgs("prompt is required".into()));
        }
        if prompt.chars().count() > MAX_TASK_PROMPT_CHARS {
            return Err(ToolError::InvalidArgs(format!(
                "prompt exceeds {MAX_TASK_PROMPT_CHARS} characters"
            )));
        }
        let description = args.description.unwrap_or_else(|| brief_label(&prompt));
        let request = SubagentRequest {
            prompt,
            description,
            worktree: args.worktree,
        };
        let answer = self.host.run_subagent(request, out, &ctx.cancel).await?;
        let answer = answer
            .chars()
            .take(MAX_TASK_ANSWER_CHARS)
            .collect::<String>();
        Ok(ToolResult::text(answer))
    }
}

/// Short progress label derived from a prompt's first line.
fn brief_label(prompt: &str) -> String {
    let first = prompt.lines().next().unwrap_or("").trim();
    let chars: Vec<char> = first.chars().collect();
    if chars.len() <= 60 {
        first.to_owned()
    } else {
        chars[..60].iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcode_core::message::ContentBlock;

    struct EchoHost;

    #[async_trait]
    impl TaskHost for EchoHost {
        async fn run_subagent(
            &self,
            request: SubagentRequest,
            _progress: &ToolStream,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<String, ToolError> {
            Ok(format!("done: {}", request.description))
        }
    }

    #[tokio::test]
    async fn runs_host_and_returns_answer() {
        let tool = TaskTool::new(Arc::new(EchoHost));
        let result = tool
            .execute(
                TaskArgs {
                    prompt: "audit the parser\nand report".to_owned(),
                    description: None,
                    worktree: false,
                },
                &ToolCtx::new("."),
                &mut ToolStream::channel().0,
            )
            .await
            .expect("result");
        assert!(!result.is_error);
        let text = match &result.content[0] {
            ContentBlock::Text(text) => text.text.clone(),
            other => panic!("unexpected content block: {other:?}"),
        };
        assert_eq!(text, "done: audit the parser");
    }

    #[tokio::test]
    async fn empty_prompt_is_rejected() {
        let tool = TaskTool::new(Arc::new(EchoHost));
        let error = tool
            .execute(
                TaskArgs {
                    prompt: "   ".to_owned(),
                    description: None,
                    worktree: false,
                },
                &ToolCtx::new("."),
                &mut ToolStream::channel().0,
            )
            .await
            .expect_err("rejected");
        assert!(matches!(error, ToolError::InvalidArgs(_)));
    }
}
