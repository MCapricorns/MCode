//! `ToolRegistry` — name-keyed store of type-erased tools
//! (design doc `02-tools-permissions.md` §2).
//!
//! Registration is **last-wins** per name (pi semantics): a plugin can
//! override a builtin by registering under the same name. Specs are
//! served sorted by tool name so provider requests serialize
//! deterministically.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use mycode_core::tool::ToolSpec;

use crate::tool::ToolDyn;

/// Thread-safe registry of tools, keyed by name.
pub struct ToolRegistry {
    tools: RwLock<BTreeMap<String, Arc<dyn ToolDyn>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(BTreeMap::new()),
        }
    }

    /// Register a tool. If a tool with the same name is already
    /// registered, the new one replaces it (last-wins — plugins may
    /// override builtins).
    pub fn register(&self, tool: Arc<dyn ToolDyn>) {
        let name = tool.spec().name;
        self.write().insert(name, tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn ToolDyn>> {
        self.read().get(name).cloned()
    }

    /// Specs of all registered tools, sorted by tool name for stable
    /// provider serialization.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.read().values().map(|tool| tool.spec()).collect()
    }

    /// Names and optional prompt snippets of all registered tools, sorted by name.
    pub fn prompt_entries(&self) -> Vec<(String, Option<String>)> {
        self.read()
            .iter()
            .map(|(name, tool)| {
                (
                    name.clone(),
                    tool.prompt_snippet_dyn().map(ToOwned::to_owned),
                )
            })
            .collect()
    }

    /// Names of all registered tools, sorted.
    pub fn names(&self) -> Vec<String> {
        self.read().keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    fn read(&self) -> RwLockReadGuard<'_, BTreeMap<String, Arc<dyn ToolDyn>>> {
        self.tools.read().expect("tool registry lock poisoned")
    }

    fn write(&self) -> RwLockWriteGuard<'_, BTreeMap<String, Arc<dyn ToolDyn>>> {
        self.tools.write().expect("tool registry lock poisoned")
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::ToolCtx;
    use crate::stream::ToolStream;
    use crate::tool::{Tool, ToolError, ToolResult};
    use async_trait::async_trait;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::json;

    struct StubTool {
        name: &'static str,
        description: &'static str,
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
            self.description
        }

        async fn execute(
            &self,
            _args: Self::Args,
            _ctx: &ToolCtx,
            _out: &mut ToolStream,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::text(self.name))
        }
    }

    #[test]
    fn register_and_get() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool {
            name: "alpha",
            description: "a",
        }));

        assert_eq!(registry.len(), 1);
        assert!(registry.get("alpha").is_some());
        assert!(registry.get("missing").is_none());
        assert_eq!(registry.names(), vec!["alpha".to_owned()]);
    }

    #[test]
    fn last_registration_wins_per_name() {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool {
            name: "read",
            description: "builtin read",
        }));
        registry.register(Arc::new(StubTool {
            name: "read",
            description: "plugin read override",
        }));

        // One entry, the later registration.
        assert_eq!(registry.len(), 1);
        let specs = registry.specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].description, "plugin read override");
        assert_eq!(
            registry.get("read").unwrap().spec().description,
            "plugin read override"
        );
    }

    #[test]
    fn specs_are_sorted_by_name() {
        let registry = ToolRegistry::new();
        for name in ["grep", "edit", "shell", "write", "read"] {
            registry.register(Arc::new(StubTool {
                name,
                description: "stub",
            }));
        }

        let specs = registry.specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["edit", "grep", "read", "shell", "write"]);
    }

    #[test]
    fn empty_registry() {
        let registry = ToolRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.specs().is_empty());
        assert!(registry.names().is_empty());
    }

    #[tokio::test]
    async fn dispatches_through_stored_dyn_tool() {
        // The stored Arc<dyn ToolDyn> executes via the blanket impl path.
        let registry = ToolRegistry::new();
        registry.register(Arc::new(StubTool {
            name: "alpha",
            description: "a",
        }));

        let tool = registry.get("alpha").unwrap();
        let ctx = ToolCtx::new(".");
        let result = tool
            .execute_dyn(json!({}), &ctx, &mut ToolStream::closed())
            .await
            .unwrap();
        assert_eq!(
            result.content,
            vec![mycode_core::message::ContentBlock::Text("alpha".into())]
        );
    }
}
