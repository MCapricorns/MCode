//! Parser for the raw models.dev `api.json` cloud document.
//!
//! The cloud document is normalized into the same compact schema as the
//! vendored snapshot: only providers MCode can serve (anthropic-messages or
//! OpenAI-compatible endpoints) are kept, first-party labs without a
//! published `api` field are filled from a pinned endpoint table, and
//! console-auth vendors (cloud SDKs, OAuth flows) are excluded.
use serde::Deserialize;

use crate::{
    CatalogDocument, CatalogModel, CatalogProvider, KIND_ANTHROPIC_MESSAGES,
    KIND_OPENAI_COMPLETIONS, clean_text, normalize, valid_provider_id,
};

/// Cloud source of the provider catalog.
pub const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";

/// Known base URLs for first-party labs that models.dev lists without an
/// `api` field. Pinned from the vendors' published endpoints.
fn endpoint_fix(provider_id: &str) -> Option<(&'static str, &'static str)> {
    match provider_id {
        "anthropic" => Some((KIND_ANTHROPIC_MESSAGES, "https://api.anthropic.com")),
        "openai" => Some((KIND_OPENAI_COMPLETIONS, "https://api.openai.com/v1")),
        "groq" => Some((KIND_OPENAI_COMPLETIONS, "https://api.groq.com/openai/v1")),
        "mistral" => Some((KIND_OPENAI_COMPLETIONS, "https://api.mistral.ai/v1")),
        "xai" => Some((KIND_OPENAI_COMPLETIONS, "https://api.x.ai/v1")),
        "cerebras" => Some((KIND_OPENAI_COMPLETIONS, "https://api.cerebras.ai/v1")),
        "perplexity" => Some((KIND_OPENAI_COMPLETIONS, "https://api.perplexity.ai")),
        _ => None,
    }
}

/// Providers excluded from presets: their credentials are not portable API
/// keys (cloud SDKs, OAuth device flows).
fn excluded(provider_id: &str, npm: &str) -> bool {
    provider_id == "github-copilot"
        || ["bedrock", "vertex", "azure", "google"]
            .iter()
            .any(|token| npm.contains(token))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevModel {
    name: String,
    status: String,
    reasoning: bool,
    tool_call: bool,
    attachment: bool,
    limit: ModelsDevLimit,
    cost: ModelsDevCost,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevLimit {
    context: u64,
    output: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevCost {
    input: serde_json::Value,
    output: serde_json::Value,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModelsDevProvider {
    npm: String,
    name: String,
    api: String,
    doc: String,
    models: std::collections::BTreeMap<String, ModelsDevModel>,
}

fn number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|cost| *cost >= 0.0 && cost.is_finite())
}

/// Parses and normalizes a raw models.dev `api.json` payload.
#[must_use]
pub fn parse_models_dev(bytes: &[u8]) -> CatalogDocument {
    let Ok(raw) =
        serde_json::from_slice::<std::collections::BTreeMap<String, ModelsDevProvider>>(bytes)
    else {
        return CatalogDocument::default();
    };
    let mut providers = Vec::new();
    for (provider_id, entry) in raw {
        if !valid_provider_id(&provider_id) || excluded(&provider_id, &entry.npm) {
            continue;
        }
        let (kind, base_url) = if entry.npm.contains("anthropic") {
            (KIND_ANTHROPIC_MESSAGES, entry.api.as_str())
        } else {
            (KIND_OPENAI_COMPLETIONS, entry.api.as_str())
        };
        let (kind, base_url) = match clean_text(base_url) {
            Some(base) if base.starts_with("https://") => (kind.to_owned(), base),
            _ => match endpoint_fix(&provider_id) {
                Some((kind, base)) => (kind.to_owned(), base.to_owned()),
                None => continue,
            },
        };
        let mut models = Vec::new();
        for (model_id, model) in entry.models {
            if models.len() >= crate::MAX_MODELS_PER_PROVIDER {
                break;
            }
            if !valid_model_id(&model_id) || !clean_text(&model.status).is_none() {
                continue;
            }
            models.push(CatalogModel {
                id: model_id.clone(),
                name: clean_text(&model.name).unwrap_or(model_id),
                reasoning: model.reasoning,
                tool_call: model.tool_call,
                attachment: model.attachment,
                context: model.limit.context,
                output: model.limit.output,
                cost_in: number(&model.cost.input),
                cost_out: number(&model.cost.output),
            });
        }
        if models.is_empty() {
            continue;
        }
        providers.push(CatalogProvider {
            id: provider_id,
            name: clean_text(&entry.name).unwrap_or_else(|| "provider".to_owned()),
            kind: kind.to_owned(),
            base_url,
            doc: clean_text(&entry.doc).filter(|doc| doc.starts_with("https://")),
            models,
        });
    }
    normalize(providers)
}

/// Model ids keep the provider's own spelling: any nonempty printable id
/// without whitespace or control characters is accepted.
fn valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= crate::MAX_STRING_BYTES
        && id.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cloud_document_and_applies_wire_rules() {
        let raw = br#"{
            "anthropic": {"npm": "@ai-sdk/anthropic", "name": "Anthropic",
                "models": {"claude-x": {"name": "Claude X", "reasoning": true,
                    "tool_call": true, "limit": {"context": 200000, "output": 64000},
                    "cost": {"input": 3.0, "output": 15.0}}}},
            "openai": {"npm": "@ai-sdk/openai", "name": "OpenAI",
                "models": {"gpt-x": {"name": "GPT X"}}},
            "google": {"npm": "@ai-sdk/google", "name": "Google",
                "models": {"gemini-x": {"name": "Gemini X"}}},
            "acme": {"npm": "@ai-sdk/openai-compatible", "api": "https://api.acme.dev/v1",
                "name": "Acme", "models": {"m1": {}, "m2": {"status": "deprecated"}}},
            "bad id": {"npm": "@ai-sdk/openai-compatible", "api": "https://x.example.com",
                "models": {"m": {}}}
        }"#;
        let document = parse_models_dev(raw);
        let ids: Vec<&str> = document
            .providers
            .iter()
            .map(|provider| provider.id.as_str())
            .collect();
        assert_eq!(ids, vec!["acme", "anthropic", "openai"]);

        let anthropic = document.provider("anthropic").expect("anthropic preset");
        assert_eq!(anthropic.kind, KIND_ANTHROPIC_MESSAGES);
        assert_eq!(anthropic.base_url, "https://api.anthropic.com");
        let model = &anthropic.models[0];
        assert!(model.reasoning && model.tool_call);
        assert_eq!(model.context, 200_000);
        assert_eq!(model.cost_out, Some(15.0));

        let acme = document.provider("acme").expect("acme preset");
        assert_eq!(acme.models.len(), 1, "deprecated entries dropped");

        let openai = document.provider("openai").expect("openai preset");
        assert_eq!(openai.kind, KIND_OPENAI_COMPLETIONS);
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn malformed_documents_yield_an_empty_catalog() {
        assert!(parse_models_dev(b"not json").providers.is_empty());
        assert!(parse_models_dev(b"[]").providers.is_empty());
    }
}
