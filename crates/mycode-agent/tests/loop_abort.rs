//! Abort scenarios: `CancellationToken` mid-turn keeps state consistent
//! (no half `TurnEnded::Completed`), and an abort mid-dispatch of a
//! multi-call response still answers every tool call.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use common::local_provider::LocalProvider;
use common::{
    DELAY, Rig, spawn_collector, spawn_on_first_delta, spawn_on_first_tool_completed, text_turn,
    tool_turn, user,
};
use mycode_agent::{Agent, AgentConfig};
use mycode_core::events::{AgentEvent, TurnOutcome};
use mycode_core::message::{ContentBlock, Message};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn abort_via_env_cancel_mid_stream_keeps_state_consistent() {
    let long = "x".repeat(600); // many shards → ample cancel window
    let rig = Rig::new(LocalProvider::new(vec![text_turn(&long)]).with_delay(DELAY));
    let cancel = rig.cancel.clone();
    let canceller = spawn_on_first_delta(&rig, move || cancel.cancel());
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);

    let outcome = agent
        .prompt(user("an essay please"), &rig.env())
        .await
        .expect("abort is a normal outcome, not an error");
    canceller.await.expect("canceller must finish");

    assert_eq!(outcome, TurnOutcome::Aborted);
    assert!(!agent.state().is_streaming);
    // No partial assistant message enters the history.
    assert_eq!(agent.state().messages, vec![user("an essay please")]);

    let events = collector.await.expect("collector must finish");
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::MessageAdded(Message::Assistant(_))))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Error(_))),
        "cancellation is an aborted outcome, not a provider error: {events:#?}"
    );

    // The agent recovers: a fresh caller token starts a fresh turn
    // (the original env token stays cancelled, by design).
    rig.provider.push_turn(text_turn("recovered"));
    let outcome = agent
        .prompt(
            user("try again"),
            &rig.env().with_cancel(CancellationToken::new()),
        )
        .await
        .expect("second prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    // user1, user2, recovered assistant (no partial aborted response).
    assert_eq!(agent.state().messages.len(), 3);
}

/// Aborting mid-dispatch of a multi-call response must still answer
/// every `tool_call` in the history (the OpenAI wire format requires
/// one tool message per call id after an assistant `tool_calls`
/// message; chat-completions history has no pairing guard).
#[tokio::test]
async fn abort_mid_multi_call_answers_every_tool_call() {
    let rig = Rig::new(
        LocalProvider::new(vec![tool_turn(
            "three calls",
            vec![
                ("c1", "echo", json!({"text": "one"})),
                ("c2", "echo", json!({"text": "two"})),
                ("c3", "echo", json!({"text": "three"})),
            ],
        )])
        .with_delay(DELAY),
    );
    let cancel = rig.cancel.clone();
    let canceller = spawn_on_first_tool_completed(&rig, move || cancel.cancel());
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);

    let outcome = agent
        .prompt(user("run three calls"), &rig.env())
        .await
        .expect("abort is a normal outcome, not an error");
    canceller.await.expect("canceller must finish");

    assert_eq!(outcome, TurnOutcome::Aborted);

    // History: user, assistant(3 calls), then exactly one tool result
    // per call id, in call order — regardless of where the abort cut
    // the dispatch short. The calls after the cut are synthesized
    // cancellation results.
    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 5);
    let Message::Assistant(assistant) = &messages[1] else {
        panic!("message 2 must be the assistant message: {messages:#?}");
    };
    let call_ids: Vec<String> = assistant
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolCall(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(call_ids, vec!["c1", "c2", "c3"]);
    for (offset, id) in call_ids.iter().enumerate() {
        let Message::ToolResult(result) = &messages[2 + offset] else {
            panic!(
                "message {} must be a tool result: {messages:#?}",
                3 + offset
            );
        };
        assert_eq!(&result.tool_call_id, id);
    }
    // c1 completed before the abort fired.
    let Message::ToolResult(first) = &messages[2] else {
        panic!("must be a result")
    };
    assert!(!first.is_error);
    // The undispatched tail is a cancellation error result.
    let Message::ToolResult(last) = &messages[4] else {
        panic!("must be a result")
    };
    assert!(last.is_error);
    let ContentBlock::Text(text) = &last.content[0] else {
        panic!("error content must be text: {last:#?}");
    };
    assert!(text.text.contains("aborted"));

    let events = collector.await.expect("collector must finish");
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );

    // The next turn sends this history to the provider as-is: the
    // assistant `tool_calls` message is immediately followed by one
    // tool result per call id — the wire invariant the OpenAI provider
    // relies on.
    let aborted_history: Vec<Message> = agent.state().messages.clone();
    rig.provider.push_turn(text_turn("recovered"));
    let outcome = agent
        .prompt(
            user("try again"),
            &rig.env().with_cancel(CancellationToken::new()),
        )
        .await
        .expect("second prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    let second = &requests[1].messages;
    assert_eq!(second.len(), 6); // user, assistant(3 calls), 3 results, user2
    assert_eq!(&second[1], &aborted_history[1]);
    for (offset, id) in call_ids.iter().enumerate() {
        let Message::ToolResult(result) = &second[2 + offset] else {
            panic!("wire history must answer every call");
        };
        assert_eq!(&result.tool_call_id, id);
    }
}
