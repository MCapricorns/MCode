//! Provider failure scenarios: stream errors and pre-stream failures
//! emit an `Error` event and abort the turn with `Err`; setup
//! cancellation maps to a plain abort; oversized requests fail closed
//! before the provider is called; a dangling producer becomes a
//! protocol error event.
//!
//! Part of the loop scenario groups listed in `common/mod.rs`.

mod common;

use async_trait::async_trait;
use common::local_provider::{LocalProvider, LocalTurn};
use common::{Rig, position, spawn_collector, text_turn, user};
use mycode_agent::{Agent, AgentConfig, HookRunner, TurnEnv};
use mycode_core::events::{AgentEvent, TurnOutcome};
use mycode_core::{
    EventStream, MAX_REQUEST_ENCODED_BYTES, Provider, ProviderError, ProviderErrorKind, Request,
    StreamEvent,
};
use mycode_tools::ToolRegistry;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn provider_error_returns_err_without_half_completed_turn() {
    let rig = Rig::new(LocalProvider::new(vec![LocalTurn::Fail(
        ProviderError::with_message(ProviderErrorKind::Unavailable, "boom"),
    )]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);

    let err = agent
        .prompt(user("hi"), &rig.env())
        .await
        .expect_err("provider failure must surface as Err");
    assert!(matches!(err, mycode_core::MycodeError::Provider(_)));
    assert!(!agent.state().is_streaming);
    assert_eq!(agent.state().messages, vec![user("hi")]);

    let events = collector.await.expect("collector must finish");
    let error_pos = position(
        &events,
        |event| {
            matches!(
                event,
                AgentEvent::Error(mycode_core::MycodeError::Provider(_))
            )
        },
        "AgentEvent::Error(Provider)",
    );
    let ended_pos = position(
        &events,
        |event| matches!(event, AgentEvent::TurnEnded(_)),
        "TurnEnded",
    );
    assert!(error_pos < ended_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
}

/// A provider that fails before streaming begins (`stream()` returns
/// `Err` — connect/config failure; concretely, a local provider whose
/// script is exhausted).
#[tokio::test]
async fn provider_request_failure_emits_error_event() {
    // Empty script: the first `stream()` call fails with a typed Rejected error.
    let rig = Rig::new(LocalProvider::new(vec![]));
    let mut agent = Agent::new(AgentConfig::new());
    let collector = spawn_collector(&rig.events);

    let err = agent
        .prompt(user("hi"), &rig.env())
        .await
        .expect_err("pre-stream provider failure must surface as Err");
    assert!(matches!(err, mycode_core::MycodeError::Provider(_)));
    assert!(!agent.state().is_streaming);
    assert_eq!(agent.state().messages, vec![user("hi")]);

    // The telemetry contract: Error event emitted at the failure site,
    // then TurnEnded(Aborted) — never a silent mislabel as a plain abort.
    let events = collector.await.expect("collector must finish");
    let error_pos = position(
        &events,
        |e| matches!(e, AgentEvent::Error(mycode_core::MycodeError::Provider(_))),
        "AgentEvent::Error(Provider)",
    );
    let ended_pos = position(
        &events,
        |e| matches!(e, AgentEvent::TurnEnded(_)),
        "TurnEnded",
    );
    assert!(error_pos < ended_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
}

struct SetupCancelledProvider;

#[async_trait]
impl Provider for SetupCancelledProvider {
    async fn stream(
        &self,
        _request: &Request,
        _cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        Err(ProviderError::new(ProviderErrorKind::Cancelled))
    }
}

#[tokio::test]
async fn provider_setup_cancelled_maps_to_aborted_without_error_event() {
    let provider = SetupCancelledProvider;
    let registry = ToolRegistry::new();
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    let env = TurnEnv::new(&provider, &registry, &hooks).with_events(events.clone());
    let mut receiver = events.subscribe();
    let mut agent = Agent::new(AgentConfig::new());

    let outcome = agent
        .prompt(user("hi"), &env)
        .await
        .expect("setup cancellation is a normal abort");
    assert_eq!(outcome, TurnOutcome::Aborted);
    assert_eq!(agent.state().messages, vec![user("hi")]);

    let mut observed = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        observed.push(event);
    }
    assert_eq!(
        observed.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !observed
            .iter()
            .any(|event| matches!(event, AgentEvent::Error(_))),
        "setup cancellation must not emit an error: {observed:#?}"
    );
}

#[tokio::test]
async fn oversized_request_fails_closed_before_provider_call() {
    let provider = LocalProvider::new(vec![text_turn("must not run")]);
    let registry = ToolRegistry::new();
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    let env = TurnEnv::new(&provider, &registry, &hooks).with_events(events.clone());
    let mut receiver = events.subscribe();
    let mut agent =
        Agent::new(AgentConfig::new().with_system_prompt("x".repeat(MAX_REQUEST_ENCODED_BYTES)));

    let error = agent
        .prompt(user("hi"), &env)
        .await
        .expect_err("oversized request must fail closed");
    assert!(matches!(error, mycode_core::MycodeError::Provider(_)));
    assert!(
        provider.recorded_requests().is_empty(),
        "provider must not be called after validation fails"
    );

    let mut observed = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        observed.push(event);
    }
    let error_pos = position(
        &observed,
        |event| {
            matches!(
                event,
                AgentEvent::Error(mycode_core::MycodeError::Provider(_))
            )
        },
        "AgentEvent::Error(Provider)",
    );
    let ended_pos = position(
        &observed,
        |event| matches!(event, AgentEvent::TurnEnded(TurnOutcome::Aborted)),
        "TurnEnded(Aborted)",
    );
    assert!(error_pos < ended_pos);
}

/// A provider whose producer exits without sending `Done` or `Error`.
///
/// The neutral stream converts that drop into one Protocol terminal.
struct DanglingStreamProvider;

#[async_trait]
impl Provider for DanglingStreamProvider {
    async fn stream(
        &self,
        _req: &Request,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        let (sender, stream) = EventStream::channel(cancel);
        let _ = sender.send(StreamEvent::TextDelta("partial".into())).await;
        // Dropping every sender synthesizes one Protocol terminal.
        drop(sender);
        Ok(stream)
    }
}

#[tokio::test]
async fn producer_drop_becomes_protocol_error_event() {
    let provider = DanglingStreamProvider;
    let registry = ToolRegistry::new();
    let hooks = HookRunner::new();
    let events = broadcast::channel(256).0;
    let env = TurnEnv::new(&provider, &registry, &hooks)
        .with_events(events.clone())
        .with_cancel(CancellationToken::new());
    let collector = spawn_collector(&events);

    let mut agent = Agent::new(AgentConfig::new());
    let err = agent
        .prompt(user("hi"), &env)
        .await
        .expect_err("producer drop must surface as Err");
    assert!(
        matches!(err, mycode_core::MycodeError::Provider(message) if message
        .contains("producer ended without a terminal event"))
    );
    assert_eq!(agent.state().messages, vec![user("hi")]);

    let events = collector.await.expect("collector must finish");
    let error_pos = position(
        &events,
        |e| {
            matches!(
                e,
                AgentEvent::Error(mycode_core::MycodeError::Provider(message))
                    if message.contains("producer ended without a terminal event")
            )
        },
        "AgentEvent::Error(producer ended without a terminal event)",
    );
    let ended_pos = position(
        &events,
        |e| matches!(e, AgentEvent::TurnEnded(_)),
        "TurnEnded",
    );
    assert!(error_pos < ended_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnEnded(TurnOutcome::Aborted))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnEnded(TurnOutcome::Completed)))
    );
}
