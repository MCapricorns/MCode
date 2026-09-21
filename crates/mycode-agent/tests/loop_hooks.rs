//! Hook rewrite scenarios for search tools: a rewriting gate rebinds
//! the search path (and executes against it), a rewrite to a path that
//! does not exist fails closed even after the path later appears, and —
//! on Windows — a rewrite to a share-locked alias does not execute.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use std::sync::Arc;

use common::local_provider::LocalProvider;
use common::{Rig, spawn_collector, text_turn, tool_result, tool_turn, user};
use mycode_agent::hooks::GateResult;
use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_core::events::TurnOutcome;
use mycode_core::message::ContentBlock;
use mycode_tools::FindTool;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn hook_rewrite_binds_search_and_executes() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("safe")).unwrap();
    std::fs::write(directory.path().join("safe").join("keep.txt"), "x").unwrap();
    std::fs::create_dir(directory.path().join("secrets")).unwrap();
    std::fs::write(directory.path().join("secrets").join("leak.txt"), "x").unwrap();

    let registry = mycode_tools::ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("secrets");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![("c1", "find", json!({"pattern": "*.txt", "path": "safe"}))],
            ),
            text_turn("rewritten; noted."),
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
    assert!(text.text.contains("leak.txt"), "{text:?}");
    assert!(!text.text.contains("keep.txt"), "{text:?}");
}

#[tokio::test]
async fn hook_rewrite_to_missing_path_does_not_execute_after_it_appears() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("safe")).unwrap();
    std::fs::write(directory.path().join("safe").join("keep.txt"), "x").unwrap();

    let registry = mycode_tools::ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("later");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![("c1", "find", json!({"pattern": "*.txt", "path": "safe"}))],
            ),
            text_turn("noted."),
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
            user("find files"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    std::fs::create_dir(directory.path().join("later")).unwrap();
    std::fs::write(directory.path().join("later").join("leak.txt"), "x").unwrap();
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(
        text.text.contains("does not exist") || text.text.contains("inaccessible"),
        "{text:?}"
    );
    assert!(!text.text.contains("leak.txt"), "{text:?}");
}

#[cfg(windows)]
#[tokio::test]
async fn hook_rewrite_to_share_locked_alias_does_not_execute() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("safe")).unwrap();
    std::fs::write(directory.path().join("safe").join("keep.txt"), "x").unwrap();
    std::fs::create_dir(directory.path().join("Visible")).unwrap();
    std::fs::write(directory.path().join("Visible").join("leak.txt"), "secret").unwrap();
    // FILE_FLAG_BACKUP_SEMANTICS: required to lock a directory handle.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory.path().join("Visible"))
        .expect("exclusive directory lock");

    let registry = mycode_tools::ToolRegistry::new();
    registry.register(Arc::new(FindTool));
    let hooks = HookRunner::new().with_test_gate(|args| {
        args["path"] = json!("visible");
        GateResult::Pass
    });
    let rig = Rig {
        provider: LocalProvider::new(vec![
            tool_turn(
                "search",
                vec![("c1", "find", json!({"pattern": "*.txt", "path": "safe"}))],
            ),
            text_turn("noted."),
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
            user("find files"),
            &rig.env_at(directory.path().to_path_buf()),
        )
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    drop(lock);
    let events = collector.await.expect("collector must finish");
    let result = tool_result(&events);
    assert!(result.is_error, "{result:#?}");
    let ContentBlock::Text(text) = &result.content[0] else {
        panic!("error content must be text: {result:#?}");
    };
    assert!(!text.text.contains("leak.txt"), "{text:?}");
    assert!(
        text.text.contains("does not exist")
            || text.text.contains("inaccessible")
            || text.text.to_ascii_lowercase().contains("sharing"),
        "{text:?}"
    );
}
