//! Cloud-synced provider and model catalog.
//!
//! The catalog mirrors pi's model-data strategy: a normalized snapshot
//! generated from models.dev is vendored into the binary as the offline
//! baseline, a cached copy lives in the owned home, and a background
//! refresh re-downloads the cloud document with conditional requests.
//! Provider presets and model discovery in the desktop UI read this
//! catalog, so new vendors and models appear without an app update.
use serde::{Deserialize, Serialize};

pub mod modelsdev;
pub mod store;

pub use store::{
    CachedCatalog, DEFAULT_MAX_AGE_SECS, RefreshOutcome, bundled, current, http_client, load_cache,
    refresh,
};

/// Wire protocol: Anthropic Messages.
pub const KIND_ANTHROPIC_MESSAGES: &str = "anthropic-messages";
/// Wire protocol: OpenAI Chat Completions.
pub const KIND_OPENAI_COMPLETIONS: &str = "openai-completions";

/// Auth: a pasted API key stored in the secret vault (default).
pub const AUTH_API_KEY: &str = "";
/// Auth: an OAuth device-code sign-in (GitHub Copilot).
pub const AUTH_DEVICE_CODE: &str = "device-code";

/// Upper bound for provider entries in one catalog.
pub const MAX_PROVIDERS: usize = 1024;
/// Upper bound for model entries in one provider.
pub const MAX_MODELS_PER_PROVIDER: usize = 512;
/// Upper bound for one catalog string field.
pub const MAX_STRING_BYTES: usize = 8 * 1024;

/// One model preset in the catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogModel {
    /// Model id as sent to the provider.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Supports reasoning output.
    pub reasoning: bool,
    /// Supports tool calling.
    pub tool_call: bool,
    /// Supports image attachments.
    pub attachment: bool,
    /// Advertised context window in tokens; 0 when unknown.
    pub context: u64,
    /// Advertised output limit in tokens; 0 when unknown.
    pub output: u64,
    /// Input cost per million tokens, when published.
    pub cost_in: Option<f64>,
    /// Output cost per million tokens, when published.
    pub cost_out: Option<f64>,
}

/// One provider preset: endpoint data plus its model list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CatalogProvider {
    /// Stable provider id (models.dev spelling).
    pub id: String,
    /// Display name.
    pub name: String,
    /// MYCode wire protocol for this endpoint.
    pub kind: String,
    /// API base URL.
    pub base_url: String,
    /// Documentation URL, when published.
    pub doc: Option<String>,
    /// Credential mode: `""` (default) pastes an API key; `device-code`
    /// signs in with an OAuth device flow.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// Model presets, sorted by id.
    pub models: Vec<CatalogModel>,
}

impl Default for CatalogProvider {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: KIND_OPENAI_COMPLETIONS.to_owned(),
            base_url: String::new(),
            doc: None,
            auth: String::new(),
            models: Vec::new(),
        }
    }
}

/// The complete provider catalog.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogDocument {
    /// Providers, sorted by id.
    #[serde(default)]
    pub providers: Vec<CatalogProvider>,
}

impl CatalogDocument {
    /// Looks one provider up by id.
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&CatalogProvider> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// Returns a display name for a provider id, even when absent.
    #[must_use]
    pub fn display_name(&self, id: &str) -> String {
        self.provider(id)
            .map(|provider| provider.name.clone())
            .unwrap_or_else(|| id.to_owned())
    }
}

/// Validates one catalog string field: bounded UTF-8 without control noise.
pub(crate) fn clean_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > MAX_STRING_BYTES
        || trimmed.chars().any(char::is_control)
    {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Accepts a provider id when it is a lowercase portable spelling.
pub(crate) fn valid_provider_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// Normalizes a parsed catalog: drops invalid entries, bounds model lists,
/// and sorts providers and models by id.
pub(crate) fn normalize(mut providers: Vec<CatalogProvider>) -> CatalogDocument {
    providers.retain(|provider| {
        valid_provider_id(&provider.id)
            && clean_text(&provider.name).is_some()
            && clean_text(&provider.base_url).is_some()
            && matches!(
                provider.kind.as_str(),
                KIND_ANTHROPIC_MESSAGES | KIND_OPENAI_COMPLETIONS
            )
            && !provider.models.is_empty()
    });
    for provider in &mut providers {
        if provider.models.len() > MAX_MODELS_PER_PROVIDER {
            provider.models.truncate(MAX_MODELS_PER_PROVIDER);
        }
        for model in &mut provider.models {
            if clean_text(&model.name).is_none() {
                model.name = model.id.clone();
            }
        }
        provider.models.sort_by(|a, b| a.id.cmp(&b.id));
        provider.models.dedup_by(|a, b| a.id == b.id);
    }
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    providers.dedup_by(|a, b| a.id == b.id);
    CatalogDocument { providers }
}

/// Parses the vendored normalized snapshot into a catalog document.
///
/// The snapshot is generated by `scripts/generate_catalog.py` from models.dev
/// and compiled into the binary; a decode failure yields an empty catalog
/// rather than a panic, and the unit tests keep that failure impossible.
pub fn parse_snapshot(bytes: &[u8]) -> CatalogDocument {
    serde_json::from_slice::<CatalogDocument>(bytes)
        .map(|document| normalize(document.providers))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_snapshot_is_valid_and_populated() {
        let catalog = bundled();
        assert!(
            catalog.providers.len() >= 100,
            "snapshot carries the models.dev baseline: {}",
            catalog.providers.len()
        );
        for provider in &catalog.providers {
            assert!(valid_provider_id(&provider.id), "id {}", provider.id);
            assert!(provider.base_url.starts_with("https://"));
            assert!(matches!(
                provider.kind.as_str(),
                KIND_ANTHROPIC_MESSAGES | KIND_OPENAI_COMPLETIONS
            ));
            assert!(!provider.models.is_empty());
        }
        for wanted in ["anthropic", "openai", "openrouter", "deepseek", "minimax"] {
            assert!(
                catalog.provider(wanted).is_some(),
                "baseline keeps {wanted}"
            );
        }
    }

    #[test]
    fn normalize_drops_invalid_entries_and_sorts() {
        let document = normalize(vec![
            CatalogProvider {
                id: "b-good".to_owned(),
                name: "B".to_owned(),
                kind: KIND_OPENAI_COMPLETIONS.to_owned(),
                base_url: "https://b.example.com/v1".to_owned(),
                doc: None,
                auth: String::new(),
                models: vec![CatalogModel {
                    id: "m2".to_owned(),
                    name: "  ".to_owned(),
                    ..CatalogModel::default()
                }],
            },
            CatalogProvider {
                id: "Bad Id".to_owned(),
                name: "bad".to_owned(),
                kind: KIND_OPENAI_COMPLETIONS.to_owned(),
                base_url: "https://x.example.com".to_owned(),
                doc: None,
                auth: String::new(),
                models: vec![CatalogModel::default()],
            },
            CatalogProvider {
                id: "a-nomodels".to_owned(),
                name: "A".to_owned(),
                kind: KIND_OPENAI_COMPLETIONS.to_owned(),
                base_url: "https://a.example.com".to_owned(),
                doc: None,
                auth: String::new(),
                models: Vec::new(),
            },
            CatalogProvider {
                id: "c-kind".to_owned(),
                name: "C".to_owned(),
                kind: "unknown".to_owned(),
                base_url: "https://c.example.com".to_owned(),
                doc: None,
                auth: String::new(),
                models: vec![CatalogModel::default()],
            },
        ]);
        assert_eq!(
            document
                .providers
                .iter()
                .map(|provider| provider.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b-good"]
        );
        let provider = &document.providers[0];
        assert_eq!(provider.models[0].name, "m2", "blank names fall back to id");
    }
}
