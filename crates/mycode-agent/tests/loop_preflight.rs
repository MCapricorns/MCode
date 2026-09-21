//! Preflight scenarios: a same-name override must not inherit the
//! builtin search or file preflight; a blocked hook precedes file
//! preflight; a rewriting hook binds the prepared file; and a failed
//! file preflight never executes the tool.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use common::local_provider::LocalProvider;
use common::{Rig, spawn_collector, text_turn, tool_result, tool_turn, user};
use mycode_agent::hooks::GateResult;
use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_core::events::TurnOutcome;
use mycode_core::message::ContentBlock;
use mycode_tools::{
    FileAccess, FindTool, ReadTool, Tool, ToolCtx, ToolDyn, ToolError, ToolRegistry, ToolResult,
    ToolStream, WriteTool, read_file_async,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// A same-name override must not inherit builtin search preflight.
struct VirtualFind;

#[derive(Deserialize, JsonSchema)]
struct VirtualFindArgs {
    /// Glob accepted only to match the builtin schema shape.
    pattern: String,
    /// Virtual path that need not exist on disk.
    path: Option<String>,
}

#[async_trait]
impl Tool for VirtualFind {
    type Args = VirtualFindArgs;
    type Output = ();

    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Virtual find override (test fixture)."
    }

    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(format!(
            "virtual:{}:{}",
            args.pattern,
            args.path.unwrap_or_else(|| ".".to_owned())
        )))
    }
}

#[tokio::test]
async fn same_name_override_skips_search_preflight() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    registry.register(Arc::new(VirtualFind));
    let override_tool = registry.get("find").unwrap();
    assert!(!ToolDyn::requires_search_preflight(&*override_tool));

    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![(
                    "c1",
                    "find",
                    json!({"pattern": "*", "path": "missing-nowhere"}),
                )],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);
    let outcome = agent
        .prompt(
            user("find files"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert_eq!(text.text, "virtual:*:missing-nowhere");
}

/// A same-name override must not inherit builtin file preflight.
struct VirtualRead;

#[derive(Deserialize, JsonSchema)]
struct VirtualReadArgs {
    /// Path that need not exist on disk.
    path: String,
}

#[async_trait]
impl Tool for VirtualRead {
    type Args = VirtualReadArgs;
    type Output = ();

    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Virtual read override (test fixture)."
    }

    async fn execute(
        &self,
        args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text(format!("virtual-read:{}", args.path)))
    }
}

#[tokio::test]
async fn same_name_override_skips_file_preflight() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry.register(Arc::new(ReadTool));
    registry.register(Arc::new(VirtualRead));
    let override_tool = registry.get("read").unwrap();
    assert!(!ToolDyn::requires_file_preflight(&*override_tool));

    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "read",
                vec![("c1", "read", json!({"path": "missing-nowhere"}))],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);
    let outcome = agent
        .prompt(
            user("read file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert_eq!(text.text, "virtual-read:missing-nowhere");
}

/// File tool that records execute() and only uses the dispatcher-bound capability.
struct SentinelRead {
    executed: Arc<AtomicBool>,
}

#[derive(Deserialize, JsonSchema)]
struct SentinelReadArgs {
    /// Path bound at file preflight.
    path: String,
}

#[async_trait]
impl Tool for SentinelRead {
    type Args = SentinelReadArgs;
    type Output = ();

    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Sentinel read that requires dispatcher file preflight."
    }

    fn file_access(&self) -> Option<FileAccess> {
        Some(FileAccess::ExistingContent)
    }

    async fn execute(
        &self,
        args: Self::Args,
        ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        self.executed.store(true, Ordering::SeqCst);
        let Some(prepared) = ctx.prepared_file.clone() else {
            return Err(ToolError::Execution(
                "dispatcher did not bind a prepared file capability".to_owned(),
            ));
        };
        let outcome = read_file_async(
            Some(prepared),
            ctx.cwd.clone(),
            args.path,
            None,
            None,
            ctx.cancel.clone(),
        )
        .await?;
        Ok(ToolResult::text(outcome.displayed))
    }
}

#[tokio::test]
async fn hook_block_precedes_file_preflight() {
    let directory = tempfile::tempdir().unwrap();
    let executed = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(SentinelRead {
        executed: executed.clone(),
    }));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "read",
                vec![("c1", "read", json!({"path": "../outside.txt"}))],
            ),
            text_turn("blocked; noted."),
        ]),
        registry,
        hooks: HookRunner::new().with_test_gate(|_| GateResult::Block("blocked".into())),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);

    let outcome = agent
        .prompt(
            user("read file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(text.text.contains("blocked by hook"), "{text:?}");
    assert!(
        !executed.load(Ordering::SeqCst),
        "blocked hook must prevent tool execution"
    );
}

#[tokio::test]
async fn file_hook_rewrite_binds_prepared_file() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("a.txt"), "from-a").unwrap();
    std::fs::write(directory.path().join("b.txt"), "from-b").unwrap();

    let executed = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(SentinelRead {
        executed: executed.clone(),
    }));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("b.txt");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn("read", vec![("c1", "read", json!({"path": "a.txt"}))]),
            text_turn("rebound; noted."),
        ]),
        registry,
        hooks,
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);
    let outcome = agent
        .prompt(
            user("read file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(!result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("result content must be text: {result:#?}");
    };
    assert!(executed.load(Ordering::SeqCst), "rewrite must execute");
    assert!(text.text.contains("from-b"), "{text:?}");
    assert!(!text.text.contains("from-a"), "{text:?}");
}

#[tokio::test]
async fn file_preflight_missing_path_does_not_execute() {
    let directory = tempfile::tempdir().unwrap();
    let executed = Arc::new(AtomicBool::new(false));
    let registry = ToolRegistry::new();
    registry.register(Arc::new(SentinelRead {
        executed: executed.clone(),
    }));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "read",
                vec![("c1", "read", json!({"path": "missing-nowhere.txt"}))],
            ),
            text_turn("noted."),
        ]),
        registry,
        hooks: HookRunner::new(),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);
    let outcome = agent
        .prompt(
            user("write file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    assert!(
        !executed.load(Ordering::SeqCst),
        "failed file preflight must not execute the tool"
    );
}

#[tokio::test]
async fn hook_block_does_not_echo_write_content() {
    const SECRET: &str = "MYCODE-SECRET-SENTINEL-9f3a";
    let directory = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool));
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "write",
                vec![(
                    "c1",
                    "write",
                    json!({"path": "secret.txt", "content": SECRET}),
                )],
            ),
            text_turn("blocked; noted."),
        ]),
        registry,
        hooks: HookRunner::new().with_test_gate(|_| GateResult::Block("blocked".into())),
        events: broadcast::channel(256).0,
        cancel: CancellationToken::new(),
    };
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);
    let outcome = agent
        .prompt(
            user("write file"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(text.text.contains("blocked by hook"), "{text:?}");
    assert!(
        !text.text.contains(SECRET),
        "write content must not appear in the model-visible error: {text:?}"
    );
}
