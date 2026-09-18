//! First-party bounded web search over a Querit-compatible backend.
//!
//! The wire contract is `POST {endpoint}/v1/search` for ranked results and
//! `POST {endpoint}/v1/contents` for page text. Everything is bounded: result
//! count, URL count, response bytes, and aggregate payload bytes. The
//! transport is injectable so tests run without any network; the production
//! [`reqwest_transport::ReqwestWebTransport`] ships with the crate.

pub mod guard;
pub mod reqwest_transport;

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
    /// Posts a JSON body and returns the raw response bytes.
    ///
    /// # Errors
    ///
    /// Returns [`WebError`] for transport-level failures.
    async fn post_json(
        &self,
        endpoint: &str,
        body: &[u8],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, WebError>;
}

/// Client bound to one backend endpoint.
#[derive(Clone)]
pub struct WebClient {
    endpoint: String,
    transport: Arc<dyn WebTransport>,
    timeout: Duration,
}

impl WebClient {
    /// Creates one client over an https backend endpoint from settings.
    ///
    /// # Errors
    ///
    /// Returns [`WebError::Blocked`] when the endpoint itself violates the
    /// URL policy.
    pub fn new(endpoint: &str, transport: Arc<dyn WebTransport>) -> Result<Self, WebError> {
        if !guard::is_fetchable_url(endpoint) {
            return Err(WebError::Blocked);
        }
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            transport,
            timeout: Duration::from_secs(guard::DEFAULT_TIMEOUT_SECS),
        })
    }

    /// Runs one bounded search.
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
        let body = serde_json::to_vec(&serde_json::json!({
            "query": query,
            "maxResults": max_results,
        }))
        .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/v1/search", &body, cancel).await?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            #[serde(default)]
            results: Vec<SearchResult>,
        }
        let wire: Wire = serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let results: Vec<SearchResult> = wire
            .results
            .into_iter()
            .take(max_results)
            .filter(|result| guard::is_fetchable_url(&result.url))
            .collect();
        if results.len() > guard::MAX_SEARCH_RESULTS {
            return Err(WebError::Protocol);
        }
        Ok(results)
    }

    /// Fetches bounded page text for the given URLs.
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
        let body = serde_json::to_vec(&serde_json::json!({ "urls": urls }))
            .map_err(|_| WebError::Protocol)?;
        let raw = self.post("/v1/contents", &body, cancel).await?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            #[serde(default)]
            pages: Vec<PageContent>,
        }
        let wire: Wire = serde_json::from_slice(&raw).map_err(|_| WebError::Protocol)?;
        let allowed: std::collections::HashSet<&String> = urls.iter().collect();
        let mut total = 0usize;
        let mut pages = Vec::new();
        for mut page in wire.pages {
            if !allowed.contains(&page.url) {
                return Err(WebError::Protocol);
            }
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
                .post_json(&endpoint, body, self.timeout, cancel.clone()) => raw?,
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
    }

    #[async_trait::async_trait]
    impl WebTransport for MockTransport {
        async fn post_json(
            &self,
            endpoint: &str,
            _body: &[u8],
            _timeout: Duration,
            _cancel: CancellationToken,
        ) -> Result<Vec<u8>, WebError> {
            self.endpoints
                .lock()
                .expect("endpoints")
                .push(endpoint.to_owned());
            Ok(self.response.lock().expect("response").clone())
        }
    }

    fn client(response: serde_json::Value) -> (WebClient, Arc<MockTransport>) {
        let transport = Arc::new(MockTransport {
            response: Mutex::new(serde_json::to_vec(&response).expect("encode")),
            endpoints: Mutex::new(Vec::new()),
        });
        let client =
            WebClient::new("https://search.example.com", transport.clone()).expect("client");
        (client, transport)
    }

    #[tokio::test]
    async fn search_returns_bounded_results_and_hits_v1_search() {
        let (client, transport) = client(serde_json::json!({
            "results": [
                {"url": "https://docs.example.com/a", "title": "A", "snippet": "first"},
                {"url": "https://localhost/secret", "title": "bad", "snippet": "filtered"},
                {"url": "http://insecure.example.com", "title": "bad2", "snippet": "filtered"},
            ]
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
        });
        assert!(matches!(
            WebClient::new("http://localhost:9000", transport),
            Err(WebError::Blocked)
        ));
    }
}
