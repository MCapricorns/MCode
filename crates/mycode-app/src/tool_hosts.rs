//! Host-backed tools bridged into the agent's registry: `ask_user`, todo
//! persistence, and the settings-configured web search backend.

use std::collections::HashMap;
use std::sync::mpsc;

use mycode_agent::session::EventKind;
use mycode_config::{AppSettings, HomeLayout, read_app_settings, read_provider_secrets};
use tokio_util::sync::CancellationToken;

use crate::BridgeEvent;
use crate::ledger::{HeadWriter, render_error};
use crate::settings_io::render_config_error;

/// Environment variable carrying the Querit API key, checked before the vault
/// (the pi agent's querit plugin resolves this variable before its own config
/// file, so a machine already set up for pi keeps working).
const QUERIT_KEY_ENV: &str = "QUERIT_API_KEY";
/// Environment variable carrying the AnySearch API key. Anonymous traffic is
/// allowed when this and the vault entry are both absent.
const ANYSEARCH_KEY_ENV: &str = "ANYSEARCH_API_KEY";

/// Routes user answers from the UI to the pending `ask_user` tool.
#[derive(Default)]
struct AskRouter {
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

/// Process-wide ask router; one pending ask per session.
static ASK_ROUTER: std::sync::OnceLock<AskRouter> = std::sync::OnceLock::new();

/// Registers the turn's ask channel, or waits for an already-registered one.
pub(crate) fn register_ask(session_id: &str) -> tokio::sync::oneshot::Receiver<Vec<String>> {
    ASK_ROUTER
        .get_or_init(AskRouter::default)
        .register(session_id)
}

pub(crate) fn deliver_ask_answer(session_id: &str, answers: Vec<String>) -> Result<(), String> {
    let router = ASK_ROUTER.get_or_init(AskRouter::default);
    if router.deliver(session_id, answers) {
        Ok(())
    } else {
        Err("no pending question for this session".to_owned())
    }
}

/// Host channel forwarding `ask_user` waits through the UI event stream.
pub(crate) struct BridgeAskChannel {
    pub(crate) session_id: String,
    pub(crate) events: mpsc::Sender<BridgeEvent>,
    pub(crate) answer: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Vec<String>>>>,
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
pub(crate) struct BridgeTodoStore {
    pub(crate) events: mpsc::Sender<BridgeEvent>,
    pub(crate) session_id: String,
    pub(crate) writer: HeadWriter,
    pub(crate) home: HomeLayout,
}

#[async_trait::async_trait]
impl mycode_tools::builtin::TodoStore for BridgeTodoStore {
    async fn store(
        &self,
        tasks: &[mycode_tools::builtin::TodoWireTask],
    ) -> Result<String, mycode_tools::ToolError> {
        // Models often send short ids (`"1"`) or omit them. Mint canonical
        // ids and remap dependencies so a usable plan is not rejected as an
        // invalid authority document.
        let document = normalize_todo_document(tasks)
            .map_err(mycode_tools::ToolError::Execution)?;
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

fn normalize_todo_document(
    tasks: &[mycode_tools::builtin::TodoWireTask],
) -> Result<mycode_config::TodoDocument, String> {
    let mut id_map = HashMap::new();
    let mut assigned = Vec::with_capacity(tasks.len());
    for task in tasks {
        let raw = task.id.as_deref().unwrap_or("").trim();
        let id =
            if mycode_config::is_todo_id(raw) && !id_map.values().any(|existing| existing == raw) {
                raw.to_owned()
            } else {
                mycode_config::new_todo_id().ok_or_else(|| "id minting failed".to_owned())?
            };
        if !raw.is_empty() {
            id_map.insert(raw.to_owned(), id.clone());
        }
        assigned.push((id, task));
    }
    let mut in_progress_kept = false;
    let mut document = mycode_config::TodoDocument::default();
    for (id, task) in assigned {
        let mut status = match task.status.as_str() {
            "pending" => mycode_config::TodoStatus::Pending,
            "in_progress" | "in progress" => mycode_config::TodoStatus::InProgress,
            "completed" | "done" => mycode_config::TodoStatus::Completed,
            other => {
                return Err(format!("unknown status: {other}"));
            }
        };
        if matches!(status, mycode_config::TodoStatus::InProgress) {
            if in_progress_kept {
                status = mycode_config::TodoStatus::Pending;
            } else {
                in_progress_kept = true;
            }
        }
        let blocked_by = task
            .blocked_by
            .iter()
            .filter_map(|dep| id_map.get(dep.trim()).cloned())
            .filter(|dep| dep != &id)
            .collect();
        document.tasks.push(mycode_config::TodoTask {
            id,
            content: task.content.trim().to_owned(),
            status,
            blocked_by,
        });
    }
    document.validate().map_err(|error| error.to_string())?;
    Ok(document)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        let registry = mycode_tools::ToolRegistry::new();
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
}
