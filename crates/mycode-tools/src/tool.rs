//! The `Tool` trait — builtin tools and Rust-side plugin tools implement it;
//! `ToolDyn` erases the associated argument type for storage in the
//! [`ToolRegistry`](crate::registry::ToolRegistry) (design doc
//! `02-tools-permissions.md` §1–2).
//!
//! Single-source schema: `schemars` derives one JSON Schema per tool's
//! `Args`, used both for the LLM tool spec (`ToolSpec::params_schema`) and
//! for runtime argument validation in [`ToolDyn::execute_dyn`].

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use mycode_core::message::{ContentBlock, TextBlock};
use mycode_core::tool::ToolSpec;

use crate::builtin::fs_io::FileAccess;
use crate::builtin::fs_search::SearchAccess;
use crate::ctx::ToolCtx;
use crate::stream::ToolStream;

/// The outcome of a tool execution as seen by the model and the UI.
///
/// Mirrors `ToolResultMessage` in `mycode-core`: `content` goes back to the
/// LLM, `details` is UI-only (structured diffs, byte counts, …) and never
/// enters model context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Content visible to the model.
    pub content: Vec<ContentBlock>,
    /// A tool-level failure reported *as data* (non-zero exit, failed
    /// match, …). Distinct from `Err(ToolError)`, which signals the
    /// dispatcher should synthesize an error result.
    pub is_error: bool,
    /// Structured information for the UI layer only; not sent to the LLM.
    pub details: Option<Value>,
}

impl ToolResult {
    /// A successful result with a single text block.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextBlock::new(content))],
            is_error: false,
            details: None,
        }
    }

    /// A result whose content reports a failure (`is_error: true`).
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::Text(TextBlock::new(message))],
            is_error: true,
            details: None,
        }
    }

    /// Attach UI-only details (builder style).
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

/// Failure modes of a tool invocation. The dispatcher (agent loop)
/// converts these into `is_error` tool results for the model rather than
/// crashing the loop (design doc `01-agent-core.md` §3).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {
    /// Arguments failed schema validation or are semantically invalid.
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    /// The tool ran but failed (missing file, I/O error, …).
    #[error("{0}")]
    Execution(String),
    /// A plugin trap fired (WASM trap or plugin panic).
    #[error("plugin trap: {0}")]
    PluginTrap(String),
}

impl From<std::io::Error> for ToolError {
    fn from(err: std::io::Error) -> Self {
        Self::Execution(err.to_string())
    }
}

/// How a tool may be scheduled relative to other tools
/// (design doc `02-tools-permissions.md` §2 capability markers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Concurrency {
    /// Only one instance may run at a time (e.g. `shell`, which mutates
    /// arbitrary state).
    Exclusive,
    /// Safe to run alongside other tool calls (default).
    #[default]
    Parallel,
}

/// A tool with typed arguments. Builtin tools and Rust-side plugins
/// implement this; the blanket [`ToolDyn`] impl type-erases it.
///
/// `Args` must additionally be `Send`: the returned boxed future (from
/// `async_trait` on the object-safe [`ToolDyn`]) captures the arguments.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// Typed, schema-derived arguments. The schemars-generated schema is
    /// the single source for both the tool spec and runtime validation.
    type Args: DeserializeOwned + JsonSchema + Send;
    /// Typed output payload, reserved for renderer integration.
    type Output: Serialize;

    /// Unique tool name (registry key; last registration wins).
    fn name(&self) -> &str;
    /// What the tool does, sent to the model in the tool spec.
    fn description(&self) -> &str;
    /// Optional usage hint injected into the system prompt (pi's
    /// `promptSnippet`). `None` by default.
    fn prompt_snippet(&self) -> Option<&str> {
        None
    }

    /// Scheduling capability marker; see [`Concurrency`]. Overridden by
    /// e.g. `shell` (Exclusive).
    fn concurrency(&self) -> Concurrency {
        Concurrency::Parallel
    }

    /// Whether the tool can modify the filesystem for scheduling decisions.
    /// Overridden by `write`/`edit`/`shell`.
    fn mutates_fs(&self) -> bool {
        false
    }

    /// Access mode for a retained local search root.
    ///
    /// Built-in `grep` returns content access and `find` metadata access.
    /// Plugin overrides stay off by default and are not forced through
    /// filesystem preflight.
    fn search_access(&self) -> Option<SearchAccess> {
        None
    }

    /// Whether dispatch should bind a local search root first.
    fn requires_search_preflight(&self) -> bool {
        self.search_access().is_some()
    }

    /// Access mode for a retained local file capability.
    ///
    /// Built-in `read` and `edit` return existing-content access and `write`
    /// returns existing-or-missing access. Plugin overrides stay off by
    /// default and are not forced through filesystem preflight.
    fn file_access(&self) -> Option<FileAccess> {
        None
    }

    /// Whether dispatch should bind a local file capability first.
    fn requires_file_preflight(&self) -> bool {
        self.file_access().is_some()
    }

    /// Execute the tool. Tools may stream progress through `out`; the returned
    /// result is the terminal outcome claimed by the dispatcher.
    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError>;
}

/// Object-safe, type-erased view of a [`Tool`] for registry storage and
/// dispatch with raw-JSON arguments.
#[async_trait]
pub trait ToolDyn: Send + Sync {
    /// The tool spec (name, description, JSON Schema of arguments) as
    /// sent to LLM providers.
    fn spec(&self) -> ToolSpec;
    /// Returns the optional compact usage hint through type erasure.
    fn prompt_snippet_dyn(&self) -> Option<&str> {
        None
    }
    /// Scheduling capability marker; see [`Concurrency`].
    fn concurrency(&self) -> Concurrency {
        Concurrency::Parallel
    }
    /// Whether the tool can modify the filesystem for scheduling decisions.
    fn mutates_fs(&self) -> bool {
        false
    }
    /// Access mode for a retained local search root.
    fn search_access(&self) -> Option<SearchAccess> {
        None
    }
    /// Whether dispatch should bind a local search root first.
    fn requires_search_preflight(&self) -> bool {
        self.search_access().is_some()
    }
    /// Access mode for a retained local file capability.
    fn file_access(&self) -> Option<FileAccess> {
        None
    }
    /// Whether dispatch should bind a local file capability first.
    fn requires_file_preflight(&self) -> bool {
        self.file_access().is_some()
    }
    /// Validate `args` against the tool's schema, deserialize, and
    /// execute. Wrong-shaped arguments fail with
    /// [`ToolError::InvalidArgs`].
    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError>;
}

/// Generate the JSON Schema of `A`'s arguments (schemars single source).
pub(crate) fn args_schema<A: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(A))
        .expect("schemars schemas always serialize to JSON")
}

/// Validate raw `args` against the schemars-generated schema of `A`.
pub(crate) fn validate_args<A: JsonSchema>(args: &Value) -> Result<(), ToolError> {
    let validator = jsonschema::validator_for(&args_schema::<A>())
        .map_err(|err| ToolError::Execution(format!("failed to compile argument schema: {err}")))?;
    let errors: Vec<String> = validator
        .iter_errors(args)
        .map(|err| format!("{err} (at {})", err.instance_path()))
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ToolError::InvalidArgs(errors.join("; ")))
    }
}

/// Blanket type erasure: every [`Tool`] is a [`ToolDyn`]. Deserializes
/// `Value → Args` after validating it against the schemars-generated
/// schema, so wrong-shaped arguments are rejected with
/// [`ToolError::InvalidArgs`] before the tool runs.

#[async_trait]
impl<T: Tool> ToolDyn for T {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_owned(),
            description: self.description().to_owned(),
            params_schema: args_schema::<T::Args>(),
        }
    }

    fn prompt_snippet_dyn(&self) -> Option<&str> {
        Tool::prompt_snippet(self)
    }

    // Capability markers are declared on `Tool` and forwarded here so
    // there is exactly one place for tools to override them.
    fn concurrency(&self) -> Concurrency {
        Tool::concurrency(self)
    }

    fn mutates_fs(&self) -> bool {
        Tool::mutates_fs(self)
    }

    fn search_access(&self) -> Option<SearchAccess> {
        Tool::search_access(self)
    }

    fn requires_search_preflight(&self) -> bool {
        Tool::requires_search_preflight(self)
    }

    fn file_access(&self) -> Option<FileAccess> {
        Tool::file_access(self)
    }

    fn requires_file_preflight(&self) -> bool {
        Tool::requires_file_preflight(self)
    }

    async fn execute_dyn(
        &self,
        args: Value,
        ctx: &ToolCtx,
        out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        validate_args::<T::Args>(&args)?;
        let typed =
            serde_json::from_value(args).map_err(|err| ToolError::InvalidArgs(err.to_string()))?;
        self.execute(typed, ctx, out).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Minimal tool used to exercise the trait surface and blanket impl.
    struct EchoTool;

    #[derive(Debug, Deserialize, JsonSchema)]
    struct EchoArgs {
        /// Text to echo back.
        text: String,
        /// Optional repeat count.
        repeat: Option<usize>,
    }

    #[async_trait]
    impl Tool for EchoTool {
        type Args = EchoArgs;
        type Output = ();

        fn name(&self) -> &str {
            "echo"
        }

        fn description(&self) -> &str {
            "Echo text back (test fixture)."
        }

        fn prompt_snippet(&self) -> Option<&str> {
            Some("echo: test fixture tool")
        }

        async fn execute(
            &self,
            args: Self::Args,
            _ctx: &ToolCtx,
            _out: &mut ToolStream,
        ) -> Result<ToolResult, ToolError> {
            let n = args.repeat.unwrap_or(1);
            Ok(ToolResult::text(args.text.repeat(n)))
        }
    }

    fn ctx() -> ToolCtx {
        ToolCtx::new(".")
    }

    #[tokio::test]
    async fn blanket_impl_validates_against_schema() {
        let tool = EchoTool;
        let dyn_tool: &dyn ToolDyn = &tool;

        // Correct args dispatch to the typed implementation.
        let result = dyn_tool
            .execute_dyn(
                json!({"text": "hi", "repeat": 3}),
                &ctx(),
                &mut ToolStream::closed(),
            )
            .await
            .expect("valid args must dispatch");
        assert_eq!(result.content, vec![ContentBlock::Text("hihihi".into())]);
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn blanket_impl_rejects_wrong_shaped_args() {
        let tool = EchoTool;
        let dyn_tool: &dyn ToolDyn = &tool;

        // Missing required field.
        let err = dyn_tool
            .execute_dyn(json!({ "repeat": 2 }), &ctx(), &mut ToolStream::closed())
            .await
            .expect_err("missing `text` must be rejected");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
        assert!(err.to_string().contains("required"), "{err}");

        // Wrong type.
        let err = dyn_tool
            .execute_dyn(json!({ "text": 42 }), &ctx(), &mut ToolStream::closed())
            .await
            .expect_err("non-string `text` must be rejected");
        assert!(matches!(err, ToolError::InvalidArgs(_)));
        assert!(err.to_string().contains("type"), "{err}");
    }

    #[test]
    fn search_preflight_defaults_off() {
        let tool = EchoTool;
        let dyn_tool: &dyn ToolDyn = &tool;
        assert!(!Tool::requires_search_preflight(&tool));
        assert!(!dyn_tool.requires_search_preflight());
        assert!(!Tool::requires_file_preflight(&tool));
        assert!(!dyn_tool.requires_file_preflight());
    }

    #[tokio::test]
    async fn spec_carries_schemars_schema() {
        let tool = EchoTool;
        let dyn_tool: &dyn ToolDyn = &tool;
        let spec = dyn_tool.spec();

        assert_eq!(spec.name, "echo");
        assert_eq!(spec.description, "Echo text back (test fixture).");
        assert_eq!(spec.params_schema["required"], json!(["text"]));
        assert_eq!(spec.params_schema["properties"]["text"]["type"], "string");
        // Doc comments flow into schema descriptions (single source).
        assert_eq!(
            spec.params_schema["properties"]["text"]["description"],
            "Text to echo back."
        );
    }

    #[test]
    fn tool_result_builders() {
        let ok = ToolResult::text("done");
        assert!(!ok.is_error);
        assert_eq!(ok.content, vec![ContentBlock::Text("done".into())]);
        assert!(ok.details.is_none());

        let err = ToolResult::error("boom").with_details(json!({"code": 1}));
        assert!(err.is_error);
        assert_eq!(err.details, Some(json!({"code": 1})));
    }

    #[test]
    fn tool_error_variants_and_io_conversion() {
        assert_eq!(
            ToolError::InvalidArgs("bad path".into()).to_string(),
            "invalid arguments: bad path"
        );
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "nope");
        assert!(matches!(ToolError::from(io_err), ToolError::Execution(_)));
    }
}
