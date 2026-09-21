//! Steer and follow-up scenarios: a steer queued mid-stream jumps the
//! queue to become the next user input, and a follow-up continues an
//! agent that is about to stop.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use common::local_provider::LocalProvider;
use common::{DELAY, Rig, spawn_on_first_delta, text_turn, tool_turn, user};
use mycode_agent::{Agent, AgentConfig};
use mycode_core::events::{AgentEvent, TurnOutcome};
use mycode_core::message::Message;
use serde_json::json;

#[tokio::test]
async fn steer_jumps_the_queue_after_the_current_response() {
    let rig = Rig::new(
        LocalProvider::new(vec![
            tool_turn(
                "fetching the value",
                vec![("c1", "echo", json!({"text": "data"}))],
            ),
            text_turn("Understood, pivoting now."),
        ])
        .with_delay(DELAY),
    );
    let mut agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    let collector = common::spawn_collector(&rig.events);
    let steerer = spawn_on_first_delta(&rig, move || {
        handle.steer(user("stop, do this instead"));
    });

    let outcome = agent
        .prompt(user("compute something"), &rig.env())
        .await
        .expect("prompt must succeed");
    steerer.await.expect("steerer must finish");

    assert_eq!(outcome, TurnOutcome::Steered);

    // History: user, assistant(call), result, steer-user, final assistant.
    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[3], user("stop, do this instead"));
    assert!(matches!(messages[4], Message::Assistant(_)));

    // The steer message was injected as the next user input before the
    // second response — the queue-jump assertion.
    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages, messages[..4]);
    assert_eq!(requests[1].messages[3], user("stop, do this instead"));

    let events = collector.await.expect("collector must finish");
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Steered))
    );
    // The tool from response 1 still executed before the steer landed.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCompleted { .. }))
    );
}

#[tokio::test]
async fn follow_up_continues_when_agent_would_stop() {
    let rig = Rig::new(
        LocalProvider::new(vec![
            text_turn("First answer, complete."),
            text_turn("Follow-up answer, also done."),
        ])
        .with_delay(DELAY),
    );
    let mut agent = Agent::new(AgentConfig::new());
    let handle = agent.handle();
    let followupper = spawn_on_first_delta(&rig, move || {
        handle.follow_up(user("and also check the tests"));
    });

    let outcome = agent
        .prompt(user("answer the question"), &rig.env())
        .await
        .expect("prompt must succeed");
    followupper.await.expect("follow-up task must finish");

    // Follow-ups do not mark the turn as steered.
    assert_eq!(outcome, TurnOutcome::Completed);

    let messages = &agent.state().messages;
    assert_eq!(messages.len(), 4); // user, assistant, follow-up user, assistant
    assert_eq!(messages[2], user("and also check the tests"));

    let requests = rig.provider.recorded_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages[2], user("and also check the tests"));
}
