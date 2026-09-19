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
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, mpsc};
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
use mcode_providers::{ReqwestTransport, ResolvedProvider, WireProvider};
use mcode_session::session::{
    self, BranchId, EventKind, HeadStamp, SessionCallId, SessionError, SessionId, SessionService,
};
use mcode_tools::ToolRegistry;
use mcode_updates::{PreparedUpdate, UpdateOffer};
use mcode_web::SearchResult;
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
        // The state lives behind one Arc: `SessionService` retires its
        // publication when a clone is dropped, so clones must share one
        // instance instead of duplicating it.
        let state = Arc::new(CoreState::new(home));
        spawn_catalog_refresh(state.clone(), events.clone());
        spawn_update_check(state.clone(), events.clone());
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
        Self {
            service: SessionService::new(&home),
            catalog: Arc::new(RwLock::new(CatalogInfo {
                document: Arc::new(cached.document),
                fetched_at: cached.fetched_at,
            })),
            home,
            projects: Arc::new(Mutex::new(HashMap::new())),
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
        BridgeCommand::ListSessions => {
            BridgeReply::Sessions(inspect_summaries(&state.home).map_err(render_error))
        }
        BridgeCommand::CreateSession => match state.service.create().await {
            Ok(created) => BridgeReply::Created(Ok(SessionSummary {
                session_id: created.session_id.as_str().to_owned(),
                root_branch_id: created.branch_id.as_str().to_owned(),
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
            BridgeReply::WebSearched(web_search(&state.home, query))
        }
        BridgeCommand::McpListTools { server_id } => {
            BridgeReply::McpTools(mcp_list_tools(&state.home, server_id))
        }
        BridgeCommand::RollbackWorkspace { session_id } => BridgeReply::RolledBack(
            mcode_config::rollback_session(&state.home, session_id)
                .map_err(|error| render_config_error(&error)),
        ),
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
fn mcp_list_tools(home: &HomeLayout, server_id: &str) -> Result<(String, Vec<String>), String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let server = settings
        .mcp_servers
        .iter()
        .find(|server| server.id == server_id && server.enabled)
        .ok_or_else(|| "MCP server not found or disabled in settings".to_owned())?;
    let secrets = read_provider_secrets(home).map_err(|error| render_config_error(&error))?;
    let api_key = secrets.key(&format!("mcp-{server_id}")).map(str::to_owned);
    let timeout = mcode_mcp::DEFAULT_REQUEST_TIMEOUT;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "mcp runtime unavailable".to_owned())?;
    runtime.block_on(async {
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
    })
}

/// Runs one bounded search over the enabled backend, if any.
fn web_search(home: &HomeLayout, query: &str) -> Result<Vec<SearchResult>, String> {
    let settings = read_app_settings(home).map_err(|error| render_config_error(&error))?;
    let backend = settings
        .web
        .backends
        .iter()
        .find(|backend| backend.enabled)
        .ok_or_else(|| "no enabled search backend — add one in Settings".to_owned())?;
    let transport = mcode_web::reqwest_transport::ReqwestWebTransport::new()
        .map_err(|_| "web transport unavailable".to_owned())?;
    let client = mcode_web::WebClient::new(&backend.endpoint, std::sync::Arc::new(transport))
        .map_err(|_| "the search backend endpoint violates the URL policy".to_owned())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "web runtime unavailable".to_owned())?;
    runtime.block_on(async {
        client
            .search(query, 8, tokio_util::sync::CancellationToken::new())
            .await
            .map_err(|error| format!("search failed: {error}"))
    })
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
    let wire = WireProvider::new(resolved, Arc::new(transport));

    // The tool working directory is the bound project (created on demand).
    std::fs::create_dir_all(&cwd).map_err(|error| format!("workspace dir: {error}"))?;
    let usage_enabled = settings.usage.enabled;
    let usage_provider = provider_id.to_owned();
    let usage_model = model.to_owned();
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
                                    let _ = pump_events.send(BridgeEvent::ChatDone {
                                        session_id: pump_session_id.clone(),
                                        head: event_id,
                                        entry,
                                    });
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
