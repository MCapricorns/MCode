//! Test-only scripted provider for `mycode-agent` integration tests.
//!
//! This module is compiled only as integration-test support. It is not a
//! production provider or Host adapter and must not be linked into products.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mycode_core::message::{AssistantMessage, ContentBlock};
use mycode_core::{
    EventStream, EventStreamSender, Provider, ProviderError, ProviderErrorKind, Request,
    StreamEvent,
};
use tokio_util::sync::CancellationToken;

const SHARD_CHARS: usize = 16;

/// One scripted turn: stream a message or fail.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalTurn {
    /// Stream this assistant message as one full turn.
    Message(AssistantMessage),
    /// Terminate the turn with this error.
    Fail(ProviderError),
}

/// Test-only in-process [`Provider`] that replays fixed turns.
#[derive(Debug)]
pub struct LocalProvider {
    turns: Arc<Mutex<VecDeque<LocalTurn>>>,
    requests: Arc<Mutex<Vec<Request>>>,
    delay: Duration,
}

impl LocalProvider {
    /// Builds from an inline script.
    pub fn new(turns: Vec<LocalTurn>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
            delay: Duration::ZERO,
        }
    }

    /// Delays between emitted events for steer and abort tests.
    #[must_use]
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Returns all requests received so far.
    pub fn recorded_requests(&self) -> Vec<Request> {
        self.requests.lock().expect("local requests lock").clone()
    }

    /// Appends one turn to the script.
    pub fn push_turn(&self, turn: LocalTurn) {
        self.turns.lock().expect("local turns lock").push_back(turn);
    }
}

fn shard(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(SHARD_CHARS)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

async fn emit(
    sender: &EventStreamSender,
    event: StreamEvent,
    delay: Duration,
    cancel: &CancellationToken,
) -> bool {
    if delay > Duration::ZERO {
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = cancel.cancelled() => return false,
            () = sender.closed() => return false,
        }
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = sender.closed() => false,
        sent = sender.send(event) => sent,
    }
}

#[async_trait]
impl Provider for LocalProvider {
    async fn stream(
        &self,
        request: &Request,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        self.requests
            .lock()
            .expect("local requests lock")
            .push(request.clone());
        let turn = self
            .turns
            .lock()
            .expect("local turns lock")
            .pop_front()
            .ok_or_else(|| {
                ProviderError::with_message(
                    ProviderErrorKind::Rejected,
                    "local provider script exhausted",
                )
            })?;

        let (sender, stream) = EventStream::channel(cancel.clone());
        let delay = self.delay;
        tokio::spawn(async move {
            match turn {
                LocalTurn::Message(message) => {
                    for block in &message.blocks {
                        match block {
                            ContentBlock::Text(text) => {
                                for piece in shard(&text.text) {
                                    if !emit(&sender, StreamEvent::TextDelta(piece), delay, &cancel)
                                        .await
                                    {
                                        return;
                                    }
                                }
                            }
                            ContentBlock::Thinking(thinking) => {
                                for piece in shard(&thinking.text) {
                                    if !emit(
                                        &sender,
                                        StreamEvent::ThinkingDelta(piece),
                                        delay,
                                        &cancel,
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                }
                            }
                            ContentBlock::ToolCall(call) => {
                                let arguments = call.arguments.to_string();
                                for piece in shard(&arguments) {
                                    if !emit(
                                        &sender,
                                        StreamEvent::ToolCallDelta {
                                            id: call.id.clone(),
                                            partial_json: piece,
                                        },
                                        delay,
                                        &cancel,
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                }
                            }
                            ContentBlock::Image(_) => {}
                        }
                    }
                    let _ = emit(
                        &sender,
                        StreamEvent::Done { message },
                        Duration::ZERO,
                        &cancel,
                    )
                    .await;
                }
                LocalTurn::Fail(error) => {
                    let _ = emit(&sender, StreamEvent::Error(error), Duration::ZERO, &cancel).await;
                }
            }
        });
        Ok(stream)
    }
}
