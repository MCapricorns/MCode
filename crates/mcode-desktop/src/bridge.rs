//! Core bridge: one background thread owning the tokio-backed core services.
//!
//! GPUI runs its own executor, so the desktop never touches tokio types
//! directly. [`CoreBridge`] owns a dedicated thread with a current-thread
//! tokio runtime hosting the [`SessionService`]; the UI sends
//! [`BridgeCommand`]s and awaits [`BridgeReply`]s through tokio oneshot
//! channels, whose receivers are executor-agnostic futures. Configuration
//! reads stay synchronous on the same thread.
use std::sync::mpsc;
use std::thread::JoinHandle;

use mcode_config::{
    AppSettings, AuthorityRevision, HomeLayout, read_app_settings, replace_app_settings,
};
use mcode_plugin_host::session::{
    self, BranchId, EventKind, HeadStamp, SessionError, SessionId, SessionService,
};
use tokio::sync::oneshot;

use super::view_model::{
    ActiveConversation, ConversationEntry, EntryKind, SessionSummary, project_entry,
};

/// Bound for outstanding bridge commands.
const COMMAND_QUEUE: usize = 256;

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
    /// Load the settings document with its revision.
    LoadSettings,
    /// Persist new settings under revision compare-and-swap.
    SaveSettings {
        /// The revision the editor loaded.
        expected_revision: AuthorityRevision,
        /// The complete replacement settings.
        settings: AppSettings,
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
    /// Settings load result: document plus revision.
    Settings(Result<(AppSettings, AuthorityRevision), String>),
    /// Settings save result: the new revision.
    SettingsSaved(Result<AuthorityRevision, String>),
}

/// Handle to the core thread.
pub struct CoreBridge {
    command_tx: Option<mpsc::SyncSender<WithReply>>,
    worker: Option<JoinHandle<()>>,
}

impl CoreBridge {
    /// Starts the core thread over one owned home.
    #[must_use]
    pub fn start(home: HomeLayout) -> Self {
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_QUEUE);
        let worker = std::thread::Builder::new()
            .name("mcode-core".into())
            .spawn(move || run_core(home, command_rx))
            .expect("core bridge thread");
        Self {
            command_tx: Some(command_tx),
            worker: Some(worker),
        }
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

fn run_core(home: HomeLayout, commands: mpsc::Receiver<WithReply>) {
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
            let outcome = handle(&service, &home, &with_reply.command).await;
            if with_reply.reply.send(outcome).is_err() {
                continue;
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
    }
}

fn load_settings(home: &HomeLayout) -> Result<(AppSettings, AuthorityRevision), String> {
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
    Ok((settings, revision))
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
    let mut after: Option<mcode_plugin_host::session::SessionEventId> = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use mcode_plugin_host::session::EventKind;

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
        let mut bridge = CoreBridge::start(layout.clone());

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
        let (document, revision) = settings.expect("settings load");
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

        let BridgeReply::Settings(reloaded) = drive(&bridge, BridgeCommand::LoadSettings) else {
            panic!("reloaded reply");
        };
        let (document, revision) = reloaded.expect("settings reload");
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
