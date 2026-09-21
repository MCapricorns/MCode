//! Builtin tools — the trusted reference implementation of the
//! [`crate::tool::Tool`] trait. File discovery and content search stay in-process and never spawn
//! external `fd` or `rg` executables.

pub mod ask;
pub mod edit;
pub mod exec;
pub mod find;
pub(crate) mod fs_io;
pub(crate) mod fs_search;
pub mod grep;
#[cfg(windows)]
mod powershell;
pub(crate) mod process;
pub mod read;
pub mod shell;
pub mod task;
pub mod todo;
pub mod web;
pub mod write;

pub use ask::{AskAnswer, AskChannel, AskQuestion, AskTool, user_dismissed};
pub use edit::EditTool;
pub use exec::ExecTool;
pub use find::FindTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use shell::ShellTool;
pub use task::{SubagentRequest, TaskHost, TaskTool};
pub use todo::{TodoStore, TodoWireTask, TodoWriteTool};
pub use web::{FetchContentTool, WebHit, WebHost, WebPage, WebSearchTool};
pub use write::WriteTool;

use std::sync::Arc;

use crate::registry::ToolRegistry;
use crate::tool::ToolDyn;

/// All builtin tools as type-erased, registry-ready handles.
pub fn builtin_tools() -> Vec<Arc<dyn ToolDyn>> {
    vec![
        Arc::new(ReadTool),
        Arc::new(WriteTool),
        Arc::new(EditTool),
        Arc::new(ShellTool::default()),
        Arc::new(ExecTool::default()),
        Arc::new(GrepTool),
        Arc::new(FindTool),
    ]
}

/// Register all builtin tools into a registry.
pub fn register_builtins(registry: &ToolRegistry) {
    for tool in builtin_tools() {
        registry.register(tool);
    }
}

/// Byte-cap text truncation that respects char boundaries.
///
/// Returns `(text, truncated)`; callers append their own notice so the
/// model knows how to fetch the rest (offset/limit, narrower glob, …).
pub(crate) fn truncate_bytes(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_owned(), false);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_bytes_respects_limits_and_boundaries() {
        // Short text passes through untouched.
        assert_eq!(truncate_bytes("hello", 10), ("hello".to_owned(), false));

        // ASCII truncation at the cap.
        let (cut, truncated) = truncate_bytes(&"x".repeat(100), 50);
        assert!(truncated);
        assert_eq!(cut.len(), 50);

        // Multi-byte chars are not split (cap lands mid-char).
        let multibyte = "é".repeat(50); // 2 bytes each = 100 bytes
        let (cut, truncated) = truncate_bytes(&multibyte, 51);
        assert!(truncated);
        assert_eq!(cut.len(), 50); // backed off to the previous char boundary
        assert!(cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn builtin_tool_names_are_the_canonical_set() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        assert_eq!(
            registry.names(),
            vec!["edit", "exec", "find", "grep", "read", "shell", "write"]
        );
    }

    #[test]
    fn builtin_capability_markers() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);

        assert!(registry.get("bash").is_none());
        let shell = registry.get("shell").unwrap();
        assert_eq!(shell.concurrency(), crate::tool::Concurrency::Exclusive);
        assert!(shell.mutates_fs());

        let exec = registry.get("exec").unwrap();
        assert_eq!(exec.concurrency(), crate::tool::Concurrency::Exclusive);
        assert!(exec.mutates_fs());
        assert!(!exec.requires_file_preflight());
        assert!(!exec.requires_search_preflight());

        assert!(registry.get("write").unwrap().mutates_fs());
        assert!(registry.get("edit").unwrap().mutates_fs());
        assert!(registry.get("edit").unwrap().requires_file_preflight());

        assert!(!registry.get("read").unwrap().mutates_fs());
        assert_eq!(
            registry.get("read").unwrap().concurrency(),
            crate::tool::Concurrency::Parallel
        );
        assert!(registry.get("read").unwrap().requires_file_preflight());
        assert!(!registry.get("read").unwrap().requires_search_preflight());
        assert!(registry.get("write").unwrap().requires_file_preflight());
        assert!(!registry.get("grep").unwrap().mutates_fs());
        assert!(registry.get("grep").unwrap().requires_search_preflight());

        let find = registry.get("find").unwrap();
        assert!(!find.mutates_fs());
        assert_eq!(find.concurrency(), crate::tool::Concurrency::Parallel);
        assert!(find.requires_search_preflight());
    }

    #[test]
    fn builtin_specs_have_json_schemas() {
        let registry = ToolRegistry::new();
        register_builtins(&registry);
        for spec in registry.specs() {
            assert_eq!(spec.params_schema["type"], "object", "{spec:?}");
            assert!(
                spec.params_schema["properties"].is_object(),
                "{spec:?} lacks properties"
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared helpers for builtin-tool tests.

    use crate::ctx::ToolCtx;
    use crate::tool::{Tool, ToolDyn, ToolError, ToolResult};

    /// Build a [`ToolCtx`] rooted at `cwd`.
    pub(crate) fn ctx_at(cwd: &std::path::Path) -> ToolCtx {
        ToolCtx::new(cwd)
    }

    /// Execute a tool through its [`ToolDyn`] blanket impl (schema
    /// validation + typed dispatch), like the real dispatcher would.
    pub(crate) async fn run_dyn<T: Tool>(
        tool: &T,
        args: serde_json::Value,
        ctx: &ToolCtx,
    ) -> Result<ToolResult, ToolError> {
        let dyn_tool: &dyn ToolDyn = tool;
        let mut stream = crate::stream::ToolStream::closed();
        dyn_tool.execute_dyn(args, ctx, &mut stream).await
    }

    /// Unwraps a search `Result` in tests.
    pub(crate) fn unwrap_tool(result: Result<ToolResult, ToolError>) -> ToolResult {
        result.unwrap_or_else(|error| panic!("search failed: {error}"))
    }

    /// The single text block of a result (panics if content is not one
    /// text block — for test assertions only).
    pub(crate) fn text_of(result: &ToolResult) -> &str {
        match result.content.as_slice() {
            [mycode_core::message::ContentBlock::Text(text)] => &text.text,
            other => panic!("expected single text block, got {other:?}"),
        }
    }
}
