//! `task` calls announced in one assistant message run together.
//! Other tools stay in call order so an abort can still answer the
//! ones that never started.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use common::local_provider::LocalProvider;
use common::{NoArgs, Rig, text_turn, tool_turn, user};
use mycode_agent::{Agent, AgentConfig};
use mycode_core::events::TurnOutcome;
use mycode_core::message::{ContentBlock, Message};
use mycode_tools::{Tool, ToolCtx, ToolError, ToolResult, ToolStream};
use serde_json::json;

/// Sleeps long enough that a serial dispatcher cannot overlap calls.
struct SlowTask {
    inflight: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for SlowTask {
    type Args = NoArgs;
    type Output = ();

    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Slow task stand-in."
    }

    async fn execute(
        &self,
        _args: Self::Args,
        _ctx: &ToolCtx,
        _out: &mut ToolStream,
    ) -> Result<ToolResult, ToolError> {
        let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolResult::text("scout done"))
    }
}

#[tokio::test]
async fn four_task_calls_in_one_response_overlap() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn(
            "four scouts",
            vec![
                ("c1", "task", json!({})),
                ("c2", "task", json!({})),
                ("c3", "task", json!({})),
                ("c4", "task", json!({})),
            ],
        ),
        text_turn("all four reported"),
    ]));
    let peak = Arc::new(AtomicUsize::new(0));
    rig.registry.register(Arc::new(SlowTask {
        inflight: Arc::new(AtomicUsize::new(0)),
        peak: Arc::clone(&peak),
    }));
    let mut agent = Agent::new(AgentConfig::new());

    let outcome = agent
        .prompt(user("scout four trees"), &rig.env())
        .await
        .expect("turn");

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(peak.load(Ordering::SeqCst), 4);

    let messages = &agent.state().messages;
    let call_ids: Vec<String> = messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => Some(result.tool_call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(call_ids, vec!["c1", "c2", "c3", "c4"]);
    for message in messages.iter().filter_map(|message| match message {
        Message::ToolResult(result) => Some(result),
        _ => None,
    }) {
        assert!(!message.is_error);
        let ContentBlock::Text(text) = &message.content[0] else {
            panic!("result text");
        };
        assert_eq!(text.text, "scout done");
    }
}
