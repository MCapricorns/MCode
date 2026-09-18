//! Core bridge: one background thread owning the tokio-backed core services.
//!
//! GPUI runs its own executor, so the desktop never touches tokio types
//! directly. [`CoreBridge`] owns a dedicated thread with a current-thread
//! tokio runtime hosting the [`SessionService`]; the UI sends
//! [`BridgeCommand`]s and awaits [`BridgeReply`]s through tokio oneshot
//! channels, whose receivers are executor-agnostic futures. Model turns run
//! as concurrent runtime tasks that stream [`BridgeEvent`]s back through a
//! bounded channel the UI polls. Configuration reads stay synchronous on the
//! same thread.
use std::sync::mpsc;
use std::thread::JoinHandle;

use mcode_config::{
    AppSettings, AuthorityRevision, HomeLayout, read_app_settings, read_provider_secrets,
    replace_app_settings, replace_provider_secrets,
};
use mcode_core::Message;
use mcode_provider_api::{Provider as _, StreamEvent as ProviderStreamEvent};
use mcode_providers::{ReqwestTransport, ResolvedProvider, WireProvider};
use mcode_session::session::{
    self, BranchId, EventKind, HeadStamp, SessionError, SessionId, SessionService,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::view_model::{
    ActiveConversation, ConversationEntry, EntryKind, SessionSummary, project_entry,
};

/// Bound for outstanding bridge commands.
const COMMAND_QUEUE: usize = 256;
/// Bound for streaming chat events buffered toward the UI.
const EVENT_QUEUE: usize = 512;

/// A request from the UI to the core thread.
#[derive(Debug)]
pub enum BridgeCommand {
    /// Refresh the sidebar session list.
    ListSessions,
    /// Create and open a fresh session.
    CreateSession,
    /// Recover and open one session's root conversation.
    OpenSession(SessionId),
    /// Commit one user message on the open branch.
    SendMessage {
        /// Target session.
        session: SessionId,
        /// Target branch.
        branch: BranchId,
        /// Head the UI observed.
        expected_head: HeadStamp,
        /// Message text; nonempty and bounded by the view-model.
        text: String,
    },
    /// Load the settings document with its revision and stored key ids.
    LoadSettings,
    /// Persist new settings under revision compare-and-swap.
    SaveSettings {
        /// The revision the editor loaded.
        expected_revision: AuthorityRevision,
        /// The complete replacement settings.
        settings: AppSettings,
    },
    /// Store or clear one provider API key in the secret store.
    SaveProviderKey {
        /// Provider identity from settings.
        provider_id: String,
        /// The key; empty clears the stored entry.
        api_key: String,
    },
    /// Run one model turn over stored history and stream the reply.
    ChatTurn {
        /// Target session.
        session: SessionId,
        /// Target branch.
        branch: BranchId,
        /// Head observed after the user message commit.
        expected_head: HeadStamp,
        /// Provider identity from settings.
        provider_id: String,
        /// Model id offered by that provider.
        model: String,
        /// Conversation history including the committed user message.
        history: Vec<Message>,
    },
}

/// A streaming event from an active model turn.
#[derive(Debug, Clone)]
pub enum BridgeEvent {
    /// Incremental assistant text.
    ChatText {
        /// Session identity spelling.
        session_id: String,
        /// Text fragment.
        delta: String,
    },
    /// Incremental assistant reasoning.
    ChatThinking {
        /// Session identity spelling.
        session_id: String,
        /// Reasoning fragment.
        delta: String,
    },
    /// The turn finished and its assistant message was committed.
    ChatDone {
        /// Session identity spelling.
        session_id: String,
        /// New branch head spelling.
        head: String,
        /// Committed assistant entry projection.
        entry: ConversationEntry,
    },
    /// The turn failed; nothing was committed.
    ChatFailed {
        /// Session identity spelling.
        session_id: String,
        /// Rendered failure for the banner.
        message: String,
    },
}

/// A reply from the core thread, already projected for the view-model.
#[derive(Debug)]
pub enum BridgeReply {
    /// Session list result.
    Sessions(Result<Vec<SessionSummary>, String>),
    /// Create result (session ids).
    Created(Result<SessionSummary, String>),
    /// Open result (conversation projection).
    Conversation(Result<ActiveConversation, String>),
    /// Send result (new head plus committed entry).
    Sent(Result<(String, ConversationEntry), String>),
    /// Settings load result: document, revision, provider ids with keys.
    Settings(Result<(AppSettings, AuthorityRevision, Vec<String>), String>),
    /// Settings save result: the new revision.
    SettingsSaved(Result<AuthorityRevision, String>),
    /// Provider key save result.
    ProviderKeySaved(Result<(), String>),
    /// Chat turn acceptance; streaming continues over the event channel.
    ChatStarted(Result<(), String>),
}

/// Handle to the core thread.
pub struct CoreBridge {
    command_tx: Option<mpsc::SyncSender<WithReply>>,
    worker: Option<JoinHandle<()>>,
}

impl CoreBridge {
    /// Starts the core thread over one owned home.
    ///
    /// Returns the bridge handle together with the receiving end of the
    /// streaming event channel.
    #[must_use]
    pub fn start(home: HomeLayout) -> (Self, mpsc::Receiver<BridgeEvent>) {
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_QUEUE);
        let (event_tx, event_rx) = mpsc::sync_channel(EVENT_QUEUE);
        let worker = std::thread::Builder::new()
            .name("mcode-core".into())
            .spawn(move || run_core(home, command_rx, event_tx))
            .expect("core bridge thread");
        (
            Self {
                command_tx: Some(command_tx),
                worker: Some(worker),
            },
            event_rx,
        )
    }

    /// Sends one command and returns the reply future.
    ///
    /// Dropping the returned future cancels the wait but never cancels the
    /// durable core effect.
    ///
    /// # Panics
    ///
    /// Panics after [`CoreBridge::shutdown`].
    pub fn request(
        &self,
        command: BridgeCommand,
    ) -> impl Future<Output = BridgeReply> + Send + 'static {
        let (reply_tx, reply_rx) = oneshot::channel();
        let sender = self
            .command_tx
            .as_ref()
            .expect("core bridge already shut down");
        let _ = sender.send(command.with_reply(reply_tx));
        async move {
            match reply_rx.await {
                Ok(reply) => reply,
                Err(_) => BridgeReply::Sessions(Err("core thread stopped".to_owned())),
            }
        }
    }

    /// Stops the core thread and waits for it to finish.
    pub fn shutdown(&mut self) {
        // Dropping the sender closes the command loop; SessionService
        // shutdown runs inside the thread before it exits.
        self.command_tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl BridgeCommand {
    fn with_reply(self, reply: oneshot::Sender<BridgeReply>) -> WithReply {
        WithReply {
            command: self,
            reply,
        }
    }
}

struct WithReply {
    command: BridgeCommand,
    reply: oneshot::Sender<BridgeReply>,
}

fn run_core(
    home: HomeLayout,
    commands: mpsc::Receiver<WithReply>,
    events: mpsc::SyncSender<BridgeEvent>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            // Fail every pending request and stop; the UI surfaces the loss.
            for with_reply in commands.try_iter() {
                let _ = with_reply
                    .reply
                    .send(error_reply(&with_reply.command, "core runtime unavailable"));
            }
            return;
        }
    };
    runtime.block_on(async move {
        let service = SessionService::new(&home);
        while let Ok(with_reply) = commands.recv() {
            match with_reply.command {
                BridgeCommand::ChatTurn {
                    session,
                    branch,
                    expected_head,
                    provider_id,
                    model,
                    history,
                } => {
                    let task = chat_turn(
                        service.clone(),
                        home.clone(),
                        events.clone(),
                        session,
                        branch,
                        expected_head,
                        provider_id,
                        model,
                        history,
                    );
                    tokio::spawn(task);
                    let _ = with_reply.reply.send(BridgeReply::ChatStarted(Ok(())));
                }
                command => {
                    let outcome = handle(&service, &home, &command).await;
                    if with_reply.reply.send(outcome).is_err() {
                        continue;
                    }
                }
            }
        }
        service.shutdown().await;
    });
}

fn error_reply(command: &BridgeCommand, message: &str) -> BridgeReply {
    let message = message.to_owned();
    match command {
        BridgeCommand::ListSessions => BridgeReply::Sessions(Err(message)),
        BridgeCommand::CreateSession => BridgeReply::Created(Err(message)),
        BridgeCommand::OpenSession(_) => BridgeReply::Conversation(Err(message)),
        BridgeCommand::SendMessage { .. } => BridgeReply::Sent(Err(message)),
        BridgeCommand::LoadSettings => BridgeReply::Settings(Err(message)),
        BridgeCommand::SaveSettings { .. } => BridgeReply::SettingsSaved(Err(message)),
        BridgeCommand::SaveProviderKey { .. } => BridgeReply::ProviderKeySaved(Err(message)),
        BridgeCommand::ChatTurn { .. } => BridgeReply::ChatStarted(Err(message)),
    }
}

async fn handle(
    service: &SessionService,
    home: &HomeLayout,
    command: &BridgeCommand,
) -> BridgeReply {
    match command {
        BridgeCommand::ListSessions => {
            BridgeReply::Sessions(inspect_summaries(home).map_err(render_error))
        }
        BridgeCommand::CreateSession => match service.create().await {
            Ok(created) => BridgeReply::Created(Ok(SessionSummary {
                session_id: created.session_id.as_str().to_owned(),
                root_branch_id: created.branch_id.as_str().to_owned(),
                event_count: 0,
                active: true,
            })),
            Err(error) => BridgeReply::Created(Err(render_error(error))),
        },
        BridgeCommand::OpenSession(session) => BridgeReply::Conversation(
            open_conversation(service, session)
                .await
                .map_err(render_error),
        ),
        BridgeCommand::SendMessage {
            session,
            branch,
            expected_head,
            text,
        } => BridgeReply::Sent(
            send_message(service, session, branch, expected_head, text)
                .await
                .map_err(render_error),
        ),
        BridgeCommand::LoadSettings => BridgeReply::Settings(load_settings(home)),
        BridgeCommand::SaveSettings {
            expected_revision,
            settings,
        } => BridgeReply::SettingsSaved(save_settings(home, *expected_revision, settings)),
        BridgeCommand::SaveProviderKey {
            provider_id,
            api_key,
        } => BridgeReply::ProviderKeySaved(save_provider_key(home, provider_id, api_key)),
        BridgeCommand::ChatTurn { .. } => {
            BridgeReply::ChatStarted(Err("chat turns run as concurrent tasks".to_owned()))
        }
    }
}

fn load_settings(
    home: &HomeLayout,
) -> Result<(AppSettings, AuthorityRevision, Vec<String>), String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let revision = mcode_config::read_owned_file(
        home,
        mcode_config::SETTINGS_PATH,
        mcode_config::MAX_SETTINGS_BYTES,
    )
    .map_err(|error| render_config_error(&error))?
    .map(|bytes| settings_revision(bytes.as_slice()))
    .transpose()
    .map_err(|()| "stored settings failed validation".to_owned())?
    .unwrap_or(AuthorityRevision::ABSENT);
    let key_ids = read_provider_secrets(home)
        .map_err(|error| render_config_error(&error))?
        .provider_ids()
        .into_iter()
        .map(str::to_owned)
        .collect();
    Ok((settings, revision, key_ids))
}

fn settings_revision(bytes: &[u8]) -> Result<AuthorityRevision, ()> {
    #[derive(serde::Deserialize)]
    struct Header {
        #[serde(rename = "formatVersion")]
        format_version: u32,
        revision: u64,
    }
    let header: Header = serde_json::from_slice(bytes).map_err(|_| ())?;
    if header.format_version != mcode_config::SETTINGS_FORMAT_VERSION {
        return Err(());
    }
    AuthorityRevision::new(header.revision).map_err(|_| ())
}

fn save_settings(
    home: &HomeLayout,
    expected_revision: AuthorityRevision,
    settings: &AppSettings,
) -> Result<AuthorityRevision, String> {
    replace_app_settings(home, expected_revision, settings)
        .map_err(|error| render_config_error(&error))
}

fn render_config_error(error: &mcode_config::ConfigError) -> String {
    format!("settings error: {error}")
}

fn inspect_summaries(home: &HomeLayout) -> Result<Vec<SessionSummary>, SessionError> {
    Ok(session::inspect_sessions(home)?
        .into_iter()
        .map(|snapshot| SessionSummary {
            root_branch_id: snapshot
                .branches
                .first()
                .map(|branch| branch.branch_id.as_str().to_owned())
                .unwrap_or_default(),
            event_count: snapshot.branches.iter().map(|b| b.event_count).sum(),
            session_id: snapshot.session_id.as_str().to_owned(),
            active: false,
        })
        .collect())
}

async fn open_conversation(
    service: &SessionService,
    session: &SessionId,
) -> Result<ActiveConversation, SessionError> {
    let opened = service.open(session).await?;
    let root = opened
        .heads
        .iter()
        .min_by_key(|head| head.branch_id.as_str())
        .ok_or(SessionError::NotFound)?;
    let branch_id = root.branch_id.clone();
    let snapshot_head = root.head.clone();
    let mut entries = Vec::new();
    let mut after: Option<mcode_session::session::SessionEventId> = None;
    loop {
        let page = service
            .read(session, &branch_id, &snapshot_head, after.as_ref(), 256)
            .await?;
        if page.items.is_empty() {
            break;
        }
        let last = page.items.last().expect("nonempty page").event_id.clone();
        for event in &page.items {
            let loaded = service
                .load_event(session, &branch_id, &event.event_id)
                .await?;
            entries.push(project_entry(event, decode_text(&loaded.payload)));
        }
        match page.next {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
        if after.as_ref() == Some(&last) {
            // Defensive: a cursor equal to the last returned event would loop.
            break;
        }
    }
    Ok(ActiveConversation {
        session_id: session.as_str().to_owned(),
        branch_id: branch_id.as_str().to_owned(),
        head: head_spelling(&snapshot_head),
        entries,
        streaming: None,
    })
}

async fn send_message(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: &HeadStamp,
    text: &str,
) -> Result<(String, ConversationEntry), SessionError> {
    let payload = text.as_bytes().to_vec();
    let reservation = service
        .reserve_event(session, branch, EventKind::Message, None, &payload)
        .await?;
    let appended = service
        .append(session, branch, expected_head, &reservation)
        .await?;
    let event_id = appended
        .head
        .event()
        .cloned()
        .ok_or(SessionError::Corrupt)?;
    Ok((
        event_id.as_str().to_owned(),
        ConversationEntry {
            event_id: event_id.as_str().to_owned(),
            kind: EntryKind::UserMessage,
            text: text.to_owned(),
            call_id: None,
        },
    ))
}

fn head_spelling(head: &HeadStamp) -> String {
    match head {
        HeadStamp::Empty => "empty".to_owned(),
        HeadStamp::Event(event) => event.as_str().to_owned(),
    }
}

fn decode_text(payload: &[u8]) -> String {
    match std::str::from_utf8(payload) {
        Ok(text) => text.to_owned(),
        Err(_) => format!("(binary payload, {} bytes)", payload.len()),
    }
}

fn render_error(error: SessionError) -> String {
    match error {
        SessionError::InvalidArgument => "invalid request".to_owned(),
        SessionError::NotFound => "not found".to_owned(),
        SessionError::Conflict(_) => "the session moved on; reopen it".to_owned(),
        SessionError::Corrupt => "stored data failed validation".to_owned(),
        SessionError::Limit => "a fixed bound was reached".to_owned(),
        SessionError::Cancelled => "cancelled".to_owned(),
        SessionError::Unavailable => "the session service is unavailable".to_owned(),
    }
}

/// Stores or clears one provider key under the secret-store CAS.
fn save_provider_key(home: &HomeLayout, provider_id: &str, api_key: &str) -> Result<(), String> {
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
    let expected = mcode_config::read_owned_file(
        home,
        mcode_config::SECRETS_PATH,
        mcode_config::MAX_SECRETS_BYTES,
    )
    .map_err(|error| render_config_error(&error))?
    .map(|bytes| secrets_revision(bytes.as_slice()))
    .transpose()
    .map_err(|()| "stored secrets failed validation".to_owned())?
    .unwrap_or(AuthorityRevision::ABSENT);
    let updated = secrets.with_key(provider_id, (!api_key.is_empty()).then_some(api_key));
    replace_provider_secrets(home, expected, &updated)
        .map_err(|error| render_config_error(&error))?;
    Ok(())
}

fn secrets_revision(bytes: &[u8]) -> Result<AuthorityRevision, ()> {
    #[derive(serde::Deserialize)]
    struct Header {
        #[serde(rename = "formatVersion")]
        format_version: u32,
        revision: u64,
    }
    let header: Header = serde_json::from_slice(bytes).map_err(|_| ())?;
    if header.format_version != mcode_config::SECRETS_FORMAT_VERSION {
        return Err(());
    }
    AuthorityRevision::new(header.revision).map_err(|_| ())
}

/// One model turn: resolve the provider, stream the reply into the event
/// channel, and commit the assistant message to the session ledger.
#[allow(clippy::too_many_arguments)]
async fn chat_turn(
    service: SessionService,
    home: HomeLayout,
    events: mpsc::SyncSender<BridgeEvent>,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: String,
    model: String,
    history: Vec<Message>,
) {
    let session_id = session.as_str().to_owned();
    if let Err(message) = run_chat_turn(
        &service,
        &home,
        &events,
        &session_id,
        session,
        branch,
        expected_head,
        &provider_id,
        &model,
        &history,
    )
    .await
    {
        let _ = events.send(BridgeEvent::ChatFailed {
            session_id,
            message,
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_chat_turn(
    service: &SessionService,
    home: &HomeLayout,
    events: &mpsc::SyncSender<BridgeEvent>,
    session_id: &str,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: &str,
    model: &str,
    history: &[Message],
) -> Result<(), String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let provider = settings
        .providers
        .iter()
        .find(|provider| provider.id == provider_id && provider.enabled)
        .ok_or_else(|| "provider not found or disabled in settings".to_owned())?;
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
    let api_key = secrets
        .key(provider_id)
        .ok_or_else(|| "provider API key is not set".to_owned())?
        .to_owned();
    let resolved =
        ResolvedProvider::resolve(provider, model, &api_key, &settings.effective_user_agent())
            .map_err(|error| format!("provider setup failed: {error:?}"))?;
    let transport = ReqwestTransport::new().map_err(|_| "HTTP transport unavailable".to_owned())?;
    let wire = WireProvider::new(resolved, std::sync::Arc::new(transport));

    let request = mcode_provider_api::Request {
        system_prompt: Vec::new(),
        messages: history.to_vec(),
        tools: Vec::new(),
    };
    let cancel = CancellationToken::new();
    let mut stream = wire
        .stream(&request, cancel)
        .await
        .map_err(|error| format!("provider request failed: {error:?}"))?;
    while let Some(event) = stream.next().await {
        match event {
            ProviderStreamEvent::TextDelta(delta) => {
                let _ = events.send(BridgeEvent::ChatText {
                    session_id: session_id.to_owned(),
                    delta,
                });
            }
            ProviderStreamEvent::ThinkingDelta(delta) => {
                let _ = events.send(BridgeEvent::ChatThinking {
                    session_id: session_id.to_owned(),
                    delta,
                });
            }
            ProviderStreamEvent::ToolCallDelta { .. } => {}
            ProviderStreamEvent::Done { message } => {
                let payload = serde_json::to_vec(&message)
                    .map_err(|_| "assistant message could not be encoded".to_owned())?;
                let (head, entry) =
                    append_assistant(service, &session, &branch, expected_head, &payload)
                        .await
                        .map_err(render_error)?;
                let _ = events.send(BridgeEvent::ChatDone {
                    session_id: session_id.to_owned(),
                    head,
                    entry,
                });
                return Ok(());
            }
            ProviderStreamEvent::Error(error) => {
                return Err(format!(
                    "provider stream failed: {}",
                    error.message().unwrap_or("unknown provider error")
                ));
            }
        }
    }
    Err("provider stream ended without a terminal".to_owned())
}

/// Commits one assistant message payload and projects its entry.
async fn append_assistant(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    expected_head: HeadStamp,
    payload: &[u8],
) -> Result<(String, ConversationEntry), SessionError> {
    let reservation = service
        .reserve_event(session, branch, EventKind::Message, None, payload)
        .await?;
    let appended = service
        .append(session, branch, &expected_head, &reservation)
        .await?;
    let event_id = appended
        .head
        .event()
        .cloned()
        .ok_or(SessionError::Corrupt)?;
    let entry = project_assistant(&event_id, payload);
    Ok((event_id.as_str().to_owned(), entry))
}

/// Projects a serialized assistant payload into a display entry.
fn project_assistant(
    event_id: &mcode_session::session::SessionEventId,
    payload: &[u8],
) -> ConversationEntry {
    let mut text = String::new();
    if let Ok(message) = serde_json::from_slice::<mcode_core::AssistantMessage>(payload) {
        for block in &message.blocks {
            match block {
                mcode_core::ContentBlock::Text(block) => text.push_str(&block.text),
                mcode_core::ContentBlock::Thinking(_) => {}
                mcode_core::ContentBlock::ToolCall(call) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("tool call {}", call.name));
                }
                mcode_core::ContentBlock::Image(_) => {}
            }
        }
    }
    ConversationEntry {
        event_id: event_id.as_str().to_owned(),
        kind: EntryKind::AssistantMessage,
        text,
        call_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcode_session::session::EventKind;

    fn home() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    fn drive(bridge: &CoreBridge, command: BridgeCommand) -> BridgeReply {
        futures_executor_block(bridge.request(command))
    }

    fn futures_executor_block<F: Future>(future: F) -> F::Output {
        // tokio oneshot receivers poll fine on a minimal executor.
        let mut future = Box::pin(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        loop {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(output) => return output,
                std::task::Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn bridge_round_trips_sessions_messages_and_plugins() {
        let (_parent, layout) = home();
        let (mut bridge, _events) = CoreBridge::start(layout.clone());

        let BridgeReply::Sessions(list) = drive(&bridge, BridgeCommand::ListSessions) else {
            panic!("sessions reply");
        };
        assert!(list.expect("empty list").is_empty());

        let BridgeReply::Created(created) = drive(&bridge, BridgeCommand::CreateSession) else {
            panic!("created reply");
        };
        let summary = created.expect("created");
        let session_id = SessionId::parse(&summary.session_id).expect("session id");
        let branch_id = BranchId::parse(&summary.root_branch_id).expect("branch id");

        let BridgeReply::Sent(sent) = drive(
            &bridge,
            BridgeCommand::SendMessage {
                session: session_id.clone(),
                branch: branch_id,
                expected_head: HeadStamp::Empty,
                text: "hello core".to_owned(),
            },
        ) else {
            panic!("sent reply");
        };
        let (head, entry) = sent.expect("sent");
        assert_eq!(entry.kind, EntryKind::UserMessage);

        let BridgeReply::Conversation(conversation) =
            drive(&bridge, BridgeCommand::OpenSession(session_id))
        else {
            panic!("conversation reply");
        };
        let conversation = conversation.expect("conversation");
        assert_eq!(conversation.entries.len(), 1);
        assert_eq!(conversation.entries[0].text, "hello core");
        assert_eq!(conversation.head, head);

        let BridgeReply::Settings(settings) = drive(&bridge, BridgeCommand::LoadSettings) else {
            panic!("settings reply");
        };
        let (document, revision, key_ids) = settings.expect("settings load");
        assert!(key_ids.is_empty());
        assert_eq!(revision, AuthorityRevision::ABSENT);
        assert!(document.providers.is_empty());
        assert!(document.effective_user_agent().starts_with("pi ("));

        let BridgeReply::SettingsSaved(saved) = drive(
            &bridge,
            BridgeCommand::SaveSettings {
                expected_revision: revision,
                settings: AppSettings {
                    user_agent: "mcode-desktop-test/1".to_owned(),
                    ..AppSettings::default()
                },
            },
        ) else {
            panic!("saved reply");
        };
        assert_eq!(saved.expect("saved").get(), 1);

        let BridgeReply::ProviderKeySaved(saved) = drive(
            &bridge,
            BridgeCommand::SaveProviderKey {
                provider_id: "openai-main".to_owned(),
                api_key: "sk-test".to_owned(),
            },
        ) else {
            panic!("key reply");
        };
        saved.expect("key saved");

        let BridgeReply::Settings(reloaded) = drive(&bridge, BridgeCommand::LoadSettings) else {
            panic!("reloaded reply");
        };
        let (document, revision, key_ids) = reloaded.expect("settings reload");
        assert_eq!(key_ids, vec!["openai-main".to_owned()]);
        assert_eq!(document.user_agent, "mcode-desktop-test/1");
        assert_eq!(revision.get(), 1);

        bridge.shutdown();
        let _ = EventKind::Message;
    }

    #[test]
    fn head_spelling_and_text_decode_are_lossy_safe() {
        assert_eq!(head_spelling(&HeadStamp::Empty), "empty");
        assert_eq!(decode_text(b"abc"), "abc");
        assert_eq!(decode_text(&[0xff, 0xfe]), "(binary payload, 2 bytes)");
    }
}
