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
//!
//! The command channel is a tokio unbounded channel: the worker loop must
//! `await` commands instead of blocking the runtime thread, or spawned tasks
//! (chat turns, catalog refreshes) would starve until the next command
//! arrives. `UnboundedSender::send` is synchronous, so the UI side never
//! touches async machinery.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;

use mcode_agent::{Agent, AgentConfig, HookRunner};
use mcode_catalog::{
    CachedCatalog, CatalogDocument, DEFAULT_MAX_AGE_SECS, RefreshOutcome, http_client,
};
use mcode_config::{
    AppSettings, AuthorityRevision, HomeLayout, UiState, read_app_settings, read_provider_secrets,
    read_ui_state, replace_app_settings, replace_provider_secrets, replace_ui_state,
};
use mcode_core::Message;
use mcode_provider_api::{Provider as _, Request, StreamEvent};
use mcode_providers::{ReqwestTransport, ResolvedProvider, WireProvider};
use mcode_session::session::{
    self, BranchId, EventKind, HeadStamp, SessionCallId, SessionError, SessionId, SessionService,
};
use mcode_tools::ToolRegistry;
use mcode_updates::{PreparedUpdate, UpdateOffer};
use mcode_web::SearchResult;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::view_model::{ActiveConversation, ConversationEntry, EntryKind, SessionSummary};

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
    /// Run one bounded web search over the enabled backend.
    WebSearch {
        /// The search query.
        query: String,
    },
    /// List tools exposed by one enabled MCP server.
    McpListTools {
        /// Server identity from settings.
        server_id: String,
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
        /// Existing directory; `None` reverts to the default workspace.
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
    /// The turn finished and its assistant message was committed.
    ChatDone {
        /// Session identity spelling.
        session_id: String,
        /// New branch head spelling.
        head: String,
        /// Committed assistant entry projection.
        entry: ConversationEntry,
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
    /// Provider key save result.
    ProviderKeySaved(Result<(), String>),
    /// Chat turn acceptance; streaming continues over the event channel.
    ChatStarted(Result<(), String>),
    /// Web search result list.
    WebSearched(Result<Vec<SearchResult>, String>),
    /// MCP tools listing for one server.
    McpTools(Result<(String, Vec<String>), String>),
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
    events: mpsc::SyncSender<BridgeEvent>,
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
        // The state lives behind one Arc: `SessionService` retires its
        // publication when a clone is dropped, so clones must share one
        // instance instead of duplicating it.
        // Subagent worktree leases from a crashed process are recovered
        // before any new turn can run.
        recover_task_worktrees(&home);
        let state = Arc::new(CoreState::new(home));
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
                    history,
                } => {
                    let task = chat_turn(
                        state.clone(),
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
                            mcode_updates::latest_release(&client).await,
                        ));
                    });
                }
                BridgeCommand::DownloadUpdate { offer } => {
                    let reply = with_reply.reply;
                    tokio::spawn(async move {
                        let outcome = match http_client(UPDATE_USER_AGENT) {
                            Ok(client) => mcode_updates::download_update(&client, &offer).await,
                            Err(message) => Err(message),
                        };
                        let _ = reply.send(BridgeReply::UpdateDownloaded(outcome));
                    });
                }
                command => {
                    let outcome = handle(&state, &command).await;
                    if with_reply.reply.send(outcome).is_err() {
                        continue;
                    }
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
}

impl CoreState {
    fn new(home: HomeLayout) -> Self {
        let cached = mcode_catalog::current(&home);
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
        }
    }

    /// The tool working directory for one session.
    fn project_dir(&self, session_id: &str) -> PathBuf {
        self.projects
            .lock()
            .expect("projects")
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| self.home.root().join("workspace").join(session_id))
    }
}

/// Update checks identify the app to GitHub's API.
const UPDATE_USER_AGENT: &str = concat!("mcode-updates/", env!("CARGO_PKG_VERSION"));

/// Refreshes the provider catalog and reports a successful swap.
async fn refresh_catalog(
    state: &CoreState,
    events: &mpsc::SyncSender<BridgeEvent>,
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
    let outcome = mcode_catalog::refresh(&state.home, &client, force, DEFAULT_MAX_AGE_SECS).await;
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
fn spawn_catalog_refresh(state: Arc<CoreState>, events: mpsc::SyncSender<BridgeEvent>) {
    tokio::spawn(async move {
        refresh_catalog(&state, &events, false).await;
    });
}

/// One background update check shortly after startup.
fn spawn_update_check(state: Arc<CoreState>, events: mpsc::SyncSender<BridgeEvent>) {
    tokio::spawn(async move {
        let Ok(ui_state) = read_ui_state(&state.home) else {
            return;
        };
        if !ui_state.auto_update {
            return;
        }
        let Ok(client) = http_client(UPDATE_USER_AGENT) else {
            return;
        };
        if let Ok(Some(offer)) = mcode_updates::latest_release(&client).await {
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
        BridgeCommand::WebSearch { .. } => BridgeReply::WebSearched(Err(message)),
        BridgeCommand::McpListTools { .. } => BridgeReply::McpTools(Err(message)),
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
    }
}

async fn handle(state: &CoreState, command: &BridgeCommand) -> BridgeReply {
    match command {
        BridgeCommand::ListSessions => BridgeReply::Sessions(
            inspect_summaries(&state.service, &state.home)
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
        BridgeCommand::LoadSettings => BridgeReply::Settings(load_settings(&state.home)),
        BridgeCommand::SaveSettings {
            expected_revision,
            settings,
        } => BridgeReply::SettingsSaved(save_settings(&state.home, *expected_revision, settings)),
        BridgeCommand::SaveProviderKey {
            provider_id,
            api_key,
        } => BridgeReply::ProviderKeySaved(save_provider_key(&state.home, provider_id, api_key)),
        BridgeCommand::ChatTurn { .. } => {
            BridgeReply::ChatStarted(Err("chat turns run as concurrent tasks".to_owned()))
        }
        BridgeCommand::WebSearch { query } => {
            BridgeReply::WebSearched(web_search(&state.home, query).await)
        }
        BridgeCommand::McpListTools { server_id } => {
            BridgeReply::McpTools(mcp_list_tools(&state.home, server_id).await)
        }
        BridgeCommand::RollbackWorkspace { session_id } => BridgeReply::RolledBack(
            mcode_config::rollback_session(&state.home, session_id)
                .map_err(|error| render_config_error(&error)),
        ),
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
            BridgeReply::SessionDeleted(delete_session(&state.home, session_id))
        }
        BridgeCommand::RemoveRecent { project } => {
            let mut ui_state = match read_ui_state(&state.home) {
                Ok(ui_state) => ui_state,
                Err(error) => {
                    return BridgeReply::UiStateSaved(Err(render_config_error(&error)));
                }
            };
            ui_state.remove_recent(project);
            match replace_ui_state(&state.home, &ui_state) {
                Ok(()) => BridgeReply::UiStateSaved(Ok(())),
                Err(error) => BridgeReply::UiStateSaved(Err(render_config_error(&error))),
            }
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
            BridgeReply::Resources(list_resources(state, session_id))
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
                document: Arc::new(mcode_catalog::bundled().clone()),
                fetched_at: 0,
            }))),
        BridgeCommand::LoadUiState => BridgeReply::UiState(
            read_ui_state(&state.home).map_err(|error| render_config_error(&error)),
        ),
        BridgeCommand::SaveUiState { state: ui_state } => BridgeReply::UiStateSaved(
            replace_ui_state(&state.home, ui_state).map_err(|error| render_config_error(&error)),
        ),
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

/// Discovers prompt resources for one session workspace.
fn list_resources(state: &CoreState, session_id: &str) -> Result<Vec<(String, String)>, String> {
    let workspace = state.project_dir(session_id);
    let files = mcode_config::discover_resources(&state.home, &workspace);
    Ok(files
        .iter()
        .map(|file| {
            (
                file.name.clone(),
                file.path.as_os_str().to_string_lossy().into_owned(),
            )
        })
        .collect())
}

/// Lists tools of one enabled MCP server over stdio or HTTP.
///
/// Runs on the caller's runtime; never builds a nested one (a nested
/// `Runtime::block_on` panics and takes the core thread down with it).
async fn mcp_list_tools(
    home: &HomeLayout,
    server_id: &str,
) -> Result<(String, Vec<String>), String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let server = settings
        .mcp_servers
        .iter()
        .find(|server| server.id == server_id && server.enabled)
        .ok_or_else(|| "MCP server not found or disabled in settings".to_owned())?;
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
    let api_key = secrets.key(&format!("mcp-{server_id}")).map(str::to_owned);
    let timeout = mcode_mcp::DEFAULT_REQUEST_TIMEOUT;
    let channel: Arc<dyn mcode_mcp::JsonRpcChannel> = match server.transport.as_str() {
        "stdio" => {
            let command = server
                .command
                .as_deref()
                .ok_or("stdio server is missing its command")?;
            Arc::new(
                mcode_mcp::StdioChannel::spawn(command, &server.args, timeout)
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
                mcode_mcp::HttpChannel::new(
                    endpoint,
                    mcode_mcp::HttpChannelOptions {
                        key_header: mcode_mcp::KeyHeader::parse(server.key_header.as_deref()),
                        api_key,
                        timeout,
                    },
                )
                .map_err(|error| format!("MCP channel failed: {error}"))?,
            )
        }
        _ => return Err("unknown MCP transport".to_owned()),
    };
    let mut client = mcode_mcp::McpClient::new(channel);
    client
        .initialize()
        .await
        .map_err(|error| format!("MCP handshake failed: {error}"))?;
    let tools = client
        .list_tools()
        .await
        .map_err(|error| format!("MCP tools listing failed: {error}"))?;
    Ok((
        server_id.to_owned(),
        tools.iter().map(|tool| tool.name.clone()).collect(),
    ))
}

/// Runs one bounded search over the enabled backend, if any.
///
/// Runs on the caller's runtime; never builds a nested one.
async fn web_search(home: &HomeLayout, query: &str) -> Result<Vec<SearchResult>, String> {
    let client = web_client(home)?;
    client
        .search(query, 8, tokio_util::sync::CancellationToken::new())
        .await
        .map_err(|error| format!("search failed: {error}"))
}

/// Builds the web client for the enabled backend.
fn web_client(home: &HomeLayout) -> Result<mcode_web::WebClient, String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let backend = settings
        .web
        .backends
        .iter()
        .find(|backend| backend.enabled)
        .ok_or_else(|| "no enabled search backend — add one in Settings".to_owned())?;
    let transport = mcode_web::reqwest_transport::ReqwestWebTransport::new()
        .map_err(|_| "web transport unavailable".to_owned())?;
    mcode_web::WebClient::new(&backend.endpoint, std::sync::Arc::new(transport))
        .map_err(|_| "the search backend endpoint violates the URL policy".to_owned())
}

/// Host channel for the model's web tools: the same bounded client the
/// settings page configures.
struct BridgeWebHost {
    home: HomeLayout,
}

#[async_trait::async_trait]
impl mcode_tools::builtin::WebHost for BridgeWebHost {
    async fn search(
        &self,
        query: &str,
        max_results: usize,
        cancel: &CancellationToken,
    ) -> Result<Vec<mcode_tools::builtin::WebHit>, mcode_tools::ToolError> {
        let fail = |message: String| mcode_tools::ToolError::Execution(message);
        let client = web_client(&self.home).map_err(fail)?;
        let results = client
            .search(query, max_results, cancel.clone())
            .await
            .map_err(|error| fail(format!("search failed: {error}")))?;
        Ok(results
            .into_iter()
            .map(|result| mcode_tools::builtin::WebHit {
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
    ) -> Result<Vec<mcode_tools::builtin::WebPage>, mcode_tools::ToolError> {
        let fail = |message: String| mcode_tools::ToolError::Execution(message);
        let client = web_client(&self.home).map_err(fail)?;
        let pages = client
            .contents(urls, cancel.clone())
            .await
            .map_err(|error| fail(format!("fetch failed: {error}")))?;
        Ok(pages
            .into_iter()
            .map(|page| mcode_tools::builtin::WebPage {
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
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
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

/// Lists sessions with display titles: the first user message of each root
/// branch, first page only. Per-session read failures degrade to an empty
/// title; the listing itself never fails on one bad session.
async fn inspect_summaries(
    service: &SessionService,
    home: &HomeLayout,
) -> Result<Vec<SessionSummary>, SessionError> {
    let snapshots = session::inspect_sessions(home)?;
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
    let mut after: Option<mcode_session::session::SessionEventId> = None;
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

/// Rewinds the branch so everything from the recalled message onward is
/// gone, then returns the truncated conversation plus the edited text.
async fn recall_message(
    service: &SessionService,
    session: &SessionId,
    branch: &BranchId,
    _expected_head: &HeadStamp,
    to_event: &str,
) -> Result<ActiveConversation, String> {
    let target = mcode_session::session::SessionEventId::parse(to_event)
        .ok_or_else(|| "the rewind target is not a valid event id".to_owned())?;
    let reservation = service
        .reserve_branch(
            session,
            mcode_session::session::BranchMutationKind::Rewind,
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

/// Deletes one session's durable footprint: ledger, todos, compaction
/// checkpoint, and file snapshots. The ids are plain names by construction.
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
            .join("plugins/session/data/sessions")
            .join(session_id),
        home.root().join("workspace").join(session_id),
        home.root().join("checkpoints").join(session_id),
    ];
    for root in roots {
        std::fs::remove_dir_all(&root).map_err(|error| format!("delete: {error}"))?;
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
    state: Arc<CoreState>,
    events: mpsc::SyncSender<BridgeEvent>,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: String,
    model: String,
    history: Vec<Message>,
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
    state: &CoreState,
    events: &mpsc::SyncSender<BridgeEvent>,
    session_id: &str,
    session: SessionId,
    branch: BranchId,
    expected_head: HeadStamp,
    provider_id: &str,
    model: &str,
    cwd: PathBuf,
    history: &[Message],
) -> Result<(), String> {
    let home = &state.home;
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
    let wire = WireProvider::new(resolved.clone(), Arc::new(transport));

    // The tool working directory is the bound project (created on demand).
    std::fs::create_dir_all(&cwd).map_err(|error| format!("workspace dir: {error}"))?;
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
    let registry = Arc::new({
        let registry = ToolRegistry::new();
        mcode_tools::register_builtins(&registry);
        // ask_user rides the same registry; its channel forwards questions
        // to the UI over the event channel and waits on the shared router.
        let ask_events = events.clone();
        let ask_session = session_id.to_owned();
        let answer_rx = ASK_ROUTER
            .get_or_init(AskRouter::default)
            .register(&ask_session);
        let channel: Arc<dyn mcode_tools::builtin::AskChannel> = Arc::new(BridgeAskChannel {
            session_id: ask_session,
            events: ask_events,
            answer: tokio::sync::Mutex::new(Some(answer_rx)),
        });
        registry.register(Arc::new(mcode_tools::builtin::AskTool::new(channel)));
        // task delegates scoped work to a subagent on the same provider;
        // bounded slots, timeout, and worktree leases live in the host.
        registry.register(Arc::new(mcode_tools::builtin::TaskTool::new(Arc::new(
            BridgeTaskHost {
                resolved: resolved.clone(),
                home: home.clone(),
                cwd: cwd.clone(),
            },
        ))));
        // The model's web tools ride the same settings-configured backend.
        let web_host: Arc<dyn mcode_tools::builtin::WebHost> =
            Arc::new(BridgeWebHost { home: home.clone() });
        registry.register(Arc::new(mcode_tools::builtin::WebSearchTool::new(
            web_host.clone(),
        )));
        registry.register(Arc::new(mcode_tools::builtin::FetchContentTool::new(
            web_host,
        )));
        // todo_write persists the plan and appends a durable Task event.
        let todo_events = events.clone();
        let todo_session = session_id.to_owned();
        let todo_writer = writer.clone();
        let todo_home = home.clone();
        let store: Arc<dyn mcode_tools::builtin::TodoStore> = Arc::new(BridgeTodoStore {
            events: todo_events,
            session_id: todo_session,
            writer: todo_writer,
            home: todo_home,
        });
        registry.register(Arc::new(mcode_tools::builtin::TodoWriteTool::new(store)));
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

    // Re-estimate the context before every provider request; when the history
    // is large, replace its head with a durable summary (atomic checkpoint,
    // ledger untouched). Compaction failures degrade to the full history.
    let history = compact_history(
        home,
        &wire,
        model,
        &session_id,
        branch.as_str(),
        &head_stamp_text,
        history,
    )
    .await;

    let resources = mcode_config::discover_resources(home, &cwd);
    let mut system_prompt = String::from(
        "You are MCode, a coding agent. Use the provided tools to read, edit, and run code.          Answer concisely and explain what you did.",
    );
    for part in mcode_config::render_resource_prompt(&resources) {
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
    system_prompt.push_str(&mcode_agent::build_system_prompt(&registry));

    let (agent_tx, mut agent_rx) = tokio::sync::broadcast::channel(256);
    let checkpoint_home = home.clone();
    let checkpoint_cwd = cwd.clone();
    let checkpoint_session = session_id.to_owned();
    let hooks = HookRunner::default().with_before_tool(move |tool, args| {
        // Mutating file tools snapshot their target before dispatch; a
        // relative path resolves against the turn's working directory.
        if !matches!(tool, "write" | "edit") {
            return;
        }
        let Some(raw_path) = args.get("path").and_then(serde_json::Value::as_str) else {
            return;
        };
        let path = checkpoint_cwd.join(raw_path);
        let _ = mcode_config::checkpoint_file(&checkpoint_home, &checkpoint_session, &path);
    });
    let cancel = CancellationToken::new();
    let mut agent = Agent::new(AgentConfig::new().with_system_prompt(system_prompt));

    // The ledger pump owns the branch head: tool results commit as they
    // complete, the final assistant message commits at turn end.
    let pump_events = events.clone();
    let pump_session_id = session_id.to_owned();
    let writer = {
        let _ = &writer;
        writer
    };
    let pump = tokio::spawn(async move {
        let mut pending_assistant: Option<mcode_core::AssistantMessage> = None;
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
                mcode_core::events::AgentEvent::MessageDelta(
                    mcode_core::events::MessageDelta::TextDelta(delta),
                ) => {
                    let _ = pump_events.send(BridgeEvent::ChatText {
                        session_id: pump_session_id.clone(),
                        delta,
                    });
                }
                mcode_core::events::AgentEvent::MessageDelta(
                    mcode_core::events::MessageDelta::ThinkingDelta(delta),
                ) => {
                    let _ = pump_events.send(BridgeEvent::ChatThinking {
                        session_id: pump_session_id.clone(),
                        delta,
                    });
                }
                mcode_core::events::AgentEvent::MessageDelta(
                    mcode_core::events::MessageDelta::ToolCallDelta { .. },
                ) => {}
                mcode_core::events::AgentEvent::ToolStarted { call_id, name } => {
                    let _ = pump_events.send(BridgeEvent::ToolStarted {
                        session_id: pump_session_id.clone(),
                        call_id: call_id.to_string(),
                        name,
                    });
                }
                mcode_core::events::AgentEvent::ToolProgress { .. } => {}
                mcode_core::events::AgentEvent::ToolCompleted {
                    call_id,
                    result: tool_result,
                } => {
                    let Ok(payload) = serde_json::to_vec(&tool_result) else {
                        return;
                    };
                    match writer
                        .write(EventKind::ToolResult, Some(call_id.as_str()), &payload)
                        .await
                    {
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
                mcode_core::events::AgentEvent::MessageAdded(Message::Assistant(message)) => {
                    pending_assistant = Some(message);
                }
                mcode_core::events::AgentEvent::MessageAdded(_) => {}
                mcode_core::events::AgentEvent::TurnStarted => {}
                mcode_core::events::AgentEvent::TurnEnded(_) => {
                    let Some(message) = pending_assistant.take() else {
                        let _ = pump_events.send(BridgeEvent::ChatFailed {
                            session_id: pump_session_id.clone(),
                            message: "the turn ended without an assistant message".to_owned(),
                        });
                        return;
                    };
                    match serde_json::to_vec(&message) {
                        Ok(payload) => {
                            match writer.write(EventKind::Message, None, &payload).await {
                                Ok(event_id) => {
                                    let entry = project_assistant_from(&event_id, &payload);
                                    if usage_enabled && let Some(usage) = message.usage {
                                        let usage_payload = serde_json::json!({
                                            "provider": usage_provider,
                                            "model": usage_model,
                                            "input": usage.input_tokens,
                                            "output": usage.output_tokens,
                                        });
                                        if let Ok(bytes) = serde_json::to_vec(&usage_payload)
                                            && let Ok(usage_event) =
                                                writer.write(EventKind::Usage, None, &bytes).await
                                        {
                                            let _ = pump_events.send(BridgeEvent::UsageRecorded {
                                                session_id: pump_session_id.clone(),
                                                provider: usage_provider.clone(),
                                                model: usage_model.clone(),
                                                input: usage.input_tokens,
                                                output: usage.output_tokens,
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
                mcode_core::events::AgentEvent::Error(error) => {
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
    let env = mcode_agent::TurnEnv::new(&wire, &registry, &hooks)
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

/// Shared branch-head writer: the event pump and host-backed tools commit
/// through one CAS head.
#[derive(Clone)]
struct HeadWriter {
    service: SessionService,
    session: SessionId,
    branch: BranchId,
    head: Arc<tokio::sync::Mutex<HeadStamp>>,
}

impl HeadWriter {
    fn new(service: SessionService, session: SessionId, branch: BranchId, head: HeadStamp) -> Self {
        Self {
            service,
            session,
            branch,
            head: Arc::new(tokio::sync::Mutex::new(head)),
        }
    }

    /// The current committed head spelling, for UI refresh after any
    /// trailing writes.
    async fn head(&self) -> String {
        let guard = self.head.lock().await;
        head_spelling(&guard)
    }

    /// Commits one payload of `kind` and returns (event id spelling, bytes).
    async fn write(
        &self,
        kind: EventKind,
        call_id: Option<&str>,
        payload: &[u8],
    ) -> Result<String, SessionError> {
        let mut head = self.head.lock().await;
        let ledger_call = match call_id {
            Some(_) => Some(SessionCallId::generate().ok_or(SessionError::Corrupt)?),
            None => None,
        };
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
    events: mpsc::SyncSender<BridgeEvent>,
    answer: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Vec<String>>>>,
}

#[async_trait::async_trait]
impl mcode_tools::builtin::AskChannel for BridgeAskChannel {
    async fn ask(
        &self,
        questions: &[mcode_tools::builtin::AskQuestion],
        cancel: &CancellationToken,
    ) -> Result<Vec<mcode_tools::builtin::AskAnswer>, mcode_tools::ToolError> {
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
            .ok_or_else(mcode_tools::builtin::user_dismissed)?;
        let answers = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                return Err(mcode_tools::ToolError::Execution("cancelled".into()));
            }
            answers = rx => answers.map_err(|_| mcode_tools::builtin::user_dismissed())?,
        };
        Ok(questions
            .iter()
            .enumerate()
            .map(|(index, question)| mcode_tools::builtin::AskAnswer {
                question: question.question.clone(),
                answer: answers.get(index).cloned().unwrap_or_default(),
            })
            .collect())
    }
}

/// Persists `todo_write` payloads and mirrors them to the UI.
struct BridgeTodoStore {
    events: mpsc::SyncSender<BridgeEvent>,
    session_id: String,
    writer: HeadWriter,
    home: HomeLayout,
}

#[async_trait::async_trait]
impl mcode_tools::builtin::TodoStore for BridgeTodoStore {
    async fn store(
        &self,
        tasks: &[mcode_tools::builtin::TodoWireTask],
    ) -> Result<String, mcode_tools::ToolError> {
        // Resolve or mint stable ids, then validate the graph.
        let mut document = mcode_config::TodoDocument::default();
        for task in tasks {
            let id = match &task.id {
                Some(id) => id.clone(),
                None => mcode_config::new_todo_id()
                    .ok_or_else(|| mcode_tools::ToolError::Execution("id minting failed".into()))?,
            };
            let status = match task.status.as_str() {
                "pending" => mcode_config::TodoStatus::Pending,
                "in_progress" => mcode_config::TodoStatus::InProgress,
                _ => mcode_config::TodoStatus::Completed,
            };
            document.tasks.push(mcode_config::TodoTask {
                id,
                content: task.content.clone(),
                status,
                blocked_by: task.blocked_by.clone(),
            });
        }
        document
            .validate()
            .map_err(|error| mcode_tools::ToolError::Execution(error.to_string()))?;

        // CAS revision: read the current header.
        let revision = mcode_config::read_todo_revision(&self.home, &self.session_id)
            .map_err(|error| mcode_tools::ToolError::Execution(error.to_string()))?;
        mcode_config::replace_todo_document(&self.home, &self.session_id, revision, &document)
            .map_err(|error| mcode_tools::ToolError::Execution(error.to_string()))?;

        // Durable Task event on the branch.
        let payload = document
            .to_payload()
            .map_err(|error| mcode_tools::ToolError::Execution(error.to_string()))?;
        if let Err(error) = self.writer.write(EventKind::Task, None, &payload).await {
            return Err(mcode_tools::ToolError::Execution(render_error(error)));
        }

        let in_progress = document
            .tasks
            .iter()
            .filter(|task| matches!(task.status, mcode_config::TodoStatus::InProgress))
            .count();
        let completed = document
            .tasks
            .iter()
            .filter(|task| matches!(task.status, mcode_config::TodoStatus::Completed))
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
                            mcode_config::TodoStatus::Pending => "pending".to_owned(),
                            mcode_config::TodoStatus::InProgress => "in progress".to_owned(),
                            mcode_config::TodoStatus::Completed => "done".to_owned(),
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
fn project_replayed_entry(
    event: &mcode_session::session::SessionEvent,
    payload: &[u8],
) -> ConversationEntry {
    match event.kind {
        EventKind::Message => {
            // Assistant messages are typed JSON; a parse miss means the
            // payload is the user's plain-text message.
            if serde_json::from_slice::<mcode_core::AssistantMessage>(payload).is_ok() {
                project_assistant_from(event.event_id.as_str(), payload)
            } else {
                ConversationEntry {
                    event_id: event.event_id.as_str().to_owned(),
                    kind: EntryKind::UserMessage,
                    text: decode_text(payload),
                    call_id: None,
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
                text: name.to_owned(),
                call_id: event.call_id.as_ref().map(|call| call.as_str().to_owned()),
            }
        }
        EventKind::Usage => project_usage(event.event_id.as_str(), payload),
        EventKind::Task => ConversationEntry {
            event_id: event.event_id.as_str().to_owned(),
            kind: EntryKind::Usage,
            text: String::new(),
            call_id: None,
        },
    }
}

fn project_usage(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let value: serde_json::Value = serde_json::from_slice(payload).unwrap_or_default();
    let model = value["model"].as_str().unwrap_or("unknown");
    let input = value["input"].as_u64().unwrap_or_default();
    let output = value["output"].as_u64().unwrap_or_default();
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::Usage,
        text: format!("{model}: {input} in / {output} out"),
        call_id: None,
    }
}

/// Projects a committed tool-result payload into a display entry.
fn project_tool_result(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let result: mcode_core::ToolResultMessage =
        serde_json::from_slice(payload).unwrap_or_else(|_| mcode_core::ToolResultMessage {
            tool_call_id: String::new(),
            content: Vec::new(),
            is_error: true,
            details: None,
        });
    let text: String = result
        .content
        .iter()
        .filter_map(|block| match block {
            mcode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    ConversationEntry {
        event_id: event_id.to_owned(),
        kind: EntryKind::ToolResult,
        text: if result.is_error {
            format!("failed: {text}")
        } else {
            text
        },
        call_id: Some(result.tool_call_id),
    }
}

/// Projects a committed assistant payload into a display entry.
fn project_assistant_from(event_id: &str, payload: &[u8]) -> ConversationEntry {
    let mut text = String::new();
    if let Ok(message) = serde_json::from_slice::<mcode_core::AssistantMessage>(payload) {
        for block in &message.blocks {
            match block {
                mcode_core::ContentBlock::Text(block) => text.push_str(&block.text),
                mcode_core::ContentBlock::ToolCall(call) => {
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
        text,
        call_id: None,
    }
}

// ---- compaction (T21) ----

/// Estimated tokens that trigger compaction before a provider request.
const COMPACTION_THRESHOLD_TOKENS: usize = 48_000;
/// Verbatim messages kept after the summary.
const COMPACTION_TAIL_MESSAGES: usize = 8;
/// Wall budget for the summarization child completion.
const COMPACTION_SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
/// Largest transcript handed to the summarizer (characters).
const COMPACTION_TRANSCRIPT_CAP_CHARS: usize = 300_000;
/// Largest one-message excerpt inside the transcript.
const COMPACTION_EXCERPT_CHARS: usize = 4_000;

/// Rough token estimate of one message's text content.
fn message_tokens(message: &Message) -> usize {
    mcode_config::estimate_tokens(&message_text(message))
}

/// Flattens one message to plain text for estimation and transcripts.
fn message_text(message: &Message) -> String {
    let blocks: Vec<&str> = match message {
        Message::User(user) => user
            .content
            .iter()
            .filter_map(|block| match block {
                mcode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect(),
        Message::Assistant(assistant) => assistant
            .blocks
            .iter()
            .filter_map(|block| match block {
                mcode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    blocks.join("")
}

/// Splits history into (summarize, keep) when its estimate exceeds the
/// threshold; `None` means no compaction is due.
fn compaction_split(history: &[Message]) -> Option<(&[Message], &[Message])> {
    let estimate: usize = history.iter().map(message_tokens).sum();
    if estimate <= COMPACTION_THRESHOLD_TOKENS || history.len() <= COMPACTION_TAIL_MESSAGES {
        return None;
    }
    let split = history.len() - COMPACTION_TAIL_MESSAGES;
    Some((&history[..split], &history[split..]))
}

/// Renders the head messages (plus any prior summary) into a transcript for
/// the summarizer, tail-capped to keep the child request bounded.
fn compaction_transcript(prior_summary: Option<&str>, head: &[Message]) -> String {
    let mut transcript = String::new();
    if let Some(summary) = prior_summary {
        transcript.push_str("Summary of the earlier conversation:\n");
        transcript.push_str(summary);
        transcript.push_str("\n\n");
    }
    for message in head {
        let role = match message {
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            _ => "system",
        };
        let text = message_text(message);
        let cut = text
            .char_indices()
            .nth(COMPACTION_EXCERPT_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(text.len());
        transcript.push_str(&format!("[{role}] {}\n\n", &text[..cut]));
    }
    let count = transcript.chars().count();
    if count > COMPACTION_TRANSCRIPT_CAP_CHARS {
        let skip = transcript
            .char_indices()
            .nth(count - COMPACTION_TRANSCRIPT_CAP_CHARS)
            .map(|(index, _)| index)
            .unwrap_or(0);
        format!("...earlier content elided...\n{}", &transcript[skip..])
    } else {
        transcript
    }
}

/// Runs the summarization child completion on the turn's provider.
async fn summarize_transcript(wire: &WireProvider, transcript: &str) -> Result<String, String> {
    let request = Request::new()
        .with_system_prompt(
            "You maintain a running summary of a coding-agent conversation. \
Preserve the user's goals, decisions made, files and paths touched, commands \
run, open tasks, and unresolved errors. Be dense and factual; no preamble.",
        )
        .with_message(Message::User(mcode_core::UserMessage::text(format!(
            "Summarize the following conversation for continuation:\n\n{transcript}"
        ))));
    let cancel = CancellationToken::new();
    let mut stream = wire
        .stream(&request, cancel)
        .await
        .map_err(|error| format!("summary request failed: {error:?}"))?;
    let mut summary = String::new();
    loop {
        let Some(event) = stream.next().await else {
            return Err("summary stream ended without completion".to_owned());
        };
        match event {
            StreamEvent::TextDelta(delta) => summary.push_str(&delta),
            StreamEvent::Done { .. } => break,
            StreamEvent::Error(error) => return Err(format!("summary stream failed: {error:?}")),
            _ => {}
        }
    }
    let chars: Vec<char> = summary.chars().collect();
    if chars.len() > mcode_config::MAX_SUMMARY_CHARS {
        summary = chars[..mcode_config::MAX_SUMMARY_CHARS].iter().collect();
    }
    if summary.trim().is_empty() {
        return Err("summary was empty".to_owned());
    }
    Ok(summary)
}

/// Compacts history before a provider request. The ledger is untouched: the
/// durable checkpoint only narrows what this and later turns send. Any
/// failure degrades to the full history.
async fn compact_history(
    home: &mcode_config::HomeLayout,
    wire: &WireProvider,
    model: &str,
    session_id: &str,
    branch_id: &str,
    head: &str,
    history: Vec<Message>,
) -> Vec<Message> {
    let Some((head_messages, tail)) = compaction_split(&history) else {
        return history;
    };
    // A prior checkpoint for the same branch folds its summary into the
    // transcript so the child consolidates instead of re-reading everything.
    let prior = mcode_config::read_compaction(home, session_id)
        .ok()
        .flatten()
        .filter(|checkpoint| checkpoint.branch_id == branch_id);
    let prior_summary = prior.as_ref().map(|checkpoint| checkpoint.summary.as_str());
    let transcript = compaction_transcript(prior_summary, head_messages);
    let summarized = tokio::time::timeout(
        COMPACTION_SUMMARY_TIMEOUT,
        summarize_transcript(wire, &transcript),
    )
    .await;
    let summary = match summarized {
        Ok(Ok(summary)) => summary,
        Ok(Err(message)) => {
            eprintln!("[mcode-compaction] skipped: {message}");
            return history;
        }
        Err(_) => {
            eprintln!("[mcode-compaction] skipped: summary timed out");
            return history;
        }
    };
    let checkpoint = mcode_config::CompactionCheckpoint {
        format_version: mcode_config::COMPACTION_FORMAT_VERSION,
        kind: mcode_config::COMPACTION_KIND.to_owned(),
        session_id: session_id.to_owned(),
        branch_id: branch_id.to_owned(),
        covered_head: head.to_owned(),
        covered_messages: head_messages.len(),
        summary: summary.clone(),
        model: model.to_owned(),
        created_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
    };
    if let Err(error) = mcode_config::write_compaction(home, session_id, &checkpoint) {
        eprintln!("[mcode-compaction] checkpoint write failed: {error:?}");
        return history;
    }
    let mut compacted = Vec::with_capacity(tail.len() + 1);
    compacted.push(Message::User(mcode_core::UserMessage::text(format!(
        "Summary of the conversation so far (earlier events compacted):\n\n{summary}"
    ))));
    compacted.extend(tail.iter().cloned());
    compacted
}

// ---- subagents (T20) ----

/// Concurrent subagent slots across the process.
static TASK_SLOTS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
/// Maximum subagents running at once.
const MAX_CONCURRENT_SUBAGENTS: usize = 4;
/// Wall budget for one subagent.
const SUBAGENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
/// Subagent system brief: the delegation is one-shot, no user interaction.
const SUBAGENT_SYSTEM_PROMPT: &str = "You are an MCode subagent. Complete the delegated task \\
with the provided tools, then finish with your final answer as the last message. \\
You cannot ask the user questions; make reasonable assumptions and report them.";

/// Host for the `task` tool: runs one nested agent on the turn's provider
/// with the built-in tools only (no ask_user, no task — depth stays at one).
struct BridgeTaskHost {
    resolved: ResolvedProvider,
    home: mcode_config::HomeLayout,
    cwd: PathBuf,
}

/// One git worktree lease: a disposable checkout plus its manifest, so a
/// crashed process can recover leases on the next start.
struct WorktreeLease {
    path: PathBuf,
    manifest: PathBuf,
}

impl WorktreeLease {
    /// Creates a detached worktree of the current repository HEAD.
    fn acquire(home: &mcode_config::HomeLayout, repo: &Path) -> Result<Self, String> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let id = format!("task-{}-{stamp}", std::process::id());
        let leases = home.root().join("task-worktrees");
        std::fs::create_dir_all(&leases).map_err(|error| format!("lease dir: {error}"))?;
        let path = leases.join(&id);
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .output()
            .map_err(|error| format!("git worktree: {error}"))?;
        if !output.status.success() {
            let _ = std::fs::remove_dir(&path);
            let reason = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git worktree add failed: {}", reason.trim()));
        }
        let manifest = leases.join(format!("{id}.json"));
        let record = serde_json::json!({
            "repo": repo.to_string_lossy(),
            "path": path.to_string_lossy(),
        });
        std::fs::write(&manifest, record.to_string())
            .map_err(|error| format!("lease manifest: {error}"))?;
        Ok(Self { path, manifest })
    }

    /// Releases the lease; best-effort because the work may be done.
    fn release(self) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
        // Retry by repo path when the lease checkout itself is broken.
        if matches!(&output, Ok(result) if !result.status.success())
            && let Ok(record) = std::fs::read(&self.manifest)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&record)
            && let Some(repo) = value["repo"].as_str()
        {
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["worktree", "remove", "--force"])
                .arg(&self.path)
                .output();
        }
        let _ = std::fs::remove_file(&self.manifest);
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Removes leases left behind by a crashed process. Best-effort: a lease
/// whose repo is gone is simply deleted from disk.
fn recover_task_worktrees(home: &mcode_config::HomeLayout) {
    let leases = home.root().join("task-worktrees");
    let Ok(entries) = std::fs::read_dir(&leases) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let (Some(repo), Some(lease)) = (value["repo"].as_str(), value["path"].as_str())
            {
                let _ = std::process::Command::new("git")
                    .arg("-C")
                    .arg(repo)
                    .args(["worktree", "remove", "--force"])
                    .arg(lease)
                    .output();
            }
            let _ = std::fs::remove_file(&path);
        } else {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

#[async_trait::async_trait]
impl mcode_tools::builtin::TaskHost for BridgeTaskHost {
    async fn run_subagent(
        &self,
        request: mcode_tools::builtin::SubagentRequest,
        progress: &mcode_tools::ToolStream,
        cancel: &CancellationToken,
    ) -> Result<String, mcode_tools::ToolError> {
        let fail = |message: String| mcode_tools::ToolError::Execution(message);
        let _ = progress.progress(format!("task queued: {}", request.description));
        // Bounded queue: wait for a slot or cancellation.
        let semaphore =
            TASK_SLOTS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_SUBAGENTS));
        let permit = tokio::select! {
            permit = semaphore.acquire() => permit.map_err(|_| fail("task slots closed".to_owned()))?,
            _ = cancel.cancelled() => return Err(fail("task cancelled".to_owned())),
        };
        let lease = if request.worktree {
            match WorktreeLease::acquire(&self.home, &self.cwd) {
                Ok(lease) => Some(lease),
                Err(message) => return Err(fail(message)),
            }
        } else {
            None
        };
        let result = self
            .drive_subagent(&request, progress, cancel, lease.as_ref())
            .await;
        if let Some(lease) = lease {
            lease.release();
        }
        drop(permit);
        result
    }
}

impl BridgeTaskHost {
    /// Runs the nested agent to completion under the wall budget.
    async fn drive_subagent(
        &self,
        request: &mcode_tools::builtin::SubagentRequest,
        progress: &mcode_tools::ToolStream,
        cancel: &CancellationToken,
        lease: Option<&WorktreeLease>,
    ) -> Result<String, mcode_tools::ToolError> {
        let fail = |message: String| mcode_tools::ToolError::Execution(message);
        let run_dir = lease
            .map(|lease| lease.path.clone())
            .unwrap_or_else(|| self.cwd.clone());
        let transport =
            ReqwestTransport::new().map_err(|_| fail("HTTP transport unavailable".to_owned()))?;
        let wire = WireProvider::new(self.resolved.clone(), Arc::new(transport));
        let registry = Arc::new({
            let registry = ToolRegistry::new();
            mcode_tools::register_builtins(&registry);
            let web_host: Arc<dyn mcode_tools::builtin::WebHost> = Arc::new(BridgeWebHost {
                home: self.home.clone(),
            });
            registry.register(Arc::new(mcode_tools::builtin::WebSearchTool::new(
                web_host.clone(),
            )));
            registry.register(Arc::new(mcode_tools::builtin::FetchContentTool::new(
                web_host,
            )));
            registry
        });
        let run_dir_for_hooks = run_dir.clone();
        let run_home = self.home.clone();
        let hooks = HookRunner::default().with_before_tool(move |tool, args| {
            if !matches!(tool, "write" | "edit") {
                return;
            }
            let Some(raw_path) = args.get("path").and_then(serde_json::Value::as_str) else {
                return;
            };
            // Subagent writes snapshot into a side checkpoint store so the
            // parent session's rollback surface stays unchanged.
            let session = format!("task-{}", std::process::id());
            let path = std::path::PathBuf::from(raw_path);
            let absolute = if path.is_absolute() {
                path
            } else {
                run_dir_for_hooks.join(path)
            };
            let _ = mcode_config::checkpoint_file(&run_home, &session, &absolute);
        });

        let child_cancel = CancellationToken::new();
        // Parent cancellation or timeout aborts the child.
        let link = {
            let child_cancel = child_cancel.clone();
            let parent = cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = parent.cancelled() => child_cancel.cancel(),
                    _ = child_cancel.cancelled() => {}
                }
            })
        };
        let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(64);
        let description = request.description.clone();
        let progress_sink = progress.clone();
        let forwarder = tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                if let mcode_core::events::AgentEvent::ToolStarted { name, .. } = event {
                    let _ = progress_sink.progress(format!("{description}: {name}"));
                }
            }
        });

        let env = mcode_agent::TurnEnv::new(&wire, &registry, &hooks)
            .with_cancel(child_cancel.clone())
            .with_events(event_tx)
            .with_cwd(run_dir);
        let mut agent = Agent::new(AgentConfig::new().with_system_prompt(SUBAGENT_SYSTEM_PROMPT));
        let prompt = Message::User(mcode_core::UserMessage::text(request.prompt.clone()));
        let outcome = tokio::time::timeout(SUBAGENT_TIMEOUT, agent.prompt(prompt, &env)).await;
        link.abort();
        forwarder.abort();
        match outcome {
            Ok(Ok(_)) => (),
            Ok(Err(error)) => return Err(fail(format!("subagent failed: {error}"))),
            Err(_) => {
                child_cancel.cancel();
                return Err(fail("subagent timed out".to_owned()));
            }
        };
        let answer = agent
            .state()
            .messages()
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(assistant) => {
                    let text: String = assistant
                        .blocks
                        .iter()
                        .filter_map(|block| match block {
                            mcode_core::ContentBlock::Text(text) => Some(text.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    (!text.trim().is_empty()).then_some(text)
                }
                _ => None,
            })
            .unwrap_or_default();
        if answer.trim().is_empty() {
            return Err(fail("subagent returned no answer".to_owned()));
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcode_session::session::EventKind;

    /// End-to-end over the real provider configured in the user's home:
    /// settings parse, vault key, resolve, stream, decode. Run explicitly
    /// with `cargo test -p mcode-desktop -- --ignored live_provider`.
    #[tokio::test]
    #[ignore = "calls the live provider configured under ~/.mcode"]
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
            .with_message(Message::User(mcode_core::UserMessage::text("Say pong.")));
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

    #[test]
    fn worktree_lease_acquires_and_releases() {
        let (_parent, layout) = home();
        let repo = std::env::current_dir().expect("cwd");
        // Only meaningful inside a git checkout; skip elsewhere.
        if !repo.join(".git").exists() {
            return;
        }
        let lease = WorktreeLease::acquire(&layout, &repo).expect("lease");
        assert!(lease.path.is_dir());
        let manifest = lease.manifest.clone();
        let checkout = lease.path.clone();
        assert!(manifest.exists());
        lease.release();
        assert!(!manifest.exists());
        assert!(!checkout.exists());
    }

    fn user_msg(text: &str) -> Message {
        Message::User(mcode_core::UserMessage::text(text))
    }

    fn assistant_msg(text: &str) -> Message {
        Message::Assistant(mcode_core::AssistantMessage {
            blocks: vec![mcode_core::ContentBlock::Text(mcode_core::TextBlock::new(
                text,
            ))],
            usage: None,
            stop_reason: mcode_core::StopReason::Stop,
        })
    }

    #[test]
    fn compaction_split_skips_small_history() {
        let history: Vec<Message> = (0..20)
            .map(|index| user_msg(&format!("message {index}")))
            .collect();
        assert!(compaction_split(&history).is_none());
    }

    #[test]
    fn compaction_split_keeps_tail_verbatim() {
        let history: Vec<Message> = (0..40)
            .map(|index| {
                let text = format!("message {index}: {}", "x".repeat(6_000));
                if index % 2 == 0 {
                    user_msg(&text)
                } else {
                    assistant_msg(&text)
                }
            })
            .collect();
        let (head, tail) = compaction_split(&history).expect("compaction due");
        assert_eq!(head.len(), 40 - COMPACTION_TAIL_MESSAGES);
        assert_eq!(tail.len(), COMPACTION_TAIL_MESSAGES);
    }

    #[test]
    fn compaction_split_never_drops_everything() {
        let history: Vec<Message> = (0..4)
            .map(|index| user_msg(&format!("huge {index}: {}", "x".repeat(100_000))))
            .collect();
        assert!(compaction_split(&history).is_none());
    }

    #[test]
    fn compaction_transcript_includes_prior_summary_and_roles() {
        let head = vec![user_msg("hello"), assistant_msg("hi there")];
        let transcript = compaction_transcript(Some("prior digest"), &head);
        assert!(transcript.contains("prior digest"));
        assert!(transcript.contains("[user] hello"));
        assert!(transcript.contains("[assistant] hi there"));
    }

    #[test]
    fn compaction_transcript_caps_total_chars() {
        let head: Vec<Message> = (0..200)
            .map(|index| user_msg(&format!("{index}: {}", "y".repeat(4_000))))
            .collect();
        let transcript = compaction_transcript(None, &head);
        assert!(transcript.starts_with("...earlier content elided..."));
        assert!(transcript.chars().count() <= COMPACTION_TRANSCRIPT_CAP_CHARS + 64);
    }

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
        let (document, revision, key_ids, _mcp_ids) = reloaded.expect("settings reload");
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
