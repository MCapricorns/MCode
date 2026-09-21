//! reqwest-backed [`WebTransport`] production implementation.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::{WebError, WebTransport};

/// Production transport for the bounded web client.
#[derive(Clone, Default)]
pub struct ReqwestWebTransport {
    client: std::sync::Arc<reqwest::Client>,
}

impl ReqwestWebTransport {
    /// Builds the shared client.
    ///
    /// # Errors
    ///
    /// Returns [`WebError::Unavailable`] when the TLS backend fails.
    pub fn new() -> Result<Self, WebError> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|_| WebError::Unavailable)?;
        Ok(Self {
            client: std::sync::Arc::new(client),
        })
    }
}

#[async_trait::async_trait]
impl WebTransport for ReqwestWebTransport {
    async fn post_json(
        &self,
        endpoint: &str,
        bearer: Option<&str>,
        body: &[u8],
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<Vec<u8>, WebError> {
        let mut request = self
            .client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(timeout)
            .body(body.to_vec());
        if let Some(key) = bearer {
            request = request.bearer_auth(key);
        }
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WebError::Cancelled),
            sent = request.send() => sent.map_err(|_| WebError::Unavailable)?,
        };
        if !response.status().is_success() {
            return Err(WebError::Unavailable);
        }
        let bytes = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WebError::Cancelled),
            bytes = response.bytes() => bytes.map_err(|_| WebError::Protocol)?,
        };
        Ok(bytes.to_vec())
    }
}
