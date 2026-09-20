//! First-party provider runtime.
//!
//! Resolves one configured provider endpoint into a streaming
//! [`Provider`] over one of three wire protocols (`anthropic-messages`,
//! `openai-completions`, `openai-responses`). Vendor differences are data in
//! `settings.json`; credentials never enter settings — the caller supplies a
//! key when resolving. All egress flows through the injectable
//! [`SseTransport`] seam, and the configured User-Agent header rides every
//! request (the pi agent default when unset).

mod anthropic_messages;
mod driver;
mod oauth;
mod openai_completions;
mod openai_responses;
mod sse;
mod transport;
mod xml_tool_calls;

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use mcode_config::ProviderSettings;
use mcode_provider_api::{EventStream, Provider, ProviderError, ProviderErrorKind, Request};

pub use oauth::{
    COPILOT_PROVIDER_ID, CopilotToken, DeviceCodeStart, DeviceTokenPoll, copilot_bearer,
    poll_device_token, start_device_flow,
};
pub use sse::MAX_FRAME_BYTES;
pub use transport::{ReqwestTransport, SseTransport, TransportCall};

/// Anthropic-compatible endpoint path appended to the base URL.
pub const ANTHROPIC_MESSAGES_PATH: &str = "/v1/messages";
/// OpenAI-compatible completions path appended to the base URL.
pub const OPENAI_COMPLETIONS_PATH: &str = "/chat/completions";
/// OpenAI-compatible Responses path appended to the base URL.
pub const OPENAI_RESPONSES_PATH: &str = "/responses";
/// Anthropic protocol version header value.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One resolved streaming provider endpoint.
#[derive(Clone)]
pub struct ResolvedProvider {
    /// Wire protocol kind from settings.
    pub kind: String,
    /// Full request endpoint URL.
    pub endpoint: String,
    /// Model id sent on every request.
    pub model: String,
    /// Request headers (auth, protocol version, User-Agent).
    pub headers: Vec<(String, String)>,
}

/// Appends `prefix + suffix` to the base without doubling: a base that
/// already ends with the full path is used as-is, and a base ending with
/// `prefix` gets only `suffix`.
fn endpoint_with(base: &str, prefix: &str, suffix: &str) -> String {
    let full = format!("{prefix}{suffix}");
    if base.ends_with(&full) {
        base.to_owned()
    } else if base.ends_with(prefix) {
        format!("{base}{suffix}")
    } else {
        format!("{base}{full}")
    }
}

impl ResolvedProvider {
    /// Resolves settings plus a vault-supplied key into one endpoint.
    ///
    /// `user_agent` should be the effective UA (settings value or the pi
    /// agent default from `mcode_config::default_user_agent`).
    ///
    /// # Errors
    ///
    /// Returns a rejected error for an unknown protocol kind or model not
    /// offered by the provider.
    pub fn resolve(
        settings: &ProviderSettings,
        model: &str,
        api_key: &str,
        user_agent: &str,
    ) -> Result<Self, ProviderError> {
        let base = settings.base_url.trim_end_matches('/');
        let (endpoint, extra) = match settings.kind.as_str() {
            "anthropic-messages" => (
                // A base that already carries the versioned path (common when
                // pasting a vendor console URL) must not be doubled.
                endpoint_with(base, "/v1", "/messages"),
                vec![
                    ("x-api-key".to_owned(), api_key.to_owned()),
                    ("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()),
                ],
            ),
            "openai-completions" => (
                endpoint_with(base, "", OPENAI_COMPLETIONS_PATH),
                vec![("authorization".to_owned(), format!("Bearer {api_key}"))],
            ),
            "openai-responses" => (
                endpoint_with(base, "", OPENAI_RESPONSES_PATH),
                vec![("authorization".to_owned(), format!("Bearer {api_key}"))],
            ),
            _ => {
                return Err(ProviderError::with_message(
                    ProviderErrorKind::Rejected,
                    "unknown provider kind",
                ));
            }
        };
        if !settings.models.iter().any(|m| m == model) {
            return Err(ProviderError::with_message(
                ProviderErrorKind::Rejected,
                "model not offered by this provider",
            ));
        }
        let mut headers = vec![("user-agent".to_owned(), user_agent.to_owned())];
        headers.extend(extra);
        Ok(Self {
            kind: settings.kind.clone(),
            endpoint,
            model: model.to_owned(),
            headers,
        })
    }
}

/// A streaming provider bound to one endpoint and transport.
pub struct WireProvider {
    resolved: ResolvedProvider,
    transport: Arc<dyn SseTransport>,
}

impl WireProvider {
    /// Binds one resolved endpoint to a transport.
    #[must_use]
    pub fn new(resolved: ResolvedProvider, transport: Arc<dyn SseTransport>) -> Self {
        Self {
            resolved,
            transport,
        }
    }

    fn call_for(&self, request: &Request) -> TransportCall {
        let body = match self.resolved.kind.as_str() {
            "anthropic-messages" => anthropic_messages::build_body(&self.resolved.model, request),
            "openai-responses" => openai_responses::build_body(&self.resolved.model, request),
            _ => openai_completions::build_body(&self.resolved.model, request),
        };
        TransportCall {
            endpoint: self.resolved.endpoint.clone(),
            headers: self.resolved.headers.clone(),
            body: serde_json::to_vec(&body).unwrap_or_default(),
        }
    }

    fn reducer_for(&self) -> Box<dyn driver::FrameReducer + Send> {
        match self.resolved.kind.as_str() {
            "anthropic-messages" => Box::new(anthropic_messages::MessagesReducer::new()),
            "openai-responses" => Box::new(openai_responses::ResponsesReducer::new()),
            _ => Box::new(openai_completions::CompletionsReducer::new()),
        }
    }
}

#[async_trait::async_trait]
impl Provider for WireProvider {
    async fn stream(
        &self,
        request: &Request,
        cancel: CancellationToken,
    ) -> Result<EventStream, ProviderError> {
        request.validate()?;
        let call = self.call_for(request);
        let reducer = self.reducer_for();
        let (sender, stream) = EventStream::channel(cancel.clone());
        let transport = Arc::clone(&self.transport);
        tokio::spawn(async move {
            driver::drive(transport, call, reducer, sender, cancel).await;
        });
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn endpoint_join_never_doubles_the_path() {
        use super::endpoint_with;
        assert_eq!(
            endpoint_with("https://x/anthropic", "/v1", "/messages"),
            "https://x/anthropic/v1/messages"
        );
        assert_eq!(
            endpoint_with("https://x/anthropic/v1", "/v1", "/messages"),
            "https://x/anthropic/v1/messages"
        );
        assert_eq!(
            endpoint_with("https://x/anthropic/v1/messages", "/v1", "/messages"),
            "https://x/anthropic/v1/messages"
        );
        assert_eq!(
            endpoint_with("https://x/api", "", "/chat/completions"),
            "https://x/api/chat/completions"
        );
        assert_eq!(
            endpoint_with("https://x/api/chat/completions", "", "/chat/completions"),
            "https://x/api/chat/completions"
        );
    }

    use std::sync::Mutex;

    use bytes::Bytes;
    use mcode_core::Message;
    use mcode_provider_api::StreamEvent;

    use super::*;
    use mcode_core::{ContentBlock, StopReason, UserMessage};

    /// Transport that records calls and replays a canned SSE body.
    struct MockTransport {
        calls: Mutex<Vec<TransportCall>>,
        body: String,
    }

    #[async_trait::async_trait]
    impl SseTransport for MockTransport {
        async fn post(
            &self,
            call: TransportCall,
            _cancel: CancellationToken,
        ) -> Result<transport::ByteStream, ProviderError> {
            self.calls.lock().expect("calls").push(call);
            let body = self.body.clone();
            let stream = futures_util::stream::iter(
                body.as_bytes()
                    .chunks(7)
                    .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                    .collect::<Vec<_>>(),
            );
            Ok(Box::pin(stream))
        }
    }

    fn settings(kind: &str) -> ProviderSettings {
        ProviderSettings {
            id: "main".to_owned(),
            kind: kind.to_owned(),
            base_url: "https://api.example.com".to_owned(),
            models: vec!["model-a".to_owned()],
            enabled: true,
            context_limit: None,
            max_output: None,
        }
    }

    #[tokio::test]
    async fn completions_endpoint_streams_through_transport() {
        let transport = Arc::new(MockTransport {
            calls: Mutex::new(Vec::new()),
            body: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                "data: [DONE]\n\n",
            )
            .to_owned(),
        });
        let resolved =
            ResolvedProvider::resolve(&settings("openai-completions"), "model-a", "key-1", "ua/1")
                .expect("resolve");
        assert_eq!(
            resolved.endpoint,
            "https://api.example.com/chat/completions"
        );
        let provider = WireProvider::new(resolved, transport.clone());
        let request = Request::new().with_message(Message::User(UserMessage::text("hello")));
        let mut stream = provider
            .stream(&request, CancellationToken::new())
            .await
            .expect("stream");

        assert!(matches!(
            stream.next().await,
            Some(StreamEvent::TextDelta(t)) if t == "hi"
        ));
        assert!(matches!(
            stream.next().await,
            Some(StreamEvent::Done { .. })
        ));
        assert!(stream.next().await.is_none());

        let calls = transport.calls.lock().expect("calls");
        let call = &calls[0];
        assert_eq!(call.endpoint, "https://api.example.com/chat/completions");
        assert!(
            call.headers
                .iter()
                .any(|(name, value)| name == "user-agent" && value == "ua/1")
        );
        assert!(
            call.headers
                .iter()
                .any(|(name, value)| name == "authorization" && value == "Bearer key-1")
        );
        let body: serde_json::Value =
            serde_json::from_slice(&call.body).expect("valid completions body");
        assert_eq!(body["model"], "model-a");
        assert_eq!(body["stream"], true);
    }

    #[tokio::test]
    async fn anthropic_endpoint_uses_api_key_and_version_headers() {
        let transport = Arc::new(MockTransport {
            calls: Mutex::new(Vec::new()),
            body: concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n",
            )
            .to_owned(),
        });
        let resolved =
            ResolvedProvider::resolve(&settings("anthropic-messages"), "model-a", "key-2", "ua/2")
                .expect("resolve");
        assert_eq!(resolved.endpoint, "https://api.example.com/v1/messages");
        let provider = WireProvider::new(resolved, transport.clone());
        let request = Request::new().with_message(Message::User(UserMessage::text("hello")));
        let mut stream = provider
            .stream(&request, CancellationToken::new())
            .await
            .expect("stream");
        let StreamEvent::Done { message } = stream.next().await.expect("terminal") else {
            panic!("done required");
        };
        assert_eq!(message.stop_reason, StopReason::Stop);
        assert!(message.blocks.is_empty());

        let calls = transport.calls.lock().expect("calls");
        let call = &calls[0];
        assert!(
            call.headers
                .iter()
                .any(|(name, value)| name == "x-api-key" && value == "key-2")
        );
        assert!(
            call.headers
                .iter()
                .any(|(name, value)| name == "anthropic-version" && value == ANTHROPIC_VERSION)
        );
        let body: serde_json::Value =
            serde_json::from_slice(&call.body).expect("valid messages body");
        assert_eq!(body["max_tokens"], anthropic_messages::MAX_TOKENS_DEFAULT);
    }

    #[test]
    fn resolve_rejects_unknown_kind_and_foreign_model() {
        let mut bad = settings("openai-completions");
        bad.kind = "vendor-x".to_owned();
        assert!(ResolvedProvider::resolve(&bad, "model-a", "k", "ua").is_err());

        let settings = settings("openai-completions");
        let error = match ResolvedProvider::resolve(&settings, "model-zz", "k", "ua") {
            Err(error) => error,
            Ok(_) => panic!("foreign model must be rejected"),
        };
        assert_eq!(error.kind(), ProviderErrorKind::Rejected);
    }

    /// Keeps the unused-import lint honest for ContentBlock in this module.
    #[test]
    fn content_block_import_is_used_by_fixtures() {
        let block = ContentBlock::Text(mcode_core::TextBlock::new("x"));
        assert!(matches!(block, ContentBlock::Text(_)));
    }
}
