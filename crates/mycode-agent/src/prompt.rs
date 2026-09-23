//! Builds the default system prompt from the active tool registry.

use std::fmt::Write;

use mycode_tools::ToolRegistry;

const IDENTITY: &str =
    "You are MYCode Agent. Complete the task with the tools listed below. Do not invent tools.";

/// Tool-use contract. Only names tools this process can actually call.
const TOOL_CALLING: &str = "\
<tool_calling>
- Independent calls in one response run together. Do not wait between them.
- Read existing content before changing it.
- Prefer `read`, `write`, `edit`, `find`, and `grep` for files and search.
- Use `exec` for one program with explicit arguments and no shell parsing.
- Use `shell` only for pipelines, redirection, expansion, or a compound script. Never use it to talk to the user.
</tool_calling>";

/// Builds the compact default prompt from the currently registered tools.
pub fn build_system_prompt(tools: &ToolRegistry) -> String {
    let mut prompt = String::from(IDENTITY);
    prompt.push_str("\n\nAvailable tools:");

    let entries = tools.prompt_entries();
    if entries.is_empty() {
        prompt.push_str("\n(none)");
    } else {
        for (name, snippet) in entries {
            write!(prompt, "\n- {name}").expect("writing to a String cannot fail");
            let snippet = snippet
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty());
            if let Some(detail) = snippet {
                let detail = detail
                    .strip_prefix(&name)
                    .and_then(|text| text.strip_prefix(':'))
                    .map(str::trim)
                    .unwrap_or(detail);
                write!(prompt, ": {detail}").expect("writing to a String cannot fail");
            }
        }
    }

    prompt.push_str("\n\n");
    prompt.push_str(TOOL_CALLING);
    prompt
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use mycode_tools::{Tool, ToolCtx, ToolError, ToolResult, ToolStream, register_builtins};
    use schemars::JsonSchema;
    use serde::Deserialize;

    use super::*;

    struct StubTool {
        name: &'static str,
        snippet: Option<&'static str>,
    }

    #[derive(Deserialize, JsonSchema)]
    struct NoArgs {}

    #[async_trait]
    impl Tool for StubTool {
        type Args = NoArgs;
        type Output = ();

        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Synthetic prompt test tool."
        }

        fn prompt_snippet(&self) -> Option<&str> {
            self.snippet
        }

        async fn execute(
            &self,
            _args: Self::Args,
            _ctx: &ToolCtx,
            _out: &mut ToolStream,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::text("unused"))
        }
    }

    fn registry(include_synthetic: bool) -> ToolRegistry {
        let tools = ToolRegistry::new();
        tools.register(Arc::new(StubTool {
            name: "zeta",
            snippet: None,
        }));
        tools.register(Arc::new(StubTool {
            name: "alpha",
            snippet: Some("alpha: inspect alpha inputs."),
        }));
        if include_synthetic {
            tools.register(Arc::new(StubTool {
                name: "synthetic",
                snippet: Some("synthetic: exercise registry changes."),
            }));
        }
        tools
    }

    #[test]
    fn prompt_contract_is_exact_and_deterministic() {
        let prompt = build_system_prompt(&registry(false));
        assert_eq!(
            prompt,
            "You are MYCode Agent. Complete the task with the tools listed below. Do not invent tools.\n\n\
Available tools:\n\
- alpha: inspect alpha inputs.\n\
- zeta\n\n\
<tool_calling>\n\
- Independent calls in one response run together. Do not wait between them.\n\
- Read existing content before changing it.\n\
- Prefer `read`, `write`, `edit`, `find`, and `grep` for files and search.\n\
- Use `exec` for one program with explicit arguments and no shell parsing.\n\
- Use `shell` only for pipelines, redirection, expansion, or a compound script. Never use it to talk to the user.\n\
</tool_calling>"
        );
        assert_eq!(prompt.lines().next(), Some(IDENTITY));
        assert_eq!(build_system_prompt(&registry(false)), prompt);
    }

    #[test]
    fn prompt_list_tracks_synthetic_tool_addition_and_removal() {
        let without = build_system_prompt(&registry(false));
        let with = build_system_prompt(&registry(true));
        let without_list = without
            .split_once("Available tools:\n")
            .unwrap()
            .1
            .split_once("\n\n<tool_calling>")
            .unwrap()
            .0;
        let with_list = with
            .split_once("Available tools:\n")
            .unwrap()
            .1
            .split_once("\n\n<tool_calling>")
            .unwrap()
            .0;

        assert_eq!(without_list, "- alpha: inspect alpha inputs.\n- zeta");
        assert_eq!(
            with_list,
            "- alpha: inspect alpha inputs.\n- synthetic: exercise registry changes.\n- zeta"
        );
        assert!(!without_list.contains("synthetic"));
        assert!(with_list.contains("synthetic"));
    }

    #[test]
    fn builtin_list_names_exactly_match_the_registry() {
        let tools = ToolRegistry::new();
        register_builtins(&tools);
        let prompt = build_system_prompt(&tools);
        let list = prompt
            .split_once("Available tools:\n")
            .unwrap()
            .1
            .split_once("\n\n<tool_calling>")
            .unwrap()
            .0;
        let listed_names: Vec<&str> = list
            .lines()
            .map(|line| line.trim_start_matches("- ").split(':').next().unwrap())
            .collect();

        assert_eq!(
            listed_names,
            ["edit", "exec", "find", "grep", "read", "shell", "write"]
        );
        assert_eq!(listed_names, tools.names());
        assert!(!list.contains("bash"));
    }

    #[test]
    fn tool_calling_names_only_real_file_and_process_tools() {
        let prompt = build_system_prompt(&registry(true));
        let contract = prompt.split_once("<tool_calling>\n").unwrap().1;
        assert!(contract.contains("read"));
        assert!(contract.contains("exec"));
        assert!(contract.contains("shell"));
        assert!(contract.contains("run together"));
        assert!(!contract.contains("bash"));
    }
}
