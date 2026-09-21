//! Basic loop scenarios: single-turn text replies, the multi-turn tool
//! loop, queue-mode drain semantics, and steer queued while idle.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use common::local_provider::LocalProvider;
use common::{Rig, position, spawn_collector, text_turn, tool_turn, user};
use mycode_agent::agent::QueueMode;
use mycode_agent::{Agent, AgentConfig, TurnEnv, build_system_prompt};
use mycode_core::events::{AgentEvent, MessageDelta, TurnOutcome};
use mycode_core::message::{AssistantMessage, ContentBlock, Message, StopReason};
use mycode_tools::ToolRegistry;
use serde_json::json;

#[tokio::test]
async fn single_text_reply_stops_and_streams_events() {
    let rig = Rig::new(LocalProvider::new(vec![text_turn("Hello there!")]));
    let mut agent = Agent::new(AgentConfig::new().with_system_prompt("be terse"));
    let collector = spawn_collector(&rig.events);

    let outcome = agent
        .prompt(user("hi"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(
        agent.state().messages,
        vec![
            user("hi"),
            Message::Assistant(AssistantMessage {
                blocks: vec![ContentBlock::Text("Hello there!".into())],
                usage: None,
                stop_reason: StopReason::Stop,
            })
        ]
    );
    assert!(!agent.state().is_streaming);

    // Request shape: system prompt, history, registry specs flow in.
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].system_prompt, vec!["be terse".to_string()]);
    assert_eq!(requests[0].messages, vec![user("hi")]);
    assert!(requests[0].tools.iter().any(|spec| spec.name == "echo"));

    // Event order: TurnStarted → MessageAdded(user) → deltas →
    // MessageAdded(assistant) → TurnEnded(Completed).
    let events = collector.await.expect("collector must finish");
    assert_eq!(events.first(), Some(&AgentEvent::TurnStarted));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Completed))
    );
    let user_pos = position(
        &events,
        |e| matches!(e, AgentEvent::MessageAdded(Message::User(_))),
        "MessageAdded(user)",
    );
    let delta_pos = position(
        &events,
        |e| matches!(e, AgentEvent::MessageDelta(MessageDelta::TextDelta(_))),
        "TextDelta",
    );
    let assistant_pos = position(
        &events,
        |e| matches!(e, AgentEvent::MessageAdded(Message::Assistant(_))),
        "MessageAdded(assistant)",
    );
    assert_eq!(user_pos, 1);
    assert!(user_pos < delta_pos);
    assert!(delta_pos < assistant_pos);
    // No tool events in a pure text turn.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolStarted { .. }))
    );
}

#[tokio::test]
async fn provider_request_receives_canonical_builtin_specs() {
    let provider = LocalProvider::new(vec![text_turn("done")]);
    let registry = ToolRegistry::new();
    mycode_tools::builtin::register_builtins(&registry);
    let hooks = mycode_agent::HookRunner::new();
    let env = TurnEnv::new(&provider, &registry, &hooks);
    let mut agent = Agent::new(AgentConfig::new());

    let outcome = agent
        .prompt(user("inspect tools"), &env)
        .await
        .expect("prompt must succeed");
    assert_eq!(outcome, TurnOutcome::Completed);

    let requests = provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    let names: Vec<&str> = requests[0]
        .tools
        .iter()
        .map(|spec| spec.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["edit", "exec", "find", "grep", "read", "shell", "write"]
    );
    assert!(!names.contains(&"bash"));
}

#[tokio::test]
async fn tool_call_loop_executes_writes_back_and_stops() {
    let rig = Rig::new(LocalProvider::new(vec![
        tool_turn("let me echo", vec![("c1", "echo", json!({"text": "hi"}))]),
        text_turn("I echoed the text."),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);

    let outcome = agent
        .prompt(user("run the echo"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 4); // user, assistant(call), result, assistant(final)
    let Message::ToolResult(result) = &messages[2] else {
        panic!("message 3 must be the tool result: {messages:#?}");
    };
    assert_eq!(result.tool_call_id, "c1");
    assert!(!result.is_error);
    assert_eq!(result.content, vec![ContentBlock::Text("echo: hi".into())]);

    // The second request carries the full history including the result.
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].system_prompt,
        vec![build_system_prompt(&rig.registry)]
    );
    assert_eq!(requests[1].system_prompt, requests[0].system_prompt);
    assert_eq!(requests[1].messages.len(), 3);
    assert!(matches!(&requests[1].messages[2], Message::ToolResult(_)));
    assert_eq!(requests[1].messages, messages[..3]);

    let events = collector.await.expect("collector must finish");
    let started = position(
        &events,
        |e| matches!(e, AgentEvent::ToolStarted { call_id, name } if call_id.as_str() == "c1" && name == "echo"),
        "ToolStarted(c1)",
    );
    let completed = position(
        &events,
        |e| matches!(e, AgentEvent::ToolCompleted { result, .. } if !result.is_error),
        "ToolCompleted(c1)",
    );
    let result_added = position(
        &events,
        |e| matches!(e, AgentEvent::MessageAdded(Message::ToolResult(_))),
        "MessageAdded(ToolResult)",
    );
    assert!(started < completed);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Completed))
    );
    let _ = result_added;
}

#[tokio::test]
async fn queue_mode_one_at_a_time_delivers_each_follow_up_in_own_request() {
    let rig = Rig::new(LocalProvider::new(vec![
        text_turn("answer one"),
        text_turn("answer two"),
        text_turn("answer three"),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    assert_eq!(agent.queue_mode(), QueueMode::OneAtATime);
    // Queued while idle (a subagent callback before the next prompt).
    agent.follow_up(user("task one"));
    agent.follow_up(user("task two"));

    let outcome = agent
        .prompt(user("start"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(rig.provider.recorded_requests().len(), 3);
    let messages = &agent.state().messages;
    // user, asst, task1, asst, task2, asst
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[2], user("task one"));
    assert_eq!(messages[4], user("task two"));
}

#[tokio::test]
async fn queue_mode_all_batches_follow_ups_into_one_request() {
    let rig = Rig::new(LocalProvider::new(vec![
        text_turn("answer one"),
        text_turn("answer both"),
    ]));
    let mut agent = Agent::new(AgentConfig::new());
    agent.set_queue_mode(QueueMode::All);
    agent.follow_up(user("task one"));
    agent.follow_up(user("task two"));

    let outcome = agent
        .prompt(user("start"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(rig.provider.recorded_requests().len(), 2);
    let requests = rig.provider.recorded_requests();
    // Both follow-ups were injected before the second response.
    assert_eq!(requests[1].messages[2], user("task one"));
    assert_eq!(requests[1].messages[3], user("task two"));
}

#[tokio::test]
async fn steer_queued_while_idle_lands_before_the_first_response() {
    let rig = Rig::new(LocalProvider::new(vec![text_turn("combined answer")]));
    let mut agent = Agent::new(AgentConfig::new());
    agent.steer(user("context update before you start"));

    let outcome = agent
        .prompt(user("initial prompt"), &rig.env())
        .await
        .expect("prompt must succeed");

    assert_eq!(outcome, TurnOutcome::Steered);
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages.len(), 2);
    assert_eq!(requests[0].messages[0], user("initial prompt"));
    assert_eq!(
        requests[0].messages[1],
        user("context update before you start")
    );
}
