//! `mycode-app` — the application core, with no frontend dependency.
//!
//! Everything a MYCode frontend needs to drive is here: sessions, model
//! turns, tools and their hosts, MCP servers, web search, provider
//! credentials, self-update, and the durable settings. A frontend owns none
//! of it. It starts a [`CoreBridge`], sends [`BridgeCommand`]s, awaits
//! [`BridgeReply`]s, and renders the [`BridgeEvent`] stream; the values in
//! those messages are defined in [`protocol`].
//!
//! [`CoreBridge`] owns a dedicated thread with a current-thread tokio runtime
//! hosting the session service, so a frontend never touches tokio types and
//! its own executor stays free. Replies ride tokio oneshot channels, whose
//! receivers are executor-agnostic futures. Model turns run as concurrent
//! runtime tasks streaming events back through a channel the frontend polls;
//! configuration reads stay synchronous on the same thread.
//!
//! The command channel is unbounded on purpose: the worker loop must `await`
//! commands instead of blocking the runtime thread, or spawned tasks (chat
//! turns, catalog refreshes) would starve until the next command arrives.
//! `UnboundedSender::send` is synchronous, so the caller never touches async
//! machinery.

mod compaction;
mod export;
mod mcp_client;
mod mcp_tools;
pub mod protocol;
mod subagent;
mod updates;
mod web_client;

pub use export::{ExportSummary, ImportSummary};
pub use protocol::{
    ActiveConversation, BranchId, CHAT_CANCELLED, ConversationEntry, EntryKind, HeadStamp,
    MAX_STREAMING_CHARS, SessionEventId, SessionId, SessionSummary, StreamingReply,
};
pub use updates::{apply_and_restart, cleanup_stale_stages, current_version};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;

pub use crate::updates::{PreparedUpdate, UpdateOffer};
use mycode_agent::session::{self, EventKind, SessionCallId, SessionError, SessionService};
use mycode_agent::{Agent, AgentConfig, HookRunner};
use mycode_config::{
    AppSettings, AuthorityRevision, HomeLayout, ProviderSettings, UiState, read_app_settings,
    read_provider_secrets, read_ui_state, replace_app_settings, replace_provider_secrets,
    replace_ui_state,
};
use mycode_core::Message;
use mycode_providers::catalog::{
    CachedCatalog, CatalogDocument, DEFAULT_MAX_AGE_SECS, RefreshOutcome, http_client,
};
use mycode_providers::{
    COPILOT_PROVIDER_ID, DeviceTokenPoll, ReqwestTransport, ResolvedProvider, SseTransport,
    WireProvider, copilot_bearer, poll_device_token, start_device_flow,
};
use mycode_tools::ToolDyn as _;
use mycode_tools::ToolRegistry;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

// The event channel is unbounded on purpose: `Sender::send` never blocks,
// so a slow or stalled UI frame can never freeze the single-threaded core
// runtime mid-turn. The UI drains with `try_recv` on a fixed poll tick and
// already collapses the queue per pass, so the unbounded queue only ever
// holds at most one interval's worth of events.

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
    /// Start the GitHub Copilot OAuth device-flow sign-in.
    StartCopilotSignIn {
        /// Model ids to bind to the provider once the sign-in succeeds.
        models: Vec<String>,
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
    },
    /// Abort the in-flight turn of one session (Escape in the chat).
    CancelChat {
        /// Session identity spelling.
        session_id: String,
    },
    /// Run one bounded web search over the enabled backend.

    /// List project files matching the composer's `@` fragment.
    SearchProjectFiles {
        /// Session whose bound project is searched.
        session_id: String,
        /// Case-insensitive substring filter; empty lists the first files.
        query: String,
    },
    /// List tools exposed by one enabled MCP server.
    McpListTools {
        /// The server row to probe. The whole row travels, not just its id,
        /// so the settings form can test a binding before it is saved.
        server: Box<mycode_config::McpServerSettings>,
    },
    /// Roll every snapshotted file of one session back to its earliest state.
    RollbackWorkspace {
        /// Session identity spelling.
        session_id: String,
    },
    /// Rewind the branch to just before one user message (recall), with an
    /// optional edited text to re-send.
    RecallMessage {
        /// Session identity spelling.
        session: SessionId,
        /// Branch identity spelling.
        branch: BranchId,
        /// Head the UI believes the branch is at.
        expected_head: HeadStamp,
        /// Event to rewind to (the entry before the recalled message).
        to_event: String,
        /// Edited text to prefill for re-sending.
        edit: Option<String>,
    },
    /// Deletes one session's durable data (ledger, todos, checkpoints).
    DeleteSession {
        /// Session identity spelling.
        session_id: String,
    },
    /// Removes one directory from the remembered projects list.
    RemoveRecent {
        /// The project directory to forget.
        project: String,
    },
    /// Write one product-data export bundle to a user-chosen file.
    ExportData {
        /// Destination file chosen in a save dialog.
        path: PathBuf,
    },
    /// Apply one product-data export bundle from a user-chosen file.
    ImportData {
        /// Bundle file chosen in an open dialog.
        path: PathBuf,
    },
    /// List discovered prompt resources for the session workspace.
    ListResources {
        /// Session identity spelling (selects the workspace directory).
        session_id: String,
    },
    /// Deliver the user's answers to the pending ask of one session.
    AskAnswer {
        /// Session identity spelling.
        session_id: String,
        /// One answer per asked question, in order; empty string skips.
        answers: Vec<String>,
    },
    /// Resolve the current provider catalog (cache, else bundled snapshot).
    GetCatalog,
    /// Re-download the cloud provider catalog.
    RefreshCatalog,
    /// Load the durable UI state document.
    LoadUiState,
    /// Persist the durable UI state document.
    SaveUiState {
        /// The replacement state.
        state: UiState,
    },
    /// Bind one project directory to a session for tool runs.
    SetProjectDir {
        /// Session identity spelling.
        session_id: String,
        /// Existing directory; `None` reverts to the shared scratch directory.
        path: Option<String>,
    },
    /// Query GitHub for a newer desktop release.
    CheckUpdate,
    /// Download and verify one update offer into a staging directory.
    DownloadUpdate {
        /// The offer to download.
        offer: UpdateOffer,
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
    /// One intermediate assistant step was committed mid-turn.
    ///
    /// A turn that calls tools produces several assistant messages: each one
    /// requests tools, reads their results, and continues. Every step is
    /// committed as it arrives so the transcript and the ledger keep the
    /// model's actual order; only the closing step arrives as
    /// [`BridgeEvent::ChatDone`].
    AssistantStep {
        /// Session identity spelling.
        session_id: String,
        /// Committed assistant entry projection.
        entry: ConversationEntry,
    },
    /// The turn finished and its closing assistant message was committed.
    ChatDone {
        /// Session identity spelling.
        session_id: String,
        /// New branch head spelling.
        head: String,
        /// Committed assistant entry projection.
        entry: ConversationEntry,
    },
    /// Something the user should read that does not end the turn.
    ///
    /// History compaction and other background housekeeping report through
    /// here; the turn keeps running.
    Notice {
        /// Session identity spelling.
        session_id: String,
        /// One-line message.
        message: String,
    },
    /// A durable usage record was committed.
    UsageRecorded {
        /// Session identity spelling.
        session_id: String,
        /// Provider identity.
        provider: String,
        /// Model id.
        model: String,
        /// Input tokens.
        input: u64,
        /// Output tokens.
        output: u64,
        /// Prompt tokens served from the provider cache, when reported.
        cache: Option<u64>,
        /// Wall-clock turn duration in milliseconds.
        elapsed_ms: u64,
        /// Committed usage entry projection.
        entry: ConversationEntry,
    },
    /// The durable task list changed.
    TodoUpdated {
        /// Session identity spelling.
        session_id: String,
        /// (content, status) rows in list order.
        tasks: Vec<(String, String)>,
    },
    /// The agent asked the user structured questions.
    AskRequested {
        /// Session identity spelling.
        session_id: String,
        /// (question, choices, optional) rows.
        questions: Vec<(String, Vec<String>, bool)>,
    },
    /// A tool call started executing.
    ToolStarted {
        /// Session identity spelling.
        session_id: String,
        /// Provider-assigned call id.
        call_id: String,
        /// Tool name.
        name: String,
    },
    /// A tool call finished; its result is committed to the ledger.
    ToolCompleted {
        /// Session identity spelling.
        session_id: String,
        /// Committed tool-result entry projection.
        entry: ConversationEntry,
    },
    /// The turn failed; nothing was committed.
    ChatFailed {
        /// Session identity spelling.
        session_id: String,
        /// Rendered failure for the banner.
        message: String,
    },
    /// The provider catalog changed after a cloud refresh.
    CatalogUpdated {
        /// Provider count in the refreshed catalog.
        providers: usize,
        /// Unix seconds of the successful fetch.
        fetched_at: u64,
    },
    /// A newer desktop release is available.
    UpdateAvailable {
        /// The resolved release offer.
        offer: UpdateOffer,
    },
    /// The Copilot device-flow sign-in completed; the provider is ready.
    CopilotSignedIn,
    /// The Copilot device-flow sign-in failed or expired.
    CopilotSignInFailed {
        /// Rendered failure for the sign-in panel.
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
    Settings(Result<(AppSettings, AuthorityRevision, Vec<String>, Vec<String>), String>),
    /// Settings save result: the new revision.
    SettingsSaved(Result<AuthorityRevision, String>),
    /// Provider key save result: refreshed key-id lists (providers, MCP).
    ProviderKeySaved(Result<(Vec<String>, Vec<String>), String>),
    /// Chat turn acceptance; streaming continues over the event channel.
    ChatStarted(Result<(), String>),
    /// Chat cancel acceptance; the turn unwinds with a `cancelled` event.
    ChatCancelled(Result<(), String>),
    /// Web search result list.

    /// File matches for the composer's `@` mention.
    ProjectFiles(Result<Vec<String>, String>),
    /// MCP tools listing for one server. The server id rides the reply even
    /// on failure, so the settings page can attribute the error to its row.
    McpTools {
        /// Server identity the probe ran against.
        server_id: String,
        /// Listed tool names, or why the probe failed.
        outcome: Result<Vec<String>, String>,
    },
    /// Rollback outcome: restored absolute paths.
    RolledBack(Result<Vec<String>, String>),
    Recalled(Result<(Box<ActiveConversation>, Option<String>), String>),
    SessionDeleted(Result<(), String>),
    Exported(Result<crate::export::ExportSummary, String>),
    Imported(Result<crate::export::ImportSummary, String>),
    /// Resource list: (name, absolute path) pairs.
    Resources(Result<Vec<(String, String)>, String>),
    /// The user's answers were delivered to the waiting tool.
    AskAnswered(Result<(), String>),
    /// Provider catalog snapshot with its freshness metadata.
    Catalog(Result<CatalogInfo, String>),
    /// Durable UI state load result.
    UiState(Result<UiState, String>),
    /// UI state persist result.
    UiStateSaved(Result<(), String>),
    /// Project directory bind result.
    ProjectSet(Result<(), String>),
    /// Update check result; `Ok(None)` means the app is current.
    UpdateChecked(Result<Option<UpdateOffer>, String>),
    /// Download-and-verify result.
    UpdateDownloaded(Result<PreparedUpdate, String>),
    /// The device flow started; the user code is on screen.
    CopilotSignInStarted(Result<CopilotSignInInfo, String>),
}

/// What the user needs to complete a device-flow sign-in.
#[derive(Clone, Debug)]
pub struct CopilotSignInInfo {
    /// Code the user types at the verification page.
    pub user_code: String,
    /// Verification page opened in the browser.
    pub verification_uri: String,
}

/// The resolved provider catalog shared with the UI.
#[derive(Clone, Debug)]
pub struct CatalogInfo {
    /// The catalog document.
    pub document: Arc<CatalogDocument>,
    /// Unix seconds of the successful cloud fetch; 0 for the baseline.
    pub fetched_at: u64,
}

impl CatalogInfo {
    /// Human-readable source description.
    #[must_use]
    pub fn source_label(&self) -> &'static str {
        if self.fetched_at > 0 {
            "cloud catalog"
        } else {
            "bundled snapshot"
        }
    }
}

/// Handle to the core thread.
pub struct CoreBridge {
    command_tx: Option<tokio::sync::mpsc::UnboundedSender<WithReply>>,
    worker: Option<JoinHandle<()>>,
}

impl CoreBridge {
    /// Starts the core thread over one owned home.
    ///
    /// Returns the bridge handle together with the receiving end of the
    /// streaming event channel.
    #[must_use]
    pub fn start(home: HomeLayout) -> (Self, mpsc::Receiver<BridgeEvent>) {
        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("mycode-core".into())
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
        let sender = self.command_tx.clone();
        async move {
            // An unbounded send only fails when the worker stopped; surface
            // that as the generic loss reply instead of panicking here.
            let sender = match sender {
                Some(sender) => sender,
                None => return BridgeReply::Sessions(Err("core bridge stopped".to_owned())),
            };
            if sender.send(command.with_reply(reply_tx)).is_err() {
                return BridgeReply::Sessions(Err("core thread stopped".to_owned()));
            }
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

/// Routes user answers from the UI to the pending `ask_user` tool.
#[derive(Default)]
pub struct AskRouter {
    pending: std::sync::Mutex<
        std::collections::HashMap<String, tokio::sync::oneshot::Sender<Vec<String>>>,
    >,
}

impl AskRouter {
    fn register(&self, session_id: &str) -> tokio::sync::oneshot::Receiver<Vec<String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .expect("ask router")
            .insert(session_id.to_owned(), tx);
        rx
    }

    fn deliver(&self, session_id: &str, answers: Vec<String>) -> bool {
        self.pending
            .lock()
            .expect("ask router")
            .remove(session_id)
            .is_some_and(|tx| tx.send(answers).is_ok())
    }
}

fn deliver_ask_answer(session_id: &str, answers: Vec<String>) -> Result<(), String> {
    let router = ASK_ROUTER.get_or_init(AskRouter::default);
    if router.deliver(session_id, answers) {
        Ok(())
    } else {
        Err("no pending question for this session".to_owned())
    }
}

/// Process-wide ask router; one pending ask per session.
static ASK_ROUTER: std::sync::OnceLock<AskRouter> = std::sync::OnceLock::new();

fn run_core(
    home: HomeLayout,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<WithReply>,
    events: mpsc::Sender<BridgeEvent>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            // Fail every pending request and stop; the UI surfaces the loss.
            while let Some(with_reply) = commands.blocking_recv() {
                let _ = with_reply
                    .reply
                    .send(error_reply(&with_reply.command, "core runtime unavailable"));
            }
            return;
        }
    };
    runtime.block_on(async move {
        // The state lives behind one Arc: `SessionService` clones share the
        // actor and fence, and only the last drop retires the publication,
        // so per-turn tasks may hold clones freely.
        // Subagent worktree leases from a crashed process are recovered
        // before any new turn can run; the git subprocess calls are blocking,
        // so they run off the core thread.
        let recovery_home = home.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::subagent::recover_task_worktrees(&recovery_home)
        })
        .await;
        // The catalog cache is a multi-megabyte document; parse it off the
        // core thread so startup does not stall the command loop.
        let cached = tokio::task::spawn_blocking({
            let home = home.clone();
            move || mycode_providers::catalog::current(&home)
        })
        .await
        .unwrap_or_else(|_| mycode_providers::catalog::CachedCatalog {
            document: mycode_providers::catalog::bundled().clone(),
            fetched_at: 0,
            etag: None,
        });
        // `CoreState::new` reads the durable UI state and starts the session
        // actor (a blocking startup handshake); build it off the core thread.
        let state_home = home.clone();
        let Ok(state) =
            tokio::task::spawn_blocking(move || CoreState::new(state_home, cached)).await
        else {
            eprintln!("mycode-desktop: core state failed to initialize");
            return;
        };
        let state = Arc::new(state);
        spawn_catalog_refresh(state.clone(), events.clone());
        spawn_update_check(state.clone(), events.clone());
        while let Some(with_reply) = commands.recv().await {
            match with_reply.command {
                BridgeCommand::ChatTurn {
                    session,
                    branch,
                    expected_head,
                    provider_id,
                    model,
                } => {
                    let task = chat_turn(
                        state.clone(),
                        events.clone(),
                        session,
                        branch,
                        expected_head,
                        provider_id,
                        model,
                    );
                    tokio::spawn(task);
                    let _ = with_reply.reply.send(BridgeReply::ChatStarted(Ok(())));
                }
                BridgeCommand::CancelChat { session_id } => {
                    let reply = with_reply.reply;
                    let cancels = state.turn_cancels.clone();
                    tokio::spawn(async move {
                        let outcome = match cancels.lock() {
                            Ok(map) => match map.get(&session_id) {
                                Some(token) => {
                                    token.cancel();
                                    Ok(())
                                }
                                None => Err("no turn is running for this session".to_owned()),
                            },
                            Err(_) => Err("cancel registry locked".to_owned()),
                        };
                        let _ = reply.send(BridgeReply::ChatCancelled(outcome));
                    });
                }
                BridgeCommand::RefreshCatalog => {
                    let task_state = state.clone();
                    let task_events = events.clone();
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome = refresh_catalog(&task_state, &task_events, true).await;
                        let _ = reply.send(outcome);
                    });
                }
                BridgeCommand::CheckUpdate => {
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let client = match http_client(UPDATE_USER_AGENT) {
                            Ok(client) => client,
                            Err(message) => {
                                let _ = reply.send(BridgeReply::UpdateChecked(Err(message)));
                                return;
                            }
                        };
                        let _ = reply.send(BridgeReply::UpdateChecked(
                            crate::updates::latest_release(&client).await,
                        ));
                    });
                }
                BridgeCommand::DownloadUpdate { offer } => {
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome = match http_client(UPDATE_USER_AGENT) {
                            Ok(client) => crate::updates::download_update(&client, &offer).await,
                            Err(message) => Err(message),
                        };
                        let _ = reply.send(BridgeReply::UpdateDownloaded(outcome));
                    });
                }
                BridgeCommand::StartCopilotSignIn { models } => {
                    let reply = with_reply.reply;
                    let task_state = state.clone();
                    let task_events = events.clone();
                    tokio::spawn(async move {
                        let outcome = copilot_sign_in(task_state, task_events, models).await;
                        let _ = reply.send(outcome);
                    });
                }
                command => {
                    // Handlers perform storage and config I/O; running them
                    // inline would serialize the command loop behind every
                    // await and every synchronous file operation.
                    let task_state = state.clone();
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome = handle(&task_state, &command).await;
                        let _ = reply.send(outcome);
                    });
                }
            }
        }
        state.service.clone().shutdown().await;
    });
}

/// Shared per-process core state owned by the bridge thread.
struct CoreState {
    service: SessionService,
    home: HomeLayout,
    /// Resolved provider catalog; swapped in place by refreshes.
    catalog: Arc<RwLock<CatalogInfo>>,
    /// Per-session project directories for tool runs.
    projects: Arc<Mutex<HashMap<String, PathBuf>>>,
    /// Cached short-lived Copilot bearer token and its unix expiry.
    copilot: Arc<tokio::sync::Mutex<Option<(String, u64)>>>,
    /// Live turn cancellation tokens by session id; Escape targets these.
    turn_cancels: Arc<std::sync::Mutex<HashMap<String, CancellationToken>>>,
}

impl CoreState {
    fn new(home: HomeLayout, cached: CachedCatalog) -> Self {
        // Session→project bindings survive restarts through the durable UI
        // state; seed the in-memory map so tool working directories resolve
        // before the desktop re-binds anything.
        let mut projects = HashMap::new();
        if let Ok(ui_state) = read_ui_state(&home) {
            for (session_id, project) in &ui_state.session_projects {
                if let Some(session) = SessionId::parse(session_id) {
                    projects.insert(session.as_str().to_owned(), PathBuf::from(project));
                }
            }
        }
        Self {
            service: SessionService::new(&home),
            catalog: Arc::new(RwLock::new(CatalogInfo {
                document: Arc::new(cached.document),
                fetched_at: cached.fetched_at,
            })),
            home,
            projects: Arc::new(Mutex::new(projects)),
            copilot: Arc::new(tokio::sync::Mutex::new(None)),
            turn_cancels: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    /// The tool working directory for one session.
    fn project_dir(&self, session_id: &str) -> PathBuf {
        self.projects
            .lock()
            .expect("projects")
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| self.home.root().join(mycode_config::SCRATCH_DIR))
    }
}

/// Update checks identify the app to GitHub's API.
const UPDATE_USER_AGENT: &str = concat!("mycode-updates/", env!("CARGO_PKG_VERSION"));

/// Refreshes the provider catalog and reports a successful swap.
async fn refresh_catalog(
    state: &CoreState,
    events: &mpsc::Sender<BridgeEvent>,
    force: bool,
) -> BridgeReply {
    let settings = match read_app_settings(&state.home) {
        Ok(settings) => settings,
        Err(error) => return BridgeReply::Catalog(Err(render_config_error(&error))),
    };
    let client = match http_client(&settings.effective_user_agent()) {
        Ok(client) => client,
        Err(message) => return BridgeReply::Catalog(Err(message)),
    };
    let outcome =
        mycode_providers::catalog::refresh(&state.home, &client, force, DEFAULT_MAX_AGE_SECS).await;
    match outcome {
        RefreshOutcome::Fresh(cache) | RefreshOutcome::NotModified(cache) => {
            BridgeReply::Catalog(Ok(catalog_info_from(cache)))
        }
        RefreshOutcome::Updated(cache) => {
            let info = catalog_info_from(cache);
            let providers = info.document.providers.len();
            let fetched_at = info.fetched_at;
            if let Ok(mut guard) = state.catalog.write() {
                *guard = info.clone();
            }
            let _ = events.send(BridgeEvent::CatalogUpdated {
                providers,
                fetched_at,
            });
            BridgeReply::Catalog(Ok(info))
        }
        RefreshOutcome::Unavailable(message) => BridgeReply::Catalog(Err(message)),
    }
}

/// One background catalog refresh shortly after startup.
fn spawn_catalog_refresh(state: Arc<CoreState>, events: mpsc::Sender<BridgeEvent>) {
    tokio::spawn(async move {
        refresh_catalog(&state, &events, false).await;
    });
}

/// One background update check shortly after startup.
fn spawn_update_check(state: Arc<CoreState>, events: mpsc::Sender<BridgeEvent>) {
    tokio::spawn(async move {
        let home = state.home.clone();
        let Ok(Ok(ui_state)) = tokio::task::spawn_blocking(move || read_ui_state(&home)).await
        else {
            return;
        };
        if !ui_state.auto_update {
            return;
        }
        let Ok(client) = http_client(UPDATE_USER_AGENT) else {
            return;
        };
        if let Ok(Some(offer)) = crate::updates::latest_release(&client).await {
            let _ = events.send(BridgeEvent::UpdateAvailable { offer });
        }
    });
}

fn catalog_info_from(cache: CachedCatalog) -> CatalogInfo {
    CatalogInfo {
        document: Arc::new(cache.document),
        fetched_at: cache.fetched_at,
    }
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

        BridgeCommand::SearchProjectFiles { .. } => BridgeReply::ProjectFiles(Err(message)),
        BridgeCommand::McpListTools { server } => BridgeReply::McpTools {
            server_id: server.id.clone(),
            outcome: Err(message),
        },
        BridgeCommand::RollbackWorkspace { .. } => BridgeReply::RolledBack(Err(message)),
        BridgeCommand::RecallMessage { .. } => BridgeReply::Recalled(Err(message)),
        BridgeCommand::DeleteSession { .. } => BridgeReply::SessionDeleted(Err(message)),
        BridgeCommand::RemoveRecent { .. } => BridgeReply::UiStateSaved(Err(message)),
        BridgeCommand::ExportData { .. } => BridgeReply::Exported(Err(message)),
        BridgeCommand::ImportData { .. } => BridgeReply::Imported(Err(message)),
        BridgeCommand::ListResources { .. } => BridgeReply::Resources(Err(message)),
        BridgeCommand::AskAnswer { .. } => BridgeReply::AskAnswered(Err(message)),
        BridgeCommand::GetCatalog | BridgeCommand::RefreshCatalog => {
            BridgeReply::Catalog(Err(message))
        }
        BridgeCommand::LoadUiState => BridgeReply::UiState(Err(message)),
        BridgeCommand::SaveUiState { .. } => BridgeReply::UiStateSaved(Err(message)),
        BridgeCommand::SetProjectDir { .. } => BridgeReply::ProjectSet(Err(message)),
        BridgeCommand::CheckUpdate => BridgeReply::UpdateChecked(Err(message)),
        BridgeCommand::DownloadUpdate { .. } => BridgeReply::UpdateDownloaded(Err(message)),
        BridgeCommand::StartCopilotSignIn { .. } => BridgeReply::CopilotSignInStarted(Err(message)),
        BridgeCommand::CancelChat { .. } => BridgeReply::ChatCancelled(Err(message)),
    }
}

/// Runs one synchronous storage/config step off the core runtime thread.
/// File locks, staged writes, and directory walks must never run inline:
/// the bridge runtime is single-threaded, so any blocking syscall freezes
/// every queued command and in-flight turn.
async fn blocking<T: Send + 'static>(
    step: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(step)
        .await
        .map_err(|error| format!("background task failed: {error}"))?
}

async fn handle(state: &CoreState, command: &BridgeCommand) -> BridgeReply {
    match command {
        BridgeCommand::ListSessions => BridgeReply::Sessions(
            inspect_summaries(&state.service, state.home.clone())
                .await
                .map_err(render_error),
        ),
        BridgeCommand::CreateSession => match state.service.create().await {
            Ok(created) => BridgeReply::Created(Ok(SessionSummary {
                session_id: created.session_id.as_str().to_owned(),
                root_branch_id: created.branch_id.as_str().to_owned(),
                title: String::new(),
                event_count: 0,
                active: true,
            })),
            Err(error) => BridgeReply::Created(Err(render_error(error))),
        },
        BridgeCommand::OpenSession(session) => BridgeReply::Conversation(
            open_conversation(&state.service, session)
                .await
                .map_err(render_error),
        ),
        BridgeCommand::SendMessage {
            session,
            branch,
            expected_head,
            text,
        } => BridgeReply::Sent(
            send_message(&state.service, session, branch, expected_head, text)
                .await
                .map_err(render_error),
        ),
        BridgeCommand::LoadSettings => {
            let home = state.home.clone();
            BridgeReply::Settings(blocking(move || load_settings(&home)).await)
        }
        BridgeCommand::SaveSettings {
            expected_revision,
            settings,
        } => {
            let home = state.home.clone();
            let settings = settings.clone();
            let expected_revision = *expected_revision;
            BridgeReply::SettingsSaved(
                blocking(move || save_settings(&home, expected_revision, &settings)).await,
            )
        }
        BridgeCommand::SaveProviderKey {
            provider_id,
            api_key,
        } => {
            let home = state.home.clone();
            let provider_id = provider_id.clone();
            let api_key = api_key.clone();
            BridgeReply::ProviderKeySaved(
                blocking(move || save_provider_key(&home, &provider_id, &api_key)).await,
            )
        }
        BridgeCommand::StartCopilotSignIn { .. } => BridgeReply::CopilotSignInStarted(Err(
            "device sign-in runs as a concurrent task".to_owned(),
        )),
        BridgeCommand::ChatTurn { .. } => {
            BridgeReply::ChatStarted(Err("chat turns run as concurrent tasks".to_owned()))
        }
        BridgeCommand::CancelChat { .. } => {
            BridgeReply::ChatCancelled(Err("chat cancels run as concurrent tasks".to_owned()))
        }

        BridgeCommand::SearchProjectFiles { session_id, query } => {
            let root = state.project_dir(session_id);
            let query = query.clone();
            BridgeReply::ProjectFiles(
                blocking(move || Ok(search_project_files(&root, &query))).await,
            )
        }
        BridgeCommand::McpListTools { server } => BridgeReply::McpTools {
            server_id: server.id.clone(),
            outcome: mcp_list_tools(&state.home, server).await,
        },
        BridgeCommand::RollbackWorkspace { session_id } => {
            let home = state.home.clone();
            let session_id = session_id.clone();
            BridgeReply::RolledBack(
                blocking(move || {
                    mycode_config::rollback_session(&home, &session_id)
                        .map_err(|error| render_config_error(&error))
                })
                .await,
            )
        }
        BridgeCommand::RecallMessage {
            session,
            branch,
            expected_head,
            to_event,
            edit,
        } => {
            let service = state.service.clone();
            let session = session.clone();
            let branch = branch.clone();
            let expected_head = expected_head.clone();
            let to_event = to_event.clone();
            let edit = edit.clone();
            let task = tokio::spawn(async move {
                recall_message(&service, &session, &branch, &expected_head, &to_event)
                    .await
                    .map(|conversation| (Box::new(conversation), edit))
            });
            match task.await {
                Ok(Ok(payload)) => BridgeReply::Recalled(Ok(payload)),
                Ok(Err(message)) => BridgeReply::Recalled(Err(message)),
                Err(error) => BridgeReply::Recalled(Err(error.to_string())),
            }
        }
        BridgeCommand::DeleteSession { session_id } => {
            let home = state.home.clone();
            let session_id = session_id.clone();
            BridgeReply::SessionDeleted(blocking(move || delete_session(&home, &session_id)).await)
        }
        BridgeCommand::RemoveRecent { project } => {
            let home = state.home.clone();
            let project = project.clone();
            BridgeReply::UiStateSaved(
                blocking(move || {
                    let mut ui_state =
                        read_ui_state(&home).map_err(|error| render_config_error(&error))?;
                    ui_state.remove_recent(&project);
                    replace_ui_state(&home, &ui_state).map_err(|error| render_config_error(&error))
                })
                .await,
            )
        }
        BridgeCommand::ExportData { path } => {
            let home = state.home.clone();
            let path = path.clone();
            let outcome =
                tokio::task::spawn_blocking(move || crate::export::export_to_file(&home, &path))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|outcome| outcome);
            BridgeReply::Exported(outcome)
        }
        BridgeCommand::ImportData { path } => {
            let home = state.home.clone();
            let path = path.clone();
            let outcome =
                tokio::task::spawn_blocking(move || crate::export::import_from_file(&home, &path))
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|outcome| outcome);
            BridgeReply::Imported(outcome)
        }
        BridgeCommand::ListResources { session_id } => {
            let home = state.home.clone();
            let workspace = state.project_dir(session_id);
            BridgeReply::Resources(
                blocking(move || {
                    let files = mycode_config::discover_resources(&home, &workspace);
                    Ok(files
                        .iter()
                        .map(|file| {
                            (
                                file.name.clone(),
                                file.path.as_os_str().to_string_lossy().into_owned(),
                            )
                        })
                        .collect())
                })
                .await,
            )
        }
        BridgeCommand::AskAnswer {
            session_id,
            answers,
        } => BridgeReply::AskAnswered(deliver_ask_answer(session_id, answers.clone())),
        BridgeCommand::GetCatalog => BridgeReply::Catalog(Ok(state
            .catalog
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| CatalogInfo {
                document: Arc::new(mycode_providers::catalog::bundled().clone()),
                fetched_at: 0,
            }))),
        BridgeCommand::LoadUiState => {
            let home = state.home.clone();
            BridgeReply::UiState(
                blocking(move || read_ui_state(&home).map_err(|error| render_config_error(&error)))
                    .await,
            )
        }
        BridgeCommand::SaveUiState { state: ui_state } => {
            let home = state.home.clone();
            let ui_state = ui_state.clone();
            BridgeReply::UiStateSaved(
                blocking(move || {
                    replace_ui_state(&home, &ui_state).map_err(|error| render_config_error(&error))
                })
                .await,
            )
        }
        BridgeCommand::SetProjectDir { session_id, path } => {
            BridgeReply::ProjectSet(set_project_dir(state, session_id, path.as_deref()))
        }
        BridgeCommand::RefreshCatalog | BridgeCommand::CheckUpdate => {
            BridgeReply::UpdateChecked(Err("this request runs as a concurrent task".to_owned()))
        }
        BridgeCommand::DownloadUpdate { .. } => {
            BridgeReply::UpdateDownloaded(Err("downloads run as concurrent tasks".to_owned()))
        }
    }
}

/// Validates and binds one project directory to a session.
fn set_project_dir(state: &CoreState, session_id: &str, path: Option<&str>) -> Result<(), String> {
    let Some(path) = path else {
        state.projects.lock().expect("projects").remove(session_id);
        return Ok(());
    };
    let directory = PathBuf::from(path);
    if !directory.is_absolute() || !directory.is_dir() {
        return Err("choose an existing directory".to_owned());
    }
    state
        .projects
        .lock()
        .expect("projects")
        .insert(session_id.to_owned(), directory);
    Ok(())
}

/// Lists the tools of one MCP server binding over stdio or HTTP.
///
/// The caller supplies the row, so a settings form can test a binding it has
/// not saved yet. Only the API key is read from the vault, because a key is
/// never carried in a command.
///
/// Runs on the caller's runtime; never builds a nested one (a nested
/// `Runtime::block_on` panics and takes the core thread down with it).
async fn mcp_list_tools(
    home: &HomeLayout,
    server: &mycode_config::McpServerSettings,
) -> Result<Vec<String>, String> {
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
    let api_key = secrets
        .key(&format!("mcp-{}", server.id))
        .map(str::to_owned);
    let timeout = crate::mcp_client::DEFAULT_REQUEST_TIMEOUT;
    let channel: Arc<dyn crate::mcp_client::JsonRpcChannel> = match server.transport.as_str() {
        "stdio" => {
            let command = server
                .command
                .as_deref()
                .ok_or("stdio server is missing its command")?;
            let env: Vec<(String, String)> = server
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            Arc::new(
                crate::mcp_client::StdioChannel::spawn(command, &server.args, &env, timeout)
                    .await
                    .map_err(|error| format!("MCP spawn failed: {error}"))?,
            )
        }
        "http" => {
            let endpoint = server
                .endpoint
                .as_deref()
                .ok_or("http server is missing its endpoint")?;
            Arc::new(
                crate::mcp_client::HttpChannel::new(
                    endpoint,
                    crate::mcp_client::HttpChannelOptions {
                        key_header: crate::mcp_client::KeyHeader::parse(
                            server.key_header.as_deref(),
                        ),
                        api_key,
                        timeout,
                    },
                )
                .map_err(|error| format!("MCP channel failed: {error}"))?,
            )
        }
        _ => return Err("unknown MCP transport".to_owned()),
    };
    let mut client = crate::mcp_client::McpClient::new(channel);
    client
        .initialize()
        .await
        .map_err(|error| format!("MCP handshake failed: {error}"))?;
    let tools = client
        .list_tools()
        .await
        .map_err(|error| format!("MCP tools listing failed: {error}"))?;
    client.shutdown().await;
    Ok(tools.iter().map(|tool| tool.name.clone()).collect())
}

/// Environment variable carrying the Querit API key, checked before the vault
/// (the pi agent's querit plugin resolves this variable before its own config
/// file, so a machine already set up for pi keeps working).
const QUERIT_KEY_ENV: &str = "QUERIT_API_KEY";
/// Environment variable carrying the AnySearch API key. Anonymous traffic is
/// allowed when this and the vault entry are both absent.
const ANYSEARCH_KEY_ENV: &str = "ANYSEARCH_API_KEY";

/// Builds the web client for the enabled backend, or the first builtin that
/// already has a key (env or vault).
fn web_client(home: &HomeLayout) -> Result<crate::web_client::WebClient, String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let secrets = read_provider_secrets(home).ok();
    let backend = resolve_web_backend(&settings, secrets.as_ref())?;
    let kind = crate::web_client::SearchKind::parse(&backend.kind)
        .ok_or_else(|| format!("unknown search backend kind '{}'", backend.kind))?;
    let key = web_backend_key(&backend, secrets.as_ref());
    if key.is_none() && kind == crate::web_client::SearchKind::Querit {
        return Err(format!(
            "no API key for the '{}' search backend — paste one in Settings or set {QUERIT_KEY_ENV}",
            backend.id
        ));
    }
    let transport = crate::web_client::transport::ReqwestWebTransport::new()
        .map_err(|_| "web transport unavailable".to_owned())?;
    crate::web_client::WebClient::new(&backend.endpoint, key, std::sync::Arc::new(transport))
        .map_err(|_| "the search backend endpoint violates the URL policy".to_owned())
        .map(|client| client.with_kind(kind))
}

fn web_backend_key(
    backend: &mycode_config::WebBackendSettings,
    secrets: Option<&mycode_config::ProviderSecrets>,
) -> Option<String> {
    let env_name = match backend.kind.as_str() {
        "querit" => Some(QUERIT_KEY_ENV),
        "anysearch" => Some(ANYSEARCH_KEY_ENV),
        _ => None,
    };
    env_name
        .and_then(|name| std::env::var(name).ok())
        .map(|value| mycode_config::normalize_api_key(&value))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            secrets.and_then(|secrets| {
                secrets
                    .key(&format!("web-{}", backend.id))
                    .map(mycode_config::normalize_api_key)
                    .filter(|value| !value.is_empty())
            })
        })
}

fn resolve_web_backend(
    settings: &AppSettings,
    secrets: Option<&mycode_config::ProviderSecrets>,
) -> Result<mycode_config::WebBackendSettings, String> {
    if let Some(backend) = settings.web.backends.iter().find(|backend| backend.enabled) {
        return Ok(backend.clone());
    }
    let mut candidates = mycode_config::builtin_web_backends();
    for backend in &settings.web.backends {
        if let Some(slot) = candidates.iter_mut().find(|item| item.id == backend.id) {
            *slot = backend.clone();
        } else {
            candidates.push(backend.clone());
        }
    }
    if let Some(backend) = candidates
        .iter()
        .find(|backend| web_backend_key(backend, secrets).is_some())
    {
        return Ok(backend.clone());
    }
    Err("no search backend is ready — paste a Querit or AnySearch API key in Settings".to_owned())
}

/// Host channel for the model's web tools: the same bounded client the
/// settings page configures.
pub(crate) struct BridgeWebHost {
    pub(crate) home: HomeLayout,
}

#[async_trait::async_trait]
impl mycode_tools::builtin::WebHost for BridgeWebHost {
    async fn search(
        &self,
        query: &str,
        max_results: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<mycode_tools::builtin::WebHit>, mycode_tools::ToolError> {
        let fail = |message: String| mycode_tools::ToolError::Execution(message);
        let client = web_client(&self.home).map_err(fail)?;
        let results = client
            .search(query, max_results, cancel.clone())
            .await
            .map_err(|error| fail(format!("search failed: {error}")))?;
        Ok(results
            .into_iter()
            .map(|result| mycode_tools::builtin::WebHit {
                url: result.url,
                title: result.title,
                snippet: result.snippet,
            })
            .collect())
    }

    async fn fetch_content(
        &self,
        urls: &[String],
        cancel: &CancellationToken,
    ) -> Result<Vec<mycode_tools::builtin::WebPage>, mycode_tools::ToolError> {
        let fail = |message: String| mycode_tools::ToolError::Execution(message);
        let client = web_client(&self.home).map_err(fail)?;
        let pages = client
            .contents(urls, cancel.clone())
            .await
            .map_err(|error| fail(format!("fetch failed: {error}")))?;
        Ok(pages
            .into_iter()
            .map(|page| mycode_tools::builtin::WebPage {
                url: page.url,
                content: page.content,
                truncated: page.truncated,
            })
            .collect())
    }
}

fn load_settings(
    home: &HomeLayout,
) -> Result<(AppSettings, AuthorityRevision, Vec<String>, Vec<String>), String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let revision = mycode_config::read_owned_file(
        home,
        mycode_config::SETTINGS_PATH,
        mycode_config::MAX_SETTINGS_BYTES,
    )
    .map_err(|error| render_config_error(&error))?
    .map(|bytes| settings_revision(bytes.as_slice()))
    .transpose()
    .map_err(|()| "stored settings failed validation".to_owned())?
    .unwrap_or(AuthorityRevision::ABSENT);
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
    let (provider_keys, mcp_keys) = split_key_ids(&secrets);
    Ok((settings, revision, provider_keys, mcp_keys))
}

fn settings_revision(bytes: &[u8]) -> Result<AuthorityRevision, ()> {
    #[derive(serde::Deserialize)]
    struct Header {
        #[serde(rename = "formatVersion")]
        format_version: u32,
        revision: u64,
    }
    let header: Header = serde_json::from_slice(bytes).map_err(|_| ())?;
    if header.format_version != mycode_config::SETTINGS_FORMAT_VERSION {
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

fn render_config_error(error: &mycode_config::ConfigError) -> String {
    format!("settings error: {error}")
}

/// Lists sessions with display titles: the first user message of each root
/// branch, first page only. Per-session read failures degrade to an empty
/// title; the listing itself never fails on one bad session.
async fn inspect_summaries(
    service: &SessionService,
    home: HomeLayout,
) -> Result<Vec<SessionSummary>, SessionError> {
    // Directory walk stays off the core runtime thread.
    let snapshots = tokio::task::spawn_blocking(move || session::inspect_sessions(&home))
        .await
        .map_err(|_| SessionError::Unavailable)??;
    let mut summaries = Vec::with_capacity(snapshots.len());
    for snapshot in snapshots {
        let Some(root) = snapshot.branches.first() else {
            continue;
        };
        let branch_id = root.branch_id.clone();
        let snapshot_head = root.head.clone();
        let title = session_title(service, &snapshot.session_id, &branch_id, &snapshot_head).await;
        summaries.push(SessionSummary {
            root_branch_id: branch_id.as_str().to_owned(),
            event_count: snapshot.branches.iter().map(|b| b.event_count).sum(),
            session_id: snapshot.session_id.as_str().to_owned(),
            title,
            active: false,
        });
    }
    Ok(summaries)
}

/// Reads the first user message of a session's root branch for the sidebar
/// title; empty when the session has no messages yet. A stale manifest head
/// (an active turn appended events) degrades to an empty title.
async fn session_title(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    head: &HeadStamp,
) -> String {
    let Ok(page) = service.read(session, branch, head, None, 8).await else {
        return String::new();
    };
    for event in &page.items {
        if event.kind != EventKind::Message {
            continue;
        }
        if let Ok(loaded) = service.load_event(session, branch, &event.event_id).await {
            return title_from_text(&decode_text(&loaded.payload));
        }
    }
    String::new()
}

/// Collapses one message into a one-line sidebar title.
fn title_from_text(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(60).collect()
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
    read_branch(service, session, &root.branch_id, &root.head).await
}

/// Reads one branch's committed events into display entries.
async fn read_branch(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    snapshot_head: &HeadStamp,
) -> Result<ActiveConversation, SessionError> {
    let mut entries = Vec::new();
    let mut after: Option<mycode_agent::session::SessionEventId> = None;
    loop {
        let page = service
            .read(session, branch, snapshot_head, after.as_ref(), 256)
            .await?;
        if page.items.is_empty() {
            break;
        }
        let last = page.items.last().expect("nonempty page").event_id.clone();
        for event in &page.items {
            let loaded = service.load_event(session, branch, &event.event_id).await?;
            entries.push(project_replayed_entry(event, &loaded.payload));
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
        branch_id: branch.as_str().to_owned(),
        head: head_spelling(snapshot_head),
        entries,
        streaming: None,
    })
}

/// Rebuilds the model-facing turn history from committed branch events.
///
/// Assistant messages keep their tool_use blocks, and tool results replay as
/// `Message::ToolResult`, so the wire sequence stays valid across turns.
/// Thinking blocks are stripped from replay: their signatures are bound to
/// the model that produced them, and gateways reject a cross-model replay
/// (an M2 conversation continued on M3 fails with "invalid parameter").
/// In-turn thinking (same model, same agentic loop) never passes through
/// here, so tool_use continuation keeps its signatures on the wire.
/// Usage and task bookkeeping never reach the provider.
async fn ledger_history(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    snapshot_head: &HeadStamp,
) -> Result<Vec<Message>, SessionError> {
    let mut history = Vec::new();
    let mut after: Option<mycode_agent::session::SessionEventId> = None;
    loop {
        let page = service
            .read(session, branch, snapshot_head, after.as_ref(), 256)
            .await?;
        if page.items.is_empty() {
            break;
        }
        let last = page.items.last().expect("nonempty page").event_id.clone();
        for event in &page.items {
            let loaded = service.load_event(session, branch, &event.event_id).await?;
            match event.kind {
                EventKind::Message => {
                    // Assistant messages are typed JSON; a parse miss means
                    // the payload is the user's plain-text message.
                    match serde_json::from_slice::<mycode_core::AssistantMessage>(&loaded.payload) {
                        Ok(mut assistant) => {
                            // Signatures are model-bound; replaying them to a
                            // different model fails provider validation.
                            assistant.blocks.retain(|block| {
                                !matches!(block, mycode_core::ContentBlock::Thinking(_))
                            });
                            history.push(Message::Assistant(assistant));
                        }
                        Err(_) => history.push(Message::User(mycode_core::UserMessage::text(
                            decode_text(&loaded.payload),
                        ))),
                    }
                }
                EventKind::ToolResult => {
                    if let Ok(result) =
                        serde_json::from_slice::<mycode_core::ToolResultMessage>(&loaded.payload)
                    {
                        history.push(Message::ToolResult(result));
                    }
                }
                EventKind::ToolCall | EventKind::Usage | EventKind::Task => {}
            }
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
    Ok(history)
}

/// Rewinds the branch so everything from the recalled message onward is
/// gone, then returns the truncated conversation plus the edited text.
async fn recall_message(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    _expected_head: &HeadStamp,
    to_event: &str,
) -> Result<ActiveConversation, String> {
    let target = mycode_agent::session::SessionEventId::parse(to_event)
        .ok_or_else(|| "the rewind target is not a valid event id".to_owned())?;
    let reservation = service
        .reserve_branch(
            session,
            mycode_agent::session::BranchMutationKind::Rewind,
            branch,
            &target,
        )
        .await
        .map_err(render_error)?;
    let branched = service
        .rewind(session, branch, &target, &reservation)
        .await
        .map_err(render_error)?;
    read_branch(service, session, &branched.branch_id, &branched.head)
        .await
        .map_err(render_error)
}

/// Bounded file index for the composer's `@` mention: a case-insensitive
/// substring match over the session project, skipping dependency and VCS
/// directories, shortest paths first.
fn search_project_files(root: &std::path::Path, query: &str) -> Vec<String> {
    const MAX_VISIT: usize = 8_192;
    const MAX_COLLECT: usize = 64;
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        "out",
        ".next",
        ".venv",
        "__pycache__",
        ".mycode",
        "checkpoints",
    ];
    let needle = query.to_ascii_lowercase();
    let mut matches: Vec<String> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_VISIT {
                return finish_mention_matches(matches);
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if let Some(name) = path.file_name().and_then(|name| name.to_str())
                    && (SKIP_DIRS.contains(&name) || name.starts_with('.'))
                {
                    continue;
                }
                stack.push(path);
            } else if meta.is_file() {
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let spelling = rel.to_string_lossy().replace('\\', "/");
                if needle.is_empty() || spelling.to_ascii_lowercase().contains(&needle) {
                    matches.push(spelling);
                    if matches.len() >= MAX_COLLECT {
                        return finish_mention_matches(matches);
                    }
                }
            }
        }
    }
    finish_mention_matches(matches)
}

/// Shortest-first truncation shared by the walk's exit points.
fn finish_mention_matches(mut matches: Vec<String>) -> Vec<String> {
    const MAX_MATCHES: usize = 8;
    matches.sort_by_key(|path| (path.len(), path.clone()));
    matches.truncate(MAX_MATCHES);
    matches
}

/// Deletes one session's durable footprint: ledger, todos, compaction
/// checkpoint, and file snapshots. The ids are plain names by construction.
///
/// Only the ledger directory always exists; todos and snapshots are created
/// lazily by tools, so an absent directory is a successful delete rather than
/// an error.
fn delete_session(home: &HomeLayout, session_id: &str) -> Result<(), String> {
    if session_id.is_empty()
        || session_id.contains(['/', '\\', ':', '\0'])
        || session_id == "."
        || session_id == ".."
    {
        return Err("invalid session id".to_owned());
    }
    let roots = [
        home.root()
            .join(mycode_config::SESSIONS_DIR)
            .join(session_id),
        home.root().join("checkpoints").join(session_id),
    ];
    for root in roots {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("delete: {error}")),
        }
    }
    Ok(())
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
            text: text.into(),
            call_id: None,
            thinking: String::new(),
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

/// Splits stored secret ids into provider and MCP key markers.
fn split_key_ids(secrets: &mycode_config::ProviderSecrets) -> (Vec<String>, Vec<String>) {
    let mut provider_keys = Vec::new();
    let mut mcp_keys = Vec::new();
    for id in secrets.provider_ids() {
        if let Some(server_id) = id.strip_prefix("mcp-") {
            if !server_id.is_empty() {
                mcp_keys.push(server_id.to_owned());
            }
        } else {
            provider_keys.push(id.to_owned());
        }
    }
    (provider_keys, mcp_keys)
}

/// Stores or clears one provider key under the secret-store CAS, returning
/// the refreshed key-id lists so the UI updates its markers in place.
fn save_provider_key(
    home: &HomeLayout,
    provider_id: &str,
    api_key: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
    let expected = mycode_config::read_owned_file(
        home,
        mycode_config::SECRETS_PATH,
        mycode_config::MAX_SECRETS_BYTES,
    )
    .map_err(|error| render_config_error(&error))?
    .map(|bytes| secrets_revision(bytes.as_slice()))
    .transpose()
    .map_err(|()| "stored secrets failed validation".to_owned())?
    .unwrap_or(AuthorityRevision::ABSENT);
    let api_key = mycode_config::normalize_api_key(api_key);
    let updated = secrets.with_key(
        provider_id,
        (!api_key.is_empty()).then_some(api_key.as_str()),
    );
    replace_provider_secrets(home, expected, &updated)
        .map_err(|error| render_config_error(&error))?;
    Ok(split_key_ids(&updated))
}

fn secrets_revision(bytes: &[u8]) -> Result<AuthorityRevision, ()> {
    #[derive(serde::Deserialize)]
    struct Header {
        #[serde(rename = "formatVersion")]
        format_version: u32,
        revision: u64,
    }
    let header: Header = serde_json::from_slice(bytes).map_err(|_| ())?;
    if header.format_version != mycode_config::SECRETS_FORMAT_VERSION {
        return Err(());
    }
    AuthorityRevision::new(header.revision).map_err(|_| ())
}

// ---- GitHub Copilot OAuth device flow ----

/// Starts the device flow, opens the browser, and spawns the poll loop that
/// finishes the sign-in (or reports failure) over the event channel.
async fn copilot_sign_in(
    state: Arc<CoreState>,
    events: mpsc::Sender<BridgeEvent>,
    models: Vec<String>,
) -> BridgeReply {
    let client = match http_client(UPDATE_USER_AGENT) {
        Ok(client) => client,
        Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
    };
    let start = match start_device_flow(&client).await {
        Ok(start) => start,
        Err(message) => return BridgeReply::CopilotSignInStarted(Err(message)),
    };
    open_browser(&start.verification_uri);
    let device_code = start.device_code;
    let mut interval_secs = start.interval_secs.max(1);
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in_secs.max(1));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
            if std::time::Instant::now() >= deadline {
                let _ = events.send(BridgeEvent::CopilotSignInFailed {
                    message: "the sign-in code expired before authorization".to_owned(),
                });
                return;
            }
            match poll_device_token(&client, &device_code).await {
                Ok(DeviceTokenPoll::Granted(token)) => {
                    let outcome = finish_copilot_sign_in(&state, &token, &models).await;
                    let _ = events.send(match outcome {
                        Ok(()) => BridgeEvent::CopilotSignedIn,
                        Err(message) => BridgeEvent::CopilotSignInFailed { message },
                    });
                    return;
                }
                Ok(DeviceTokenPoll::Pending) => {}
                Ok(DeviceTokenPoll::SlowDown) => interval_secs += 5,
                Ok(DeviceTokenPoll::Denied(reason)) => {
                    let _ = events.send(BridgeEvent::CopilotSignInFailed {
                        message: reason.to_owned(),
                    });
                    return;
                }
                Err(message) => {
                    let _ = events.send(BridgeEvent::CopilotSignInFailed { message });
                    return;
                }
            }
        }
    });
    BridgeReply::CopilotSignInStarted(Ok(CopilotSignInInfo {
        user_code: start.user_code,
        verification_uri: start.verification_uri,
    }))
}

/// Verifies the grant works, stores the OAuth token, and configures the
/// provider entry.
async fn finish_copilot_sign_in(
    state: &CoreState,
    github_token: &str,
    models: &[String],
) -> Result<(), String> {
    let client = http_client(UPDATE_USER_AGENT)?;
    let bearer = copilot_bearer(&client, github_token).await?;
    *state.copilot.lock().await = Some((bearer.token, bearer.expires_at_unix));
    save_provider_key(&state.home, COPILOT_PROVIDER_ID, github_token)?;
    upsert_copilot_provider(state, models)
}

/// Adds or refreshes the `github-copilot` provider entry with the chosen
/// models, defaulting to the catalog's tool-calling presets.
fn upsert_copilot_provider(state: &CoreState, models: &[String]) -> Result<(), String> {
    let catalog = state
        .catalog
        .read()
        .map(|guard| guard.clone())
        .ok()
        .and_then(|catalog| {
            catalog
                .document
                .provider(COPILOT_PROVIDER_ID)
                .map(|preset| (preset.base_url.clone(), preset.models.clone()))
        });
    let (base_url, catalog_models) =
        catalog.unwrap_or_else(|| ("https://api.githubcopilot.com".to_owned(), Vec::new()));
    let bound: Vec<String> = if models.is_empty() {
        catalog_models
            .iter()
            .filter(|model| model.tool_call)
            .take(6)
            .map(|model| model.id.clone())
            .collect()
    } else {
        models.to_vec()
    };
    if bound.is_empty() {
        return Err("no Copilot models were selected".to_owned());
    }
    let (mut settings, revision, _, _) = load_settings(&state.home)?;
    match settings
        .providers
        .iter_mut()
        .find(|provider| provider.id == COPILOT_PROVIDER_ID)
    {
        Some(existing) => {
            existing.models = bound;
            existing.enabled = true;
        }
        None => settings.providers.push(ProviderSettings {
            id: COPILOT_PROVIDER_ID.to_owned(),
            kind: mycode_providers::catalog::KIND_OPENAI_COMPLETIONS.to_owned(),
            base_url,
            models: bound,
            enabled: true,
            context_limit: None,
            max_output: None,
        }),
    }
    replace_app_settings(&state.home, revision, &settings)
        .map_err(|error| render_config_error(&error))?;
    Ok(())
}

/// Returns a live Copilot bearer, exchanging a fresh one when the cached copy
/// is stale.
async fn ensure_copilot_bearer(state: &CoreState, github_token: &str) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let mut cache = state.copilot.lock().await;
    if let Some((token, expires_at)) = cache.as_ref()
        && *expires_at > now.saturating_add(60)
    {
        return Ok(token.clone());
    }
    let client = http_client(UPDATE_USER_AGENT)?;
    let bearer = copilot_bearer(&client, github_token).await?;
    let token = bearer.token;
    *cache = Some((token.clone(), bearer.expires_at_unix));
    Ok(token)
}

/// Opens one verification page in the default browser, best-effort.
fn open_browser(url: &str) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = url;
    }
}

/// One model turn: resolve the provider, stream the reply into the event
/// channel, and commit the assistant message to the session ledger.
#[allow(clippy::too_many_arguments)]
async fn chat_turn(
    state: Arc<CoreState>,
    events: mpsc::Sender<BridgeEvent>,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: String,
    model: String,
) {
    let session_id = session.as_str().to_owned();
    let cwd = state.project_dir(&session_id);
    if let Err(message) = run_chat_turn(
        &state,
        &events,
        &session_id,
        session,
        branch,
        expected_head,
        &provider_id,
        &model,
        cwd,
    )
    .await
    {
        let _ = events.send(BridgeEvent::ChatFailed {
            session_id,
            message,
        });
    }
}

/// Loads the provider row and its stored key for one turn. Saves dispatched
/// alongside the turn run as concurrent tasks, so a just-added provider may
/// not have reached the disk yet — the read settles with a short retry.
async fn turn_credentials(
    home: &HomeLayout,
    provider_id: &str,
) -> Result<(AppSettings, ProviderSettings, String), String> {
    let mut last_error = "provider not found or disabled in settings".to_owned();
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
        let settings = match read_app_settings(home) {
            Ok(settings) => settings,
            Err(error) => {
                last_error = render_config_error(&error);
                continue;
            }
        };
        let Some(index) = settings
            .providers
            .iter()
            .position(|provider| provider.id == provider_id && provider.enabled)
        else {
            last_error = "provider not found or disabled in settings".to_owned();
            continue;
        };
        let secrets = match read_provider_secrets(home) {
            Ok(secrets) => secrets,
            Err(error) => {
                last_error = render_config_error(&error);
                continue;
            }
        };
        let Some(key) = secrets.key(provider_id) else {
            last_error = "provider API key is not set".to_owned();
            continue;
        };
        let provider = settings.providers[index].clone();
        return Ok((settings, provider, key.to_owned()));
    }
    Err(last_error)
}

/// Catalog or settings context window for compaction. Zero means the
/// Codex-style fallback threshold.
fn model_context_window(state: &CoreState, provider: &ProviderSettings, model: &str) -> u64 {
    if let Some(limit) = provider.context_limit.filter(|tokens| *tokens > 0) {
        return limit;
    }
    let Ok(catalog) = state.catalog.read() else {
        return 0;
    };
    let document = &catalog.document;
    document
        .provider(&provider.id)
        .or_else(|| {
            document
                .providers
                .iter()
                .find(|item| item.base_url == provider.base_url)
        })
        .and_then(|item| item.models.iter().find(|entry| entry.id == model))
        .map(|entry| entry.context)
        .filter(|context| *context > 0)
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
async fn run_chat_turn(
    state: &CoreState,
    events: &mpsc::Sender<BridgeEvent>,
    session_id: &str,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: &str,
    model: &str,
    cwd: PathBuf,
) -> Result<(), String> {
    let home = &state.home;
    // Settings and keys are written by concurrently dispatched save
    // commands; a provider added moments ago may not have landed on disk
    // yet, so the lookup retries briefly before failing the turn.
    let (settings, provider, stored_key) = turn_credentials(home, provider_id).await?;
    // Copilot stores its long-lived OAuth token where other providers keep
    // an API key; each turn exchanges it for a short-lived bearer.
    let (bearer, extra_headers) = if provider.base_url.contains("githubcopilot.com") {
        let token = ensure_copilot_bearer(state, &stored_key).await?;
        (
            token,
            vec![("copilot-integration-id".to_owned(), "mycode".to_owned())],
        )
    } else {
        (stored_key, Vec::new())
    };
    let mut resolved =
        ResolvedProvider::resolve(&provider, model, &bearer, &settings.effective_user_agent())
            .map_err(|error| format!("provider setup failed: {error:?}"))?;
    resolved.headers.extend(extra_headers);
    let transport: Arc<dyn SseTransport> =
        Arc::new(ReqwestTransport::new().map_err(|_| "HTTP transport unavailable".to_owned())?);
    let wire = WireProvider::new(resolved.clone(), transport.clone());

    // The tool working directory is the bound project (created on demand).
    let cwd_for_mkdir = cwd.clone();
    tokio::task::spawn_blocking(move || std::fs::create_dir_all(&cwd_for_mkdir))
        .await
        .map_err(|error| format!("workspace dir task: {error}"))?
        .map_err(|error| format!("workspace dir: {error}"))?;
    // Replay history is rebuilt from the ledger's typed events: display
    // entries flatten tool traffic into text, which breaks the
    // tool_use/tool_result pairing providers validate.
    let history = ledger_history(&state.service, &session, &branch, &expected_head)
        .await
        .map_err(render_error)?;
    let usage_enabled = settings.usage.enabled;
    let usage_provider = provider_id.to_owned();
    let usage_model = model.to_owned();
    let head_stamp_text = match &expected_head {
        HeadStamp::Empty => "empty".to_owned(),
        HeadStamp::Event(event) => event.as_str().to_owned(),
    };
    let writer = HeadWriter::new(
        state.service.clone(),
        session.clone(),
        branch.clone(),
        expected_head,
    );
    // MCP servers connect here (spawn + handshake + tools/list): awaited on
    // the spawned turn task, so command processing never blocks. A server
    // that fails to connect is skipped, never a failed turn.
    let mcp_tools = crate::mcp_tools::connect_mcp_tools(home, &settings).await;
    let registry = Arc::new({
        let registry = ToolRegistry::new();
        mycode_tools::register_builtins(&registry);
        // ask_user rides the same registry; its channel forwards questions
        // to the UI over the event channel and waits on the shared router.
        let ask_events = events.clone();
        let ask_session = session_id.to_owned();
        let answer_rx = ASK_ROUTER
            .get_or_init(AskRouter::default)
            .register(&ask_session);
        let channel: Arc<dyn mycode_tools::builtin::AskChannel> = Arc::new(BridgeAskChannel {
            session_id: ask_session,
            events: ask_events,
            answer: tokio::sync::Mutex::new(Some(answer_rx)),
        });
        registry.register(Arc::new(mycode_tools::builtin::AskTool::new(channel)));
        // task delegates scoped work to a catalog role; slots, isolation,
        // and per-role model routes live in the host.
        let role_catalog = mycode_config::discover_roles(home, Some(&cwd));
        if crate::subagent::any_role_enabled(&role_catalog, &settings.subagents) {
            registry.register(Arc::new(mycode_tools::builtin::TaskTool::new(Arc::new(
                crate::subagent::BridgeTaskHost::new(
                    resolved.clone(),
                    home.clone(),
                    cwd.clone(),
                    &settings,
                ),
            ))));
        }
        // The model's web tools ride the same settings-configured backend.
        let web_host: Arc<dyn mycode_tools::builtin::WebHost> =
            Arc::new(BridgeWebHost { home: home.clone() });
        registry.register(Arc::new(mycode_tools::builtin::WebSearchTool::new(
            web_host.clone(),
        )));
        registry.register(Arc::new(mycode_tools::builtin::FetchContentTool::new(
            web_host,
        )));
        // todo_write persists the plan and appends a durable Task event.
        let todo_events = events.clone();
        let todo_session = session_id.to_owned();
        let todo_writer = writer.clone();
        let todo_home = home.clone();
        let store: Arc<dyn mycode_tools::builtin::TodoStore> = Arc::new(BridgeTodoStore {
            events: todo_events,
            session_id: todo_session,
            writer: todo_writer,
            home: todo_home,
        });
        registry.register(Arc::new(mycode_tools::builtin::TodoWriteTool::new(store)));
        // MCP tools ride the same registry; they never shadow an existing
        // registration (registry.register is last-wins per name).
        for tool in mcp_tools {
            let name = tool.spec().name;
            if registry.get(&name).is_none() {
                registry.register(tool);
            }
        }
        registry
    });

    // Split the last committed user message off as the prompt; everything
    // before it is replay history.
    let (history, prompt) = match history.split_last() {
        Some((Message::User(user), prefix)) => (prefix.to_vec(), user.clone()),
        _ => return Err("the turn has no user message to answer".to_owned()),
    };
    // session_id is borrowed by the checkpoint closure and later moved into
    // the pump; give each its own copy.
    let session_id = session_id.to_owned();

    let context_window = model_context_window(state, &provider, model);
    // Codex-style checkpoint: 90% of the usable window, ~20k-token tail.
    // Also installed as a before-request hook so tool-heavy mid-turn
    // cycles re-estimate after each durable tool result.
    let compact_scope = crate::compaction::CompactScope {
        home,
        wire: &wire,
        model,
        session_id: &session_id,
        branch_id: branch.as_str(),
        head: &head_stamp_text,
        context_window,
    };
    let history = crate::compaction::compact_history(&compact_scope, history).await;

    let resources = mycode_config::discover_resources(home, &cwd);
    let mut system_prompt =
        String::from("You are MYCode, a coding agent. Answer concisely and explain what you did.");
    system_prompt.push_str(
        "\n\nFile work MUST use the native tools: `find` and `grep` to locate \
files and code, `read` to inspect them, `write` and `edit` to change them. \
Use `shell` only when a task genuinely needs a process (build, test, git, \
package installs) — never to search, read, or write files.\n\
Use `web_search` then `fetch_content` for current web facts (Querit or AnySearch). \
Use `task` to delegate to scout/artisan/steward/sentinel when a scoped role fits. \
Connected MCP servers add their tools to the list below.",
    );
    for part in mycode_config::render_resource_prompt(&resources) {
        system_prompt.push_str(
            "

",
        );
        system_prompt.push_str(&part);
    }
    // The tool registry's usage hints ride along, so the model knows which
    // tools exist and how to call them (a custom prompt alone drops them).
    system_prompt.push_str(
        "

",
    );
    system_prompt.push_str(&mycode_agent::build_system_prompt(&registry));
    let role_catalog = mycode_config::discover_roles(home, Some(&cwd));
    system_prompt.push_str(&crate::subagent::delegation_directive(
        &role_catalog,
        &settings.subagents,
    ));

    let turn_started = std::time::Instant::now();
    let (agent_tx, mut agent_rx) = tokio::sync::broadcast::channel(256);
    let checkpoint_home = home.clone();
    let checkpoint_cwd = cwd.clone();
    let checkpoint_session = session_id.to_owned();
    let compact_home = home.clone();
    let compact_wire = wire.clone();
    let compact_model = model.to_owned();
    let compact_session = session_id.clone();
    let compact_branch = branch.as_str().to_owned();
    let compact_head = head_stamp_text.clone();
    let hooks = HookRunner::default()
        .with_before_request(move |mut request| {
            let home = compact_home.clone();
            let wire = compact_wire.clone();
            let model = compact_model.clone();
            let session_id = compact_session.clone();
            let branch_id = compact_branch.clone();
            let head = compact_head.clone();
            async move {
                let scope = crate::compaction::CompactScope {
                    home: &home,
                    wire: &wire,
                    model: &model,
                    session_id: &session_id,
                    branch_id: &branch_id,
                    head: &head,
                    context_window,
                };
                request.messages =
                    crate::compaction::compact_history(&scope, request.messages).await;
                request
            }
        })
        .with_before_tool(move |tool, args| {
            // Mutating file tools snapshot their target before dispatch; a
            // relative path resolves against the turn's working directory. The
            // copy is file I/O, so it runs on the blocking pool rather than the
            // single-threaded core executor.
            let raw_path = matches!(tool, "write" | "edit")
                .then(|| {
                    args.get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .flatten();
            let checkpoint_home = checkpoint_home.clone();
            let checkpoint_cwd = checkpoint_cwd.clone();
            let checkpoint_session = checkpoint_session.clone();
            async move {
                let Some(raw_path) = raw_path else { return };
                let path = checkpoint_cwd.join(raw_path);
                let _ = tokio::task::spawn_blocking(move || {
                    mycode_config::checkpoint_file(&checkpoint_home, &checkpoint_session, &path)
                })
                .await;
            }
        });
    let cancel = CancellationToken::new();
    // Publish the token so an Escape-driven CancelChat can abort this turn;
    // the guard unpublishes it on every exit path.
    let _cancel_guard = CancelGuard::register(state.turn_cancels.clone(), &session_id, &cancel);
    let mut config = AgentConfig::new().with_system_prompt(system_prompt);
    if let Some(level) = settings.reasoning_effort.as_deref() {
        let level = match level {
            "low" => mycode_core::ReasoningLevel::Low,
            "medium" => mycode_core::ReasoningLevel::Medium,
            "high" => mycode_core::ReasoningLevel::High,
            _ => return Err("settings reasoningEffort must be low, medium, or high".to_owned()),
        };
        config = config.with_reasoning(level);
    }
    let mut agent = Agent::new(config);

    // The ledger pump owns the branch head: tool results commit as they
    // complete, the final assistant message commits at turn end.
    let pump_events = events.clone();
    let pump_session_id = session_id.to_owned();
    let writer = {
        let _ = &writer;
        writer
    };
    let pump = tokio::spawn(async move {
        let mut pending_assistant: Option<mycode_core::AssistantMessage> = None;
        let mut turn_usage = TurnUsage::default();
        loop {
            let event = match agent_rx.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = pump_events.send(BridgeEvent::ChatFailed {
                        session_id: pump_session_id.clone(),
                        message: "agent event stream lagged".to_owned(),
                    });
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            match event {
                mycode_core::events::AgentEvent::MessageDelta(
                    mycode_core::events::MessageDelta::TextDelta(delta),
                ) => {
                    let _ = pump_events.send(BridgeEvent::ChatText {
                        session_id: pump_session_id.clone(),
                        delta,
                    });
                }
                mycode_core::events::AgentEvent::MessageDelta(
                    mycode_core::events::MessageDelta::ThinkingDelta(delta),
                ) => {
                    let _ = pump_events.send(BridgeEvent::ChatThinking {
                        session_id: pump_session_id.clone(),
                        delta,
                    });
                }
                mycode_core::events::AgentEvent::MessageDelta(
                    mycode_core::events::MessageDelta::ToolCallDelta { .. },
                ) => {}
                mycode_core::events::AgentEvent::ToolStarted { call_id, name } => {
                    let spelling = call_id.to_string();
                    // The ToolCall event must commit before its result; the
                    // ledger's ordering check rejects results for calls that
                    // were never opened.
                    if let Err(error) = writer.open_call(&spelling, &name).await {
                        let _ = pump_events.send(BridgeEvent::ChatFailed {
                            session_id: pump_session_id.clone(),
                            message: render_error(error),
                        });
                        return;
                    }
                    let _ = pump_events.send(BridgeEvent::ToolStarted {
                        session_id: pump_session_id.clone(),
                        call_id: spelling,
                        name,
                    });
                }
                mycode_core::events::AgentEvent::ToolProgress { .. } => {}
                mycode_core::events::AgentEvent::ToolCompleted {
                    call_id,
                    result: tool_result,
                } => {
                    let Ok(payload) = serde_json::to_vec(&tool_result) else {
                        return;
                    };
                    match writer.close_call(call_id.as_str(), &payload).await {
                        Ok(event_id) => {
                            let entry = project_tool_result(&event_id, &payload);
                            let _ = pump_events.send(BridgeEvent::ToolCompleted {
                                session_id: pump_session_id.clone(),
                                entry,
                            });
                        }
                        Err(error) => {
                            let _ = pump_events.send(BridgeEvent::ChatFailed {
                                session_id: pump_session_id.clone(),
                                message: render_error(error),
                            });
                            return;
                        }
                    }
                }
                mycode_core::events::AgentEvent::MessageAdded(Message::Assistant(message)) => {
                    // Usage is reported per response cycle, so it has to be
                    // summed here: reading it off the closing message alone
                    // would bill a ten-step turn as one.
                    if let Some(usage) = message.usage.as_ref() {
                        turn_usage.fold(usage);
                    }
                    // A step that requests tools is not the end of the turn.
                    // Commit it now so the ledger and the transcript keep the
                    // model's real order instead of collapsing the turn into
                    // its last message.
                    let is_step = message
                        .blocks
                        .iter()
                        .any(|block| matches!(block, mycode_core::ContentBlock::ToolCall(_)));
                    if !is_step {
                        pending_assistant = Some(message);
                        continue;
                    }
                    let Ok(payload) = serde_json::to_vec(&message) else {
                        let _ = pump_events.send(BridgeEvent::ChatFailed {
                            session_id: pump_session_id.clone(),
                            message: "assistant step could not be encoded".to_owned(),
                        });
                        return;
                    };
                    match writer.write(EventKind::Message, &payload).await {
                        Ok(event_id) => {
                            let _ = pump_events.send(BridgeEvent::AssistantStep {
                                session_id: pump_session_id.clone(),
                                entry: project_assistant_from(&event_id, &payload),
                            });
                        }
                        Err(error) => {
                            let _ = pump_events.send(BridgeEvent::ChatFailed {
                                session_id: pump_session_id.clone(),
                                message: render_error(error),
                            });
                            return;
                        }
                    }
                }
                mycode_core::events::AgentEvent::MessageAdded(_) => {}
                mycode_core::events::AgentEvent::TurnStarted => {}
                mycode_core::events::AgentEvent::TurnEnded(outcome) => {
                    let Some(message) = pending_assistant.take() else {
                        // A cancelled mid-stream turn commits nothing; the
                        // UI resets quietly on the sentinel message.
                        let message =
                            if matches!(outcome, mycode_core::events::TurnOutcome::Aborted) {
                                CHAT_CANCELLED.to_owned()
                            } else {
                                "the turn ended without an assistant message".to_owned()
                            };
                        let _ = pump_events.send(BridgeEvent::ChatFailed {
                            session_id: pump_session_id.clone(),
                            message,
                        });
                        return;
                    };
                    match serde_json::to_vec(&message) {
                        Ok(payload) => {
                            match writer.write(EventKind::Message, &payload).await {
                                Ok(event_id) => {
                                    let entry = project_assistant_from(&event_id, &payload);
                                    if usage_enabled && turn_usage.seen {
                                        let elapsed_ms = turn_started.elapsed().as_millis() as u64;
                                        let usage_payload = serde_json::json!({
                                            "provider": usage_provider,
                                            "model": usage_model,
                                            "input": turn_usage.input,
                                            "output": turn_usage.output,
                                            "cache": turn_usage.cache,
                                            "elapsed_ms": elapsed_ms,
                                        });
                                        if let Ok(bytes) = serde_json::to_vec(&usage_payload)
                                            && let Ok(usage_event) =
                                                writer.write(EventKind::Usage, &bytes).await
                                        {
                                            let _ = pump_events.send(BridgeEvent::UsageRecorded {
                                                session_id: pump_session_id.clone(),
                                                provider: usage_provider.clone(),
                                                model: usage_model.clone(),
                                                input: turn_usage.input,
                                                output: turn_usage.output,
                                                cache: turn_usage.cache,
                                                elapsed_ms,
                                                entry: project_usage(&usage_event, &bytes),
                                            });
                                        }
                                    }
                                    // Sent after the trailing usage write so the
                                    // head the UI receives is the ledger's final
                                    // head; a stale head fails the next append's
                                    // compare-and-swap as "session unavailable".
                                    let _ = pump_events.send(BridgeEvent::ChatDone {
                                        session_id: pump_session_id.clone(),
                                        head: writer.head().await,
                                        entry,
                                    });
                                }
                                Err(error) => {
                                    let _ = pump_events.send(BridgeEvent::ChatFailed {
                                        session_id: pump_session_id.clone(),
                                        message: render_error(error),
                                    });
                                }
                            }
                        }
                        Err(_) => {
                            let _ = pump_events.send(BridgeEvent::ChatFailed {
                                session_id: pump_session_id.clone(),
                                message: "assistant message could not be encoded".to_owned(),
                            });
                        }
                    }
                    return;
                }
                mycode_core::events::AgentEvent::Error(error) => {
                    let _ = pump_events.send(BridgeEvent::ChatFailed {
                        session_id: pump_session_id.clone(),
                        message: format!("agent error: {error}"),
                    });
                    return;
                }
            }
        }
    });

    agent.seed_history(history);
    let env = mycode_agent::TurnEnv::new(&wire, &registry, &hooks)
        .with_cancel(cancel)
        .with_events(agent_tx)
        .with_cwd(cwd);
    let prompt_message = Message::User(prompt);
    let outcome = agent.prompt(prompt_message, &env).await;
    let _ = outcome
        .as_ref()
        .map_err(|error| format!("turn failed: {error}"))?;
    // The pump emits ChatDone/ChatFailed; wait for it to finish draining.
    let _ = pump.await;
    Ok(())
}

/// Removes one session's turn token from the cancel registry on scope exit.
struct CancelGuard {
    cancels: Arc<std::sync::Mutex<HashMap<String, CancellationToken>>>,
    session_id: String,
}

impl CancelGuard {
    fn register(
        cancels: Arc<std::sync::Mutex<HashMap<String, CancellationToken>>>,
        session_id: &str,
        token: &CancellationToken,
    ) -> Self {
        if let Ok(mut map) = cancels.lock() {
            map.insert(session_id.to_owned(), token.clone());
        }
        Self {
            cancels,
            session_id: session_id.to_owned(),
        }
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        // A contended lock leaks one stale entry; the next turn for the same
        // session replaces it, so the leak is bounded and harmless.
        if let Ok(mut map) = self.cancels.try_lock() {
            map.remove(&self.session_id);
        }
    }
}

/// Shared branch-head writer: the event pump and host-backed tools commit
/// through one CAS head.
#[derive(Clone)]
struct HeadWriter {
    service: SessionService,
    session: SessionId,
    branch: BranchId,
    head: Arc<tokio::sync::Mutex<HeadStamp>>,
    /// Provider tool-call ids mapped to their open ledger call identity.
    /// The ledger's ordering check requires every ToolResult to resolve a
    /// ToolCall event with the same identity, so the identity is minted
    /// once at ToolStarted and reused at ToolCompleted.
    calls: Arc<tokio::sync::Mutex<HashMap<String, SessionCallId>>>,
}

impl HeadWriter {
    fn new(service: SessionService, session: SessionId, branch: BranchId, head: HeadStamp) -> Self {
        Self {
            service,
            session,
            branch,
            head: Arc::new(tokio::sync::Mutex::new(head)),
            calls: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Opens the ledger call identity for one provider tool call and commits
    /// the ToolCall event (payload carries the tool name for replay).
    async fn open_call(&self, provider_call_id: &str, name: &str) -> Result<(), SessionError> {
        let identity = SessionCallId::generate().ok_or(SessionError::Corrupt)?;
        self.calls
            .lock()
            .await
            .insert(provider_call_id.to_owned(), identity.clone());
        let payload = serde_json::json!({ "name": name });
        let bytes = serde_json::to_vec(&payload).map_err(|_| SessionError::Corrupt)?;
        self.write_event(EventKind::ToolCall, Some(identity), &bytes)
            .await
            .map(|_| ())
    }

    /// Commits one ToolResult under the identity opened at ToolStarted.
    async fn close_call(
        &self,
        provider_call_id: &str,
        payload: &[u8],
    ) -> Result<String, SessionError> {
        let identity = self
            .calls
            .lock()
            .await
            .remove(provider_call_id)
            .ok_or(SessionError::InvalidArgument)?;
        self.write_event(EventKind::ToolResult, Some(identity), payload)
            .await
    }

    /// The current committed head spelling, for UI refresh after any
    /// trailing writes.
    async fn head(&self) -> String {
        let guard = self.head.lock().await;
        head_spelling(&guard)
    }

    /// Commits one payload of `kind` and returns (event id spelling, bytes).
    async fn write(&self, kind: EventKind, payload: &[u8]) -> Result<String, SessionError> {
        self.write_event(kind, None, payload).await
    }

    /// Commits one payload of `kind` under an explicit ledger call identity.
    async fn write_event(
        &self,
        kind: EventKind,
        ledger_call: Option<SessionCallId>,
        payload: &[u8],
    ) -> Result<String, SessionError> {
        let mut head = self.head.lock().await;
        let reservation = self
            .service
            .reserve_event(&self.session, &self.branch, kind, ledger_call, payload)
            .await?;
        let appended = self
            .service
            .append(&self.session, &self.branch, &head, &reservation)
            .await?;
        let event_id = appended
            .head
            .event()
            .cloned()
            .ok_or(SessionError::Corrupt)?;
        *head = HeadStamp::Event(event_id.clone());
        Ok(event_id.as_str().to_owned())
    }
}

/// Host channel forwarding `ask_user` waits through the UI event stream.
struct BridgeAskChannel {
    session_id: String,
    events: mpsc::Sender<BridgeEvent>,
    answer: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Vec<String>>>>,
}

#[async_trait::async_trait]
impl mycode_tools::builtin::AskChannel for BridgeAskChannel {
    async fn ask(
        &self,
        questions: &[mycode_tools::builtin::AskQuestion],
        cancel: &CancellationToken,
    ) -> Result<Vec<mycode_tools::builtin::AskAnswer>, mycode_tools::ToolError> {
        let rows = questions
            .iter()
            .map(|question| {
                (
                    question.question.clone(),
                    question.choices.clone(),
                    question.optional,
                )
            })
            .collect();
        let _ = self.events.send(BridgeEvent::AskRequested {
            session_id: self.session_id.clone(),
            questions: rows,
        });
        let rx = self
            .answer
            .lock()
            .await
            .take()
            .ok_or_else(mycode_tools::builtin::user_dismissed)?;
        let answers = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                return Err(mycode_tools::ToolError::Execution("cancelled".into()));
            }
            answers = rx => answers.map_err(|_| mycode_tools::builtin::user_dismissed())?,
        };
        Ok(questions
            .iter()
            .enumerate()
            .map(|(index, question)| mycode_tools::builtin::AskAnswer {
                question: question.question.clone(),
                answer: answers.get(index).cloned().unwrap_or_default(),
            })
            .collect())
    }
}

/// Persists `todo_write` payloads and mirrors them to the UI.
struct BridgeTodoStore {
    events: mpsc::Sender<BridgeEvent>,
    session_id: String,
    writer: HeadWriter,
    home: HomeLayout,
}

#[async_trait::async_trait]
impl mycode_tools::builtin::TodoStore for BridgeTodoStore {
    async fn store(
        &self,
        tasks: &[mycode_tools::builtin::TodoWireTask],
    ) -> Result<String, mycode_tools::ToolError> {
        // Resolve or mint stable ids, then validate the graph.
        let mut document = mycode_config::TodoDocument::default();
        for task in tasks {
            let id = match &task.id {
                Some(id) => id.clone(),
                None => mycode_config::new_todo_id().ok_or_else(|| {
                    mycode_tools::ToolError::Execution("id minting failed".into())
                })?,
            };
            let status = match task.status.as_str() {
                "pending" => mycode_config::TodoStatus::Pending,
                "in_progress" => mycode_config::TodoStatus::InProgress,
                _ => mycode_config::TodoStatus::Completed,
            };
            document.tasks.push(mycode_config::TodoTask {
                id,
                content: task.content.clone(),
                status,
                blocked_by: task.blocked_by.clone(),
            });
        }
        document
            .validate()
            .map_err(|error| mycode_tools::ToolError::Execution(error.to_string()))?;

        // CAS revision: read the current header.
        let revision = mycode_config::read_todo_revision(&self.home, &self.session_id)
            .map_err(|error| mycode_tools::ToolError::Execution(error.to_string()))?;
        mycode_config::replace_todo_document(&self.home, &self.session_id, revision, &document)
            .map_err(|error| mycode_tools::ToolError::Execution(error.to_string()))?;

        // Durable Task event on the branch.
        let payload = document
            .to_payload()
            .map_err(|error| mycode_tools::ToolError::Execution(error.to_string()))?;
        if let Err(error) = self.writer.write(EventKind::Task, &payload).await {
            return Err(mycode_tools::ToolError::Execution(render_error(error)));
        }

        let in_progress = document
            .tasks
            .iter()
            .filter(|task| matches!(task.status, mycode_config::TodoStatus::InProgress))
            .count();
        let completed = document
            .tasks
            .iter()
            .filter(|task| matches!(task.status, mycode_config::TodoStatus::Completed))
            .count();
        let _ = self.events.send(BridgeEvent::TodoUpdated {
            session_id: self.session_id.clone(),
            tasks: document
                .tasks
                .iter()
                .map(|task| {
                    (
                        task.content.clone(),
                        match task.status {
                            mycode_config::TodoStatus::Pending => "pending".to_owned(),
                            mycode_config::TodoStatus::InProgress => "in progress".to_owned(),
                            mycode_config::TodoStatus::Completed => "done".to_owned(),
                        },
                    )
                })
                .collect(),
        });
        Ok(format!(
            "stored {} tasks ({completed} done, {in_progress} in progress)",
            document.tasks.len()
        ))
    }
}

/// Projects a committed usage payload into a display entry.
/// Projects one replayed ledger event with the same projections the live
/// stream uses: assistant messages and tool results parse their typed
/// payloads instead of surfacing raw JSON, user messages stay plain text.
/// Token usage summed across every response cycle of one turn.
///
/// Providers report usage per cycle, so a turn that calls tools reports
/// several times. `seen` distinguishes "no usage reported" from a genuine
/// zero, which keeps the ledger from recording a usage event the provider
/// never sent.
#[derive(Debug, Default)]
struct TurnUsage {
    seen: bool,
    input: u64,
    output: u64,
    cache: Option<u64>,
}

impl TurnUsage {
    fn fold(&mut self, usage: &mycode_core::Usage) {
        self.seen = true;
        self.input = self.input.saturating_add(usage.input_tokens);
        self.output = self.output.saturating_add(usage.output_tokens);
        if let Some(cache) = usage.cache_read_tokens {
            self.cache = Some(self.cache.unwrap_or_default().saturating_add(cache));
        }
    }
}

fn project_replayed_entry(
    event: &mycode_agent::session::SessionEvent,
    payload: &[u8],
) -> ConversationEntry {
    match event.kind {
        EventKind::Message => {
            // Assistant messages are typed JSON; a parse miss means the
            // payload is the user's plain-text message.
            if serde_json::from_slice::<mycode_core::AssistantMessage>(payload).is_ok() {
                project_assistant_from(event.event_id.as_str(), payload)
            } else {
                ConversationEntry {
                    event_id: event.event_id.as_str().to_owned(),
                    kind: EntryKind::UserMessage,
                    text: decode_text(payload).into(),
                    call_id: None,
                    thinking: String::new(),
                }
            }
        }
        EventKind::ToolResult => project_tool_result(event.event_id.as_str(), payload),
        EventKind::ToolCall => {
            let value: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
            let name = value["name"]
                .as_str()
                .or_else(|| value["toolCall"]["name"].as_str())
                .unwrap_or("tool");
            ConversationEntry {
                event_id: event.event_id.as_str().to_owned(),
                kind: EntryKind::ToolCall,
                text: name.into(),
                call_id: event.call_id.as_ref().map(|call| call.as_str().to_owned()),
                thinking: String::new(),
            }
        }
        EventKind::Usage => project_usage(event.event_id.as_str(), payload),
        EventKind::Task => ConversationEntry {
            event_id: event.event_id.as_str().to_owned(),
            kind: EntryKind::Usage,
            text: "".into(),
            call_id: None,
            thinking: String::new(),
        },
    }
}

fn project_usage(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let value: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
    let model = value["model"].as_str().unwrap_or("unknown");
    let input = value["input"].as_u64().unwrap_or_default();
    let output = value["output"].as_u64().unwrap_or_default();
    let cache = value["cache"].as_u64();
    let elapsed_ms = value["elapsed_ms"].as_u64().unwrap_or_default();
    let mut text = format!("{model}: {input} in / {output} out");
    if elapsed_ms > 0 {
        let per_second = output as f64 / (elapsed_ms as f64 / 1000.0);
        text.push_str(&format!(" \u{b7} {per_second:.0} tok/s"));
    }
    if let Some(cache) = cache
        && input > 0
    {
        text.push_str(&format!(" \u{b7} {}% cached", cache * 100 / input));
    }
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::Usage,
        text: text.into(),
        call_id: None,
        thinking: String::new(),
    }
}

/// Projects a committed tool-result payload into a display entry.
fn project_tool_result(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let result: mycode_core::ToolResultMessage =
        serde_json::from_slice(payload).unwrap_or_else(|_| mycode_core::ToolResultMessage {
            tool_call_id: String::new(),
            content: Vec::new(),
            is_error: true,
            details: None,
        });
    let text: String = result
        .content
        .iter()
        .filter_map(|block| match block {
            mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::ToolResult,
        text: if result.is_error {
            format!("failed: {text}").into()
        } else {
            text.into()
        },
        call_id: Some(result.tool_call_id),
        thinking: String::new(),
    }
}

/// Projects a committed assistant payload into a display entry.
fn project_assistant_from(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let mut text = String::new();
    let mut thinking = String::new();
    if let Ok(message) = serde_json::from_slice::<mycode_core::AssistantMessage>(payload) {
        for block in &message.blocks {
            match block {
                mycode_core::ContentBlock::Text(block) => text.push_str(&block.text),
                mycode_core::ContentBlock::Thinking(block) => {
                    if !thinking.is_empty() {
                        thinking.push('\n');
                    }
                    thinking.push_str(&block.text);
                }
                mycode_core::ContentBlock::ToolCall(call) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("tool call {}", call.name));
                }
                _ => {}
            }
        }
    }
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::AssistantMessage,
        text: text.into(),
        call_id: None,
        thinking,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mycode_agent::session::EventKind;
    use mycode_core::{Provider as _, Request, StreamEvent};

    /// End-to-end over the real provider configured in the user's home:
    /// settings parse, vault key, resolve, stream, decode. Run explicitly
    /// with `cargo test -p mycode-desktop -- --ignored live_provider`.
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
    /// invalid request. Run with `cargo test -p mycode-desktop -- --ignored`.
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
        let text: String = reply
            .blocks
            .iter()
            .filter_map(|block| match block {
                mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect();
        assert!(!text.trim().is_empty(), "the follow-up reply has text");
    }

    /// Streams one request to its terminal assistant message (live helper).
    async fn collect_assistant(
        wire: &WireProvider,
        request: Request,
    ) -> mycode_core::AssistantMessage {
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

    /// Full tool-call round trip over a live endpoint on a
    /// `current_thread` runtime — the same executor topology the bridge
    /// uses. The session actor, checkpoint hook, tool dispatch, and ledger
    /// writes all run exactly as in production, so any synchronous blocking
    /// left in the path would deadlock this test and trip the timeout.
    ///
    /// Configure a compatible endpoint with env vars and run explicitly:
    /// `MYCODE_E2E_BASE_URL=http://host:port MYCODE_E2E_API_KEY=… \
    ///  MYCODE_E2E_MODEL=glm-5.3-flash MYCODE_E2E_KIND=anthropic-messages \
    ///  cargo test -p mycode-desktop -- --ignored live_gateway`
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "needs MYCODE_E2E_BASE_URL and MYCODE_E2E_API_KEY for a live endpoint"]
    async fn live_gateway_tool_call_round_trip() {
        let Ok(base_url) = std::env::var("MYCODE_E2E_BASE_URL") else {
            eprintln!("MYCODE_E2E_BASE_URL unset; skipping live gateway test");
            return;
        };
        let Ok(api_key) = std::env::var("MYCODE_E2E_API_KEY") else {
            eprintln!("MYCODE_E2E_API_KEY unset; skipping live gateway test");
            return;
        };
        let model =
            std::env::var("MYCODE_E2E_MODEL").unwrap_or_else(|_| "glm-5.3-flash".to_owned());
        let kind =
            std::env::var("MYCODE_E2E_KIND").unwrap_or_else(|_| "anthropic-messages".to_owned());

        let (_parent, layout) = home();
        let provider = ProviderSettings {
            id: "e2e".to_owned(),
            kind,
            base_url,
            models: vec![model.clone()],
            enabled: true,
            context_limit: None,
            max_output: None,
        };
        let resolved =
            ResolvedProvider::resolve(&provider, &model, &api_key, "mycode-e2e").expect("resolve");
        let transport = ReqwestTransport::new().expect("transport");
        let wire = WireProvider::new(resolved, Arc::new(transport));

        let registry = ToolRegistry::new();
        mycode_tools::register_builtins(&registry);

        let service = tokio::task::spawn_blocking({
            let layout = layout.clone();
            move || SessionService::new(&layout)
        })
        .await
        .expect("service start");
        let created = service.create().await.expect("session created");
        let session = created.session_id;
        let branch = created.branch_id;

        let cwd = layout.root().join("project");
        std::fs::create_dir_all(&cwd).expect("project dir");

        let prompt_text = "Use the write tool to create the file hello-e2e.txt \
                           containing exactly the text hi. Then reply with the word DONE.";
        let (head_text, _entry) =
            send_message(&service, &session, &branch, &HeadStamp::Empty, prompt_text)
                .await
                .expect("prompt committed");
        let head = HeadStamp::Event(
            mycode_agent::session::SessionEventId::parse(&head_text).expect("head id"),
        );
        let writer = HeadWriter::new(service.clone(), session.clone(), branch.clone(), head);

        let (agent_tx, mut agent_rx) = tokio::sync::broadcast::channel(256);
        let checkpoint_home = layout.clone();
        let checkpoint_cwd = cwd.clone();
        let checkpoint_session = session.as_str().to_owned();
        let hooks = HookRunner::default().with_before_tool(move |tool, args| {
            let raw_path = matches!(tool, "write" | "edit")
                .then(|| {
                    args.get("path")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })
                .flatten();
            let checkpoint_home = checkpoint_home.clone();
            let checkpoint_cwd = checkpoint_cwd.clone();
            let checkpoint_session = checkpoint_session.clone();
            async move {
                let Some(raw_path) = raw_path else { return };
                let path = checkpoint_cwd.join(raw_path);
                let _ = tokio::task::spawn_blocking(move || {
                    mycode_config::checkpoint_file(&checkpoint_home, &checkpoint_session, &path)
                })
                .await;
            }
        });

        // Ledger pump: mirror run_chat_turn's ordering guarantees —
        // ToolCall commits before its ToolResult, the assistant message
        // commits at TurnEnded.
        let pump_writer = writer.clone();
        let pump = tokio::spawn(async move {
            let mut pending: Option<mycode_core::AssistantMessage> = None;
            let mut tools = 0usize;
            loop {
                match agent_rx.recv().await {
                    Ok(mycode_core::events::AgentEvent::ToolStarted { call_id, name }) => {
                        pump_writer
                            .open_call(&call_id.to_string(), &name)
                            .await
                            .expect("tool call opened");
                        tools += 1;
                    }
                    Ok(mycode_core::events::AgentEvent::ToolCompleted { call_id, result }) => {
                        let payload = serde_json::to_vec(&result).expect("result encodes");
                        pump_writer
                            .close_call(call_id.as_str(), &payload)
                            .await
                            .expect("tool result committed");
                    }
                    Ok(mycode_core::events::AgentEvent::MessageAdded(Message::Assistant(
                        message,
                    ))) => {
                        pending = Some(message);
                    }
                    Ok(mycode_core::events::AgentEvent::TurnEnded(_)) => {
                        let message = pending.take().expect("assistant message committed");
                        let payload = serde_json::to_vec(&message).expect("message encodes");
                        pump_writer
                            .write(EventKind::Message, &payload)
                            .await
                            .expect("assistant message committed");
                        return tools;
                    }
                    Ok(_) => {}
                    Err(_) => return tools,
                }
            }
        });

        let mut agent =
            Agent::new(AgentConfig::new().with_system_prompt(
                "You are a coding agent. Use the provided tools for file work.",
            ));
        let cancel = CancellationToken::new();
        let env = mycode_agent::TurnEnv::new(&wire, &registry, &hooks)
            .with_cancel(cancel)
            .with_events(agent_tx)
            .with_cwd(cwd.clone());
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            agent.prompt(
                Message::User(mycode_core::UserMessage::text(prompt_text)),
                &env,
            ),
        )
        .await
        .expect("turn completed within the timeout — a hang means a blocking call remains");
        outcome.expect("model turn succeeded");

        let tools = tokio::time::timeout(std::time::Duration::from_secs(10), pump)
            .await
            .expect("ledger pump drained")
            .expect("pump task finished");
        assert!(tools > 0, "the model issued at least one tool call");
        assert_eq!(
            std::fs::read_to_string(cwd.join("hello-e2e.txt")).expect("tool wrote the file"),
            "hi"
        );
        service.clone().shutdown().await;
    }

    #[test]
    fn worktree_lease_acquires_and_releases() {
        let (_parent, layout) = home();
        let repo = std::env::current_dir().expect("cwd");
        // Only meaningful inside a git checkout; skip elsewhere.
        if !repo.join(".git").exists() {
            return;
        }
        let lease = crate::subagent::WorktreeLease::acquire(&layout, &repo).expect("lease");
        assert!(lease.path.is_dir());
        let manifest = lease.manifest.clone();
        let checkout = lease.path.clone();
        assert!(manifest.exists());
        lease.release();
        assert!(!manifest.exists());
        assert!(!checkout.exists());
    }

    #[tokio::test]
    async fn ledger_history_preserves_tool_traffic() {
        let (_parent, layout) = home();
        let service = SessionService::new(&layout);
        let created = service.create().await.expect("session created");
        let session = created.session_id;
        let branch = created.branch_id;
        let writer = HeadWriter::new(
            service.clone(),
            session.clone(),
            branch.clone(),
            HeadStamp::Empty,
        );

        // A tool-using turn: user ask, assistant tool_use, tool result,
        // usage bookkeeping, then the next user message.
        writer
            .write(EventKind::Message, b"list the rust files")
            .await
            .expect("user commit");
        let assistant = mycode_core::AssistantMessage {
            blocks: vec![mycode_core::ContentBlock::ToolCall(
                mycode_core::ToolCall::new(
                    "call-1",
                    "find",
                    serde_json::json!({"pattern": "*.rs"}),
                ),
            )],
            usage: None,
            stop_reason: mycode_core::StopReason::ToolUse,
        };
        writer
            .write(
                EventKind::Message,
                &serde_json::to_vec(&assistant).expect("assistant json"),
            )
            .await
            .expect("assistant commit");
        let tool_result = mycode_core::ToolResultMessage {
            tool_call_id: "call-1".to_owned(),
            content: vec![mycode_core::ContentBlock::Text(
                mycode_core::TextBlock::new("src/main.rs"),
            )],
            is_error: false,
            details: None,
        };
        writer
            .open_call("call-1", "find")
            .await
            .expect("tool call commit");
        let result_id = writer
            .close_call(
                "call-1",
                &serde_json::to_vec(&tool_result).expect("result json"),
            )
            .await
            .expect("tool result commit");
        writer
            .write(
                EventKind::Usage,
                br#"{"provider":"p","model":"m","input_tokens":1,"output_tokens":2}"#,
            )
            .await
            .expect("usage commit");
        let head = HeadStamp::Event(
            mycode_agent::session::SessionEventId::parse(&result_id).expect("head id"),
        );

        let history = ledger_history(&service, &session, &branch, &head)
            .await
            .expect("history read");
        assert_eq!(history.len(), 3, "usage events never reach the model");
        assert!(matches!(
            &history[0],
            Message::User(user) if user_content_text(user) == "list the rust files"
        ));
        match &history[1] {
            Message::Assistant(assistant) => match assistant.blocks.first() {
                Some(mycode_core::ContentBlock::ToolCall(call)) => {
                    assert_eq!(call.id, "call-1");
                    assert_eq!(call.name, "find");
                }
                other => panic!("expected a tool_use block, got {other:?}"),
            },
            other => panic!("expected an assistant message, got {other:?}"),
        }
        match &history[2] {
            Message::ToolResult(result) => assert_eq!(result.tool_call_id, "call-1"),
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ledger_history_strips_thinking_for_cross_model_replay() {
        let (_parent, layout) = home();
        let service = SessionService::new(&layout);
        let created = service.create().await.expect("session created");
        let session = created.session_id;
        let branch = created.branch_id;
        let writer = HeadWriter::new(
            service.clone(),
            session.clone(),
            branch.clone(),
            HeadStamp::Empty,
        );
        writer
            .write(EventKind::Message, b"hello")
            .await
            .expect("user commit");
        // A thinking assistant reply carrying a model-bound signature: the
        // provider rejects replaying it to a different model.
        let mut thinking = mycode_core::ThinkingBlock::new("let me think");
        thinking.signature = Some("sig-m2".to_owned());
        let assistant = mycode_core::AssistantMessage {
            blocks: vec![
                mycode_core::ContentBlock::Thinking(thinking),
                mycode_core::ContentBlock::Text(mycode_core::TextBlock::new("hi there")),
            ],
            usage: None,
            stop_reason: mycode_core::StopReason::Stop,
        };
        let last = writer
            .write(
                EventKind::Message,
                &serde_json::to_vec(&assistant).expect("assistant json"),
            )
            .await
            .expect("assistant commit");
        let head =
            HeadStamp::Event(mycode_agent::session::SessionEventId::parse(&last).expect("head id"));
        let history = ledger_history(&service, &session, &branch, &head)
            .await
            .expect("history read");
        match &history[1] {
            Message::Assistant(replayed) => {
                assert!(
                    replayed
                        .blocks
                        .iter()
                        .all(|block| !matches!(block, mycode_core::ContentBlock::Thinking(_))),
                    "thinking never replays across turns"
                );
                assert!(matches!(
                    &replayed.blocks[0],
                    mycode_core::ContentBlock::Text(text) if text.text == "hi there"
                ));
            }
            other => panic!("expected an assistant message, got {other:?}"),
        }
    }

    /// Flat text of a user message's content blocks (test helper).
    fn user_content_text(user: &mycode_core::UserMessage) -> String {
        user.content
            .iter()
            .filter_map(|block| match block {
                mycode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn project_assistant_keeps_thinking_for_the_timeline() {
        let mut thinking = mycode_core::ThinkingBlock::new("let me think");
        thinking.signature = Some("sig".to_owned());
        let assistant = mycode_core::AssistantMessage {
            blocks: vec![
                mycode_core::ContentBlock::Thinking(thinking),
                mycode_core::ContentBlock::Text(mycode_core::TextBlock::new("hi there")),
            ],
            usage: None,
            stop_reason: mycode_core::StopReason::Stop,
        };
        let entry = project_assistant_from(
            "evt1",
            &serde_json::to_vec(&assistant).expect("assistant json"),
        );
        assert_eq!(entry.kind, EntryKind::AssistantMessage);
        assert_eq!(entry.text.as_ref(), "hi there");
        assert_eq!(entry.thinking, "let me think");
    }

    #[test]
    fn system_prompt_lists_web_search_and_fetch() {
        struct StubWeb;
        #[async_trait::async_trait]
        impl mycode_tools::builtin::WebHost for StubWeb {
            async fn search(
                &self,
                _query: &str,
                _max_results: usize,
                _cancel: &CancellationToken,
            ) -> Result<Vec<mycode_tools::builtin::WebHit>, mycode_tools::ToolError> {
                Ok(Vec::new())
            }
            async fn fetch_content(
                &self,
                _urls: &[String],
                _cancel: &CancellationToken,
            ) -> Result<Vec<mycode_tools::builtin::WebPage>, mycode_tools::ToolError> {
                Ok(Vec::new())
            }
        }
        let registry = ToolRegistry::new();
        mycode_tools::register_builtins(&registry);
        let host: Arc<dyn mycode_tools::builtin::WebHost> = Arc::new(StubWeb);
        registry.register(Arc::new(mycode_tools::builtin::WebSearchTool::new(
            host.clone(),
        )));
        registry.register(Arc::new(mycode_tools::builtin::FetchContentTool::new(host)));
        let prompt = mycode_agent::build_system_prompt(&registry);
        assert!(prompt.contains("web_search"), "{prompt}");
        assert!(prompt.contains("fetch_content"), "{prompt}");
    }

    fn home() -> (tempfile::TempDir, HomeLayout) {
        let parent = tempfile::tempdir().expect("parent");
        let layout = HomeLayout::from_root(parent.path().join("home")).expect("layout");
        (parent, layout)
    }

    #[test]
    fn copilot_provider_upsert_binds_models_and_refreshes_in_place() {
        // SessionService::new starts background workers that need a reactor.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(async {
                let (parent, layout) = home();
                let state =
                    CoreState::new(layout.clone(), mycode_providers::catalog::current(&layout));
                upsert_copilot_provider(&state, &["gpt-x".to_owned(), "claude-y".to_owned()])
                    .expect("first upsert");
                let settings = read_app_settings(&layout).expect("settings");
                let provider = settings
                    .providers
                    .iter()
                    .find(|provider| provider.id == COPILOT_PROVIDER_ID)
                    .expect("provider row");
                assert!(provider.enabled);
                assert_eq!(
                    provider.kind,
                    mycode_providers::catalog::KIND_OPENAI_COMPLETIONS
                );
                assert_eq!(provider.base_url, "https://api.githubcopilot.com");
                assert_eq!(
                    provider.models,
                    vec!["gpt-x".to_owned(), "claude-y".to_owned()]
                );

                // An empty selection falls back to the catalog's tool-calling
                // models.
                upsert_copilot_provider(&state, &[]).expect("default models");
                let settings = read_app_settings(&layout).expect("settings reread");
                let provider = settings
                    .providers
                    .iter()
                    .find(|provider| provider.id == COPILOT_PROVIDER_ID)
                    .expect("row");
                assert!(!provider.models.is_empty());
                assert!(provider.models.len() <= 6);

                assert_eq!(
                    settings
                        .providers
                        .iter()
                        .filter(|provider| provider.id == COPILOT_PROVIDER_ID)
                        .count(),
                    1,
                    "refresh updates the single row instead of duplicating"
                );
                drop(state);
                drop(parent);
            });
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
    fn project_file_search_skips_noise_and_sorts_shortest_first() {
        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("proj");
        for path in [
            "src/main.rs",
            "src/lib.rs",
            "src/deep/nested/mod.rs",
            "node_modules/skip.js",
            ".git/config",
            "README.md",
        ] {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("dirs");
            std::fs::write(&full, "x").expect("seed");
        }

        let hits = search_project_files(&root, "");
        assert_eq!(
            hits,
            vec![
                "README.md".to_owned(),
                "src/lib.rs".to_owned(),
                "src/main.rs".to_owned(),
                "src/deep/nested/mod.rs".to_owned(),
            ]
        );
        let hits = search_project_files(&root, "MAIN");
        assert_eq!(hits, vec!["src/main.rs".to_owned()]);
        let hits = search_project_files(&root, "mod");
        assert_eq!(hits, vec!["src/deep/nested/mod.rs".to_owned()]);
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
        assert_eq!(conversation.entries[0].text.as_ref(), "hello core");
        assert_eq!(conversation.head, head);

        let BridgeReply::Settings(settings) = drive(&bridge, BridgeCommand::LoadSettings) else {
            panic!("settings reply");
        };
        let (document, revision, key_ids, mcp_ids) = settings.expect("settings load");
        assert!(key_ids.is_empty() && mcp_ids.is_empty());
        assert_eq!(revision, AuthorityRevision::ABSENT);
        assert!(document.providers.is_empty());
        assert!(document.effective_user_agent().starts_with("pi ("));

        let BridgeReply::SettingsSaved(saved) = drive(
            &bridge,
            BridgeCommand::SaveSettings {
                expected_revision: revision,
                settings: AppSettings {
                    user_agent: "mycode-desktop-test/1".to_owned(),
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
        let (provider_keys, mcp_keys) = saved.expect("key saved");
        assert_eq!(provider_keys, vec!["openai-main".to_owned()]);
        assert!(mcp_keys.is_empty());

        let BridgeReply::Settings(reloaded) = drive(&bridge, BridgeCommand::LoadSettings) else {
            panic!("reloaded reply");
        };
        let (document, revision, key_ids, _mcp_ids) = reloaded.expect("settings reload");
        assert_eq!(key_ids, vec!["openai-main".to_owned()]);
        assert_eq!(document.user_agent, "mycode-desktop-test/1");
        assert_eq!(revision.get(), 1);

        bridge.shutdown();
        let _ = EventKind::Message;
    }

    #[test]
    fn delete_session_covers_a_lazily_created_footprint() {
        fn create(bridge: &CoreBridge) -> String {
            let BridgeReply::Created(created) = drive(bridge, BridgeCommand::CreateSession) else {
                panic!("created reply");
            };
            created.expect("created").session_id
        }

        fn delete(bridge: &CoreBridge, session_id: &str) -> Result<(), String> {
            let BridgeReply::SessionDeleted(result) = drive(
                bridge,
                BridgeCommand::DeleteSession {
                    session_id: session_id.to_owned(),
                },
            ) else {
                panic!("deleted reply");
            };
            result
        }

        fn paths(layout: &HomeLayout, session_id: &str) -> (PathBuf, PathBuf) {
            (
                layout
                    .root()
                    .join(mycode_config::SESSIONS_DIR)
                    .join(session_id),
                layout.root().join("checkpoints").join(session_id),
            )
        }

        let (_parent, layout) = home();
        let (mut bridge, _events) = CoreBridge::start(layout.clone());

        // A conversation that never ran tools owns only its ledger directory.
        let session_id = create(&bridge);
        let (ledger, checkpoints) = paths(&layout, &session_id);
        assert!(ledger.is_dir());
        assert!(!checkpoints.exists());
        assert_eq!(delete(&bridge, &session_id), Ok(()));
        assert!(!ledger.exists());
        // Deleting twice is a no-op, not a failure.
        assert_eq!(delete(&bridge, &session_id), Ok(()));

        // Checkpoint directories disappear with the rest.
        let session_id = create(&bridge);
        let (ledger, checkpoints) = paths(&layout, &session_id);
        std::fs::create_dir_all(&checkpoints).expect("checkpoints dir");
        assert_eq!(delete(&bridge, &session_id), Ok(()));
        assert!(!ledger.exists() && !checkpoints.exists());

        bridge.shutdown();
    }

    #[test]
    fn head_spelling_and_text_decode_are_lossy_safe() {
        assert_eq!(head_spelling(&HeadStamp::Empty), "empty");
        assert_eq!(decode_text(b"abc"), "abc");
        assert_eq!(decode_text(&[0xff, 0xfe]), "(binary payload, 2 bytes)");
    }
}
