//! Live provider round trips against a real endpoint configured under the
//! user's home or `MYCODE_E2E_*` variables. Both tests are `#[ignore]`d and
//! run explicitly with `cargo test -p mycode-app -- --ignored`.

use std::sync::Arc;

use mycode_config::{HomeLayout, read_app_settings, read_provider_secrets};
use mycode_core::Message;
use mycode_core::{Provider as _, Request, StreamEvent};
use mycode_providers::{ReqwestTransport, ResolvedProvider, WireProvider};
use mycode_tools::ToolRegistry;
use tokio_util::sync::CancellationToken;

/// End-to-end over the real provider configured in the user's home:
/// settings parse, vault key, resolve, stream, decode. Run explicitly
/// with `cargo test -p mycode-app -- --ignored live_provider`.
#[tokio::test]
#[ignore = "calls the live provider configured under ~/.mycode"]
async fn live_provider_streams_a_reply() {
    let home = HomeLayout::from_process().expect("home");
    let settings = read_app_settings(&home).expect("settings parse");
    let secrets = read_provider_secrets(&home).expect("secrets vault");
    let provider = settings
        .providers
        .iter()
        .find(|provider| provider.enabled && provider.id.contains("minimax"))
        .or_else(|| settings.providers.iter().find(|provider| provider.enabled))
        .expect("configure an enabled provider with its key first");
    let model = provider
        .models
        .first()
        .expect("the provider lists a model")
        .clone();
    let key = secrets
        .key(&provider.id)
        .expect("the provider key lives in the vault")
        .to_owned();
    let resolved =
        ResolvedProvider::resolve(provider, &model, &key, &settings.effective_user_agent())
            .expect("resolve");
    let transport = ReqwestTransport::new().expect("transport");
    let wire = WireProvider::new(resolved, Arc::new(transport));
    let request = Request::new()
        .with_system_prompt("Reply with exactly one word.")
        .with_message(Message::User(mycode_core::UserMessage::text("Say pong.")));
    let cancel = CancellationToken::new();
    let mut stream = wire.stream(&request, cancel).await.expect("stream starts");
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::Done { .. } => break,
            StreamEvent::Error(error) => panic!(
                "stream error kind={:?} message={}",
                error.kind(),
                error.message().unwrap_or("<none>"),
            ),
            _ => {}
        }
    }
    assert!(!text.trim().is_empty(), "reply text arrives");
    println!(
        "provider {} model {} replied with {} chars",
        provider.id,
        model,
        text.chars().count()
    );
}

/// Live round trip over the real provider: a tool-bearing request, the
/// committed assistant tool_use, and the follow-up request that carries
/// the tool result — the wire shape that used to be rejected as an
/// invalid request. Run with `cargo test -p mycode-app -- --ignored`.
#[tokio::test]
#[ignore = "calls the live provider configured under ~/.mycode"]
async fn live_provider_round_trips_a_tool_call() {
    let home = HomeLayout::from_process().expect("home");
    let settings = read_app_settings(&home).expect("settings parse");
    let secrets = read_provider_secrets(&home).expect("secrets vault");
    let provider = settings
        .providers
        .iter()
        .find(|provider| provider.enabled)
        .expect("configure an enabled provider with its key first");
    let model = provider
        .models
        .first()
        .expect("the provider lists a model")
        .clone();
    let key = secrets
        .key(&provider.id)
        .expect("the provider key lives in the vault")
        .to_owned();
    let resolved =
        ResolvedProvider::resolve(provider, &model, &key, &settings.effective_user_agent())
            .expect("resolve");
    let transport = ReqwestTransport::new().expect("transport");
    let wire = WireProvider::new(resolved, Arc::new(transport));
    let registry = {
        let registry = ToolRegistry::new();
        mycode_tools::register_builtins(&registry);
        registry
    };
    let prompt = "Create hello-tool-e2e.txt containing the text hi. Use the write tool.";
    let request = Request::new()
        .with_system_prompt("Use the provided tools for file work.")
        .with_message(Message::User(mycode_core::UserMessage::text(prompt)))
        .with_tool(registry.get("write").expect("write tool").spec());

    let assistant = collect_assistant(&wire, request).await;
    let call = assistant
        .blocks
        .iter()
        .find_map(|block| match block {
            mycode_core::ContentBlock::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .expect("the model issues a write tool call");

    let follow_up = Request::new()
        .with_system_prompt("Use the provided tools for file work.")
        .with_message(Message::User(mycode_core::UserMessage::text(prompt)))
        .with_message(Message::Assistant(assistant))
        .with_message(Message::ToolResult(mycode_core::ToolResultMessage {
            tool_call_id: call.id.clone(),
            content: vec![mycode_core::ContentBlock::Text(
                mycode_core::TextBlock::new("wrote hello-tool-e2e.txt"),
            )],
            is_error: false,
            details: None,
        }))
        .with_tool(registry.get("write").expect("write tool").spec());
    let reply = collect_assistant(&wire, follow_up).await;
    let text = reply.text();
    assert!(!text.trim().is_empty(), "the follow-up reply has text");
}

/// Streams one request to its terminal assistant message (live helper).
async fn collect_assistant(wire: &WireProvider, request: Request) -> mycode_core::AssistantMessage {
    let cancel = CancellationToken::new();
    let mut stream = wire.stream(&request, cancel).await.expect("stream starts");
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Done { message } => return message,
            StreamEvent::Error(error) => panic!(
                "stream error kind={:?} message={}",
                error.kind(),
                error.message().unwrap_or("<none>"),
            ),
            _ => {}
        }
    }
    panic!("stream ended without a terminal event");
}
