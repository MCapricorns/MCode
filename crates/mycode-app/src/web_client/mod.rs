//! First-party bounded web search over a configured backend family.
//!
//! Querit and custom backends share `POST {endpoint}/v1/search` plus
//! `POST {endpoint}/v1/contents`. AnySearch uses `POST {endpoint}/v1/search`
//! with `{query, max_results}` answering `{data.results}` and
//! `POST {endpoint}/v1/extract` with `{url}` for page text. Everything is
//! bounded: result count, URL count, response bytes, and aggregate payload
//! bytes. The transport is injectable so tests run without any network; the
//! production [`transport::ReqwestWebTransport`] ships with the crate.

pub mod guard;
pub mod transport;

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// One ranked search hit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SearchResult {
    /// Page URL (https only).
    pub url: String,
    /// Page title.
    pub title: String,
    /// Short snippet.
    pub snippet: String,
}

/// One fetched page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageContent {
    /// Page URL.
    pub url: String,
    /// Extracted plain text.
    pub content: String,
    /// True when the extraction was cut off at the per-page cap.
    pub truncated: bool,
}

/// Errors surfaced by the web client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WebError {
    /// The request or a response URL violated the guard rules.
    #[error("blocked by URL policy")]
    Blocked,
    /// The backend response violated the bounded contract.
    #[error("backend response is malformed or oversized")]
    Protocol,
    /// The backend could not be reached.
    #[error("search backend is unavailable")]
    Unavailable,
    /// The caller cancelled the request.
    #[error("cancelled")]
    Cancelled,
}

/// Outbound POST seam for the web client.
#[async_trait::async_trait]
pub trait WebTransport: Send + Sync + 'static {
    /// Posts a JSON body with an optional bearer key and returns the raw
    /// response bytes.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for transport-level failures.
    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: &[u8],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, WebError>;
}

/// Backend family that selects the request and response wire mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchKind {
    /// Querit `POST /v1/search` + `POST /v1/contents`.
    Querit,
    /// AnySearch `POST /v1/search` + `POST /v1/extract`.
    Anysearch,
    /// Querit-compatible custom endpoint.
    #[default]
    Custom,
}

impl SearchKind {
    /// Parses a settings `kind` spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "querit" => Some(Self::Querit),
            "anysearch" => Some(Self::Anysearch),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Client bound to one backend endpoint.
#[derive(Clone)]
pub struct WebClient {
    endpoint: String,
    bearer: Option<String>,
    transport: Arc<dyn WebTransport>,
    timeout: Duration,
    kind: SearchKind,
}

impl WebClient {
    /// Creates one client over an https backend endpoint from settings.
    /// `bearer` rides every request as the Authorization header.
    ///
    /// # Errors
    ///
    /// Returns [`WebError::Blocked`] when the endpoint itself violates the
    /// URL policy.
    pub fn new(
        endpoint: &str,
        bearer: Option<String>,
        transport: Arc<dyn WebTransport>,
    ) -> Result<Self, WebError> {
        if !guard::is_fetchable_url(endpoint) {
            return Err(WebError::Blocked);
        }
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            bearer: bearer.filter(|key| !key.trim().is_empty()),
            transport,
            timeout: Duration::from_secs(guard::DEFAULT_TIMEOUT_SECS),
            kind: SearchKind::Custom,
        })
    }

    /// Selects the wire mapping for this backend family.
    #[must_use]
    pub fn with_kind(mut self, kind: SearchKind) -> Self {
        self.kind = kind;
        self
    }

    /// Runs one bounded search.
    ///
    /// Wire contract (Querit): `POST /v1/search` with `{"query", "count"}`
    /// answering `{"results": {"result": [...]}}`; the flat
    /// `{"results": [...]}` shape is accepted for custom backends.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for guard, transport, or contract violations.
    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>, WebError> {
        if query.trim().is_empty() {
            return Err(WebError::Blocked);
        }
        let max_results = max_results.clamp(1, guard::MAX_SEARCH_RESULTS);
        match self.kind {
            SearchKind::Anysearch => self.search_anysearch(query, max_results, cancel).await,
            SearchKind::Querit | SearchKind::Custom => {
                self.search_querit(query, max_results, cancel).await
            }
        }
    }

    async fn search_querit(
        &self,
        query: &str,
        max_results: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "query": query,
            "count": max_results,
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/v1/search", &body, cancel).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let hits = payload["results"]["result"].as_array().or_else(|| {
            // Custom backends answer with a flat array under `results`.
            payload["results"].as_array()
        });
        let hits = hits.ok_or(WebError::Protocol)?;
        self.collect_hits(hits, max_results, |hit| {
            (
                hit["url"].as_str(),
                hit["title"].as_str(),
                hit["snippet"].as_str(),
            )
        })
    }

    async fn search_anysearch(
        &self,
        query: &str,
        max_results: usize,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchResult>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "query": query,
            "max_results": max_results,
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/v1/search", &body, cancel).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        if payload["code"].as_i64().is_some_and(|code| code != 0) {
            return Err(WebError::Unavailable);
        }
        let hits = payload["data"]["results"]
            .as_array()
            .ok_or(WebError::Protocol)?;
        self.collect_hits(hits, max_results, |hit| {
            (
                hit["url"].as_str(),
                hit["title"].as_str(),
                hit["snippet"].as_str().or_else(|| hit["content"].as_str()),
            )
        })
    }

    fn collect_hits(
        &self,
        hits: &[serde_json::Value],
        max_results: usize,
        fields: impl Fn(&serde_json::Value) -> (Option<&str>, Option<&str>, Option<&str>),
    ) -> Result<Vec<SearchResult>, WebError> {
        let results: Vec<SearchResult> = hits
            .iter()
            .filter_map(|hit| {
                let (url, title, snippet) = fields(hit);
                let url = url?;
                Some(SearchResult {
                    url: url.to_owned(),
                    title: title.unwrap_or(url).to_owned(),
                    snippet: snippet.unwrap_or_default().to_owned(),
                })
            })
            .take(max_results)
            .filter(|result| guard::is_fetchable_url(&result.url))
            .map(|mut result| {
                result.title = guard::sanitize_remote_text(&result.title);
                result.snippet = guard::sanitize_remote_text(&result.snippet);
                result
            })
            .collect();
        if results.len() > guard::MAX_SEARCH_RESULTS {
            return Err(WebError::Protocol);
        }
        Ok(results)
    }

    /// Fetches bounded page text for the given URLs.
    ///
    /// Wire contract (Querit): `POST /v1/contents` with
    /// `{"urls", "format": "text", "crawlTimeout", "extrasMeta"}` answering
    /// `{"results": [{url, content}]}`; the `pages` shape is accepted for
    /// custom backends.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for guard, transport, or contract violations.
    pub async fn contents(
        &self,
        urls: &[String],
        cancel: CancellationToken,
    ) -> Result<Vec<PageContent>, WebError> {
        if urls.is_empty() || urls.len() > guard::MAX_CONTENTS_URLS {
            return Err(WebError::Blocked);
        }
        for url in urls {
            if !guard::is_fetchable_url(url) {
                return Err(WebError::Blocked);
            }
        }
        match self.kind {
            SearchKind::Anysearch => self.contents_anysearch(urls, cancel).await,
            SearchKind::Querit | SearchKind::Custom => self.contents_querit(urls, cancel).await,
        }
    }

    async fn contents_querit(
        &self,
        urls: &[String],
        cancel: CancellationToken,
    ) -> Result<Vec<PageContent>, WebError> {
        let body = serde_json::to_vec(&serde_json::json!({
            "urls": urls,
            "format": "text",
            "crawlTimeout": 30_000,
            "extrasMeta": false,
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/v1/contents", &body, cancel).await?;
        let payload: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let pages_wire = payload["results"]
            .as_array()
            .or_else(|| payload["pages"].as_array())
            .ok_or(WebError::Protocol)?;
        self.collect_pages(urls, pages_wire)
    }

    async fn contents_anysearch(
        &self,
        urls: &[String],
        cancel: CancellationToken,
    ) -> Result<Vec<PageContent>, WebError> {
        let mut pages = Vec::new();
        let mut total = 0usize;
        for url in urls {
            let body = serde_json::to_vec(&serde_json::json!({ "url": url }))
                .map_err(|_| WebError::Protocol)?;
            let raw = self.post("/v1/extract", &body, cancel.clone()).await?;
            let payload: serde_json::Value =
                serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
            if payload["code"].as_i64().is_some_and(|code| code != 0) {
                return Err(WebError::Unavailable);
            }
            let data = &payload["data"];
            let returned = data["url"].as_str().unwrap_or(url);
            if returned != url {
                return Err(WebError::Protocol);
            }
            let content = guard::sanitize_remote_text(data["content"].as_str().unwrap_or_default());
            let mut page = PageContent {
                url: url.clone(),
                content,
                truncated: false,
            };
            if page.content.chars().count() > guard::MAX_PAGE_BYTES {
                page.content = page.content.chars().take(guard::MAX_PAGE_BYTES).collect();
                page.truncated = true;
            }
            total += page.content.len();
            if total > guard::MAX_RESPONSE_BYTES {
                page.truncated = true;
                pages.push(page);
                return Ok(pages);
            }
            pages.push(page);
        }
        Ok(pages)
    }

    fn collect_pages(
        &self,
        urls: &[String],
        pages_wire: &[serde_json::Value],
    ) -> Result<Vec<PageContent>, WebError> {
        let allowed: std::collections::HashSet<&String> = urls.iter().collect();
        let mut total = 0usize;
        let mut pages = Vec::new();
        for wire in pages_wire {
            let url = wire["url"].as_str().ok_or(WebError::Protocol)?.to_owned();
            if !allowed.contains(&url) {
                return Err(WebError::Protocol);
            }
            let content = guard::sanitize_remote_text(wire["content"].as_str().unwrap_or_default());
            let mut page = PageContent {
                url,
                content,
                truncated: wire["truncated"].as_bool().unwrap_or(false),
            };
            if page.content.chars().count() > guard::MAX_PAGE_BYTES {
                page.content = page.content.chars().take(guard::MAX_PAGE_BYTES).collect();
                page.truncated = true;
            }
            total += page.content.len();
            if total > guard::MAX_RESPONSE_BYTES {
                page.truncated = true;
                pages.push(page);
                return Ok(pages);
            }
            pages.push(page);
        }
        Ok(pages)
    }

    async fn post(
        &self,
        path: &str,
        body: &[u8],
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, WebError> {
        let endpoint = format!("{}{path}", self.endpoint);
        let raw = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WebError::Cancelled),
            raw = self
                .transport
                .post_json(&endpoint, self.bearer.as_deref(), body, self.timeout, cancel.clone()) => raw?,
        };
        if raw.len() > guard::MAX_RESPONSE_BYTES {
            return Err(WebError::Protocol);
        }
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct MockTransport {
        response: Mutex<Vec<u8>>,
        endpoints: Mutex<Vec<String>>,
        bearers: Mutex<Vec<Option<String>>>,
        bodies: Mutex<Vec<Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl WebTransport for MockTransport {
        async fn post_json(
            &self,
            endpoint: &str,
            bearer: Option<&str>,
            body: &[u8],
            _timeout: Duration,
            _cancel: CancellationToken,
        ) -> Result<Vec<u8>, WebError> {
            self.endpoints
                .lock()
                .expect("endpoints")
                .push(endpoint.to_owned());
            self.bearers
                .lock()
                .expect("bearers")
                .push(bearer.map(str::to_owned));
            self.bodies.lock().expect("bodies").push(body.to_vec());
            Ok(self.response.lock().expect("response").clone())
        }
    }

    fn client(response: serde_json::Value) -> (WebClient, Arc<MockTransport>) {
        let transport = Arc::new(MockTransport {
            response: Mutex::new(serde_json::to_vec(&response).expect("encode")),
            endpoints: Mutex::new(Vec::new()),
            bearers: Mutex::new(Vec::new()),
            bodies: Mutex::new(Vec::new()),
        });
        let client = WebClient::new(
            "https://search.example.com",
            Some("key-1".to_owned()),
            transport.clone(),
        )
        .expect("client");
        (client, transport)
    }

    #[tokio::test]
    async fn search_hits_v1_search_with_bearer_and_querit_shape() {
        let (client, transport) = client(serde_json::json!({
            "results": {"result": [
                {"url": "https://docs.example.com/a", "title": "A", "snippet": "first"},
                {"url": "https://localhost/secret", "title": "bad", "snippet": "filtered"},
                {"url": "http://insecure.example.com", "title": "bad2", "snippet": "filtered"},
            ]}
        }));
        let results = client
            .search("rust async", 10, CancellationToken::new())
            .await
            .expect("search");
        assert_eq!(results.len(), 1, "non-https and local hits are filtered");
        assert_eq!(results[0].url, "https://docs.example.com/a");
        assert_eq!(
            transport.endpoints.lock().expect("endpoints")[0],
            "https://search.example.com/v1/search"
        );
        assert_eq!(
            transport.bearers.lock().expect("bearers")[0],
            Some("key-1".to_owned()),
            "the API key rides the Authorization header"
        );
        let request: serde_json::Value =
            serde_json::from_slice(&transport.bodies.lock().expect("bodies")[0])
                .expect("request body");
        assert_eq!(request["query"], "rust async");
        assert_eq!(request["count"], 10);
    }

    #[tokio::test]
    async fn search_hits_anysearch_data_results_with_max_results() {
        let (client, transport) = client(serde_json::json!({
            "code": 0,
            "message": "success",
            "data": {"results": [
                {"url": "https://docs.example.com/a", "title": "A", "snippet": "first"},
                {"url": "https://localhost/secret", "title": "bad", "snippet": "filtered"},
            ]}
        }));
        let client = client.with_kind(SearchKind::Anysearch);
        let results = client
            .search("rust async", 8, CancellationToken::new())
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://docs.example.com/a");
        assert_eq!(
            transport.endpoints.lock().expect("endpoints")[0],
            "https://search.example.com/v1/search"
        );
        let request: serde_json::Value =
            serde_json::from_slice(&transport.bodies.lock().expect("bodies")[0])
                .expect("request body");
        assert_eq!(request["query"], "rust async");
        assert_eq!(request["max_results"], 8);
        assert!(request.get("count").is_none());
    }

    #[tokio::test]
    async fn contents_anysearch_extracts_one_url_at_a_time() {
        let (client, transport) = client(serde_json::json!({
            "code": 0,
            "data": {
                "url": "https://docs.example.com/a",
                "content": "page text"
            }
        }));
        let client = client.with_kind(SearchKind::Anysearch);
        let pages = client
            .contents(
                &["https://docs.example.com/a".to_owned()],
                CancellationToken::new(),
            )
            .await
            .expect("pages");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].content, "page text");
        assert_eq!(
            transport.endpoints.lock().expect("endpoints")[0],
            "https://search.example.com/v1/extract"
        );
        let request: serde_json::Value =
            serde_json::from_slice(&transport.bodies.lock().expect("bodies")[0])
                .expect("request body");
        assert_eq!(request["url"], "https://docs.example.com/a");
    }

    #[tokio::test]
    async fn search_accepts_the_flat_custom_shape_without_a_key() {
        let transport = Arc::new(MockTransport {
            response: Mutex::new(
                serde_json::to_vec(&serde_json::json!({
                    "results": [
                        {"url": "https://docs.example.com/a", "title": "A", "snippet": "s"},
                    ]
                }))
                .expect("encode"),
            ),
            endpoints: Mutex::new(Vec::new()),
            bearers: Mutex::new(Vec::new()),
            bodies: Mutex::new(Vec::new()),
        });
        let client =
            WebClient::new("https://search.example.com", None, transport.clone()).expect("client");
        let results = client
            .search("rust", 5, CancellationToken::new())
            .await
            .expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(
            transport.bearers.lock().expect("bearers")[0],
            None,
            "custom backends run without credentials"
        );
    }

    #[tokio::test]
    async fn contents_requires_https_urls_and_bound_count() {
        let (client, _transport) = client(serde_json::json!({"pages": []}));
        assert_eq!(
            client
                .contents(
                    &["http://insecure.example.com".to_owned()],
                    CancellationToken::new()
                )
                .await
                .expect_err("insecure url"),
            WebError::Blocked
        );
        let many: Vec<String> = (0..guard::MAX_CONTENTS_URLS + 1)
            .map(|index| format!("https://example{index}.com"))
            .collect();
        assert_eq!(
            client
                .contents(&many, CancellationToken::new())
                .await
                .expect_err("too many urls"),
            WebError::Blocked
        );
    }

    #[tokio::test]
    async fn contents_rejects_unknown_urls_and_marks_truncation() {
        let big = "x".repeat(guard::MAX_PAGE_BYTES + 16);
        let (bound, _transport) = client(serde_json::json!({
            "pages": [
                {"url": "https://docs.example.com/a", "content": "hello", "truncated": false},
                {"url": "https://docs.example.com/big", "content": big, "truncated": false},
            ]
        }));
        let pages = bound
            .contents(
                &[
                    "https://docs.example.com/a".to_owned(),
                    "https://docs.example.com/big".to_owned(),
                ],
                CancellationToken::new(),
            )
            .await
            .expect("pages");
        assert_eq!(pages.len(), 2);
        assert!(!pages[0].truncated);
        assert!(pages[1].truncated);
        assert_eq!(pages[1].content.chars().count(), guard::MAX_PAGE_BYTES);

        // A backend returning pages for URLs that were never requested
        // violates the contract and fails closed.
        let (rebound, _transport) = client(serde_json::json!({
            "pages": [
                {"url": "https://evil.example.com/injected", "content": "smuggled", "truncated": false},
            ]
        }));
        assert!(matches!(
            rebound
                .contents(
                    &["https://docs.example.com/a".to_owned()],
                    CancellationToken::new()
                )
                .await,
            Err(WebError::Protocol)
        ));
    }

    #[tokio::test]
    async fn backend_endpoint_must_be_fetchable() {
        let transport = Arc::new(MockTransport {
            response: Mutex::new(Vec::new()),
            endpoints: Mutex::new(Vec::new()),
            bearers: Mutex::new(Vec::new()),
            bodies: Mutex::new(Vec::new()),
        });
        assert!(matches!(
            WebClient::new("http://localhost:9000", None, transport),
            Err(WebError::Blocked)
        ));
    }
}
