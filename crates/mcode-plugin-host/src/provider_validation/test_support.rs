//! Shared construction of generated Provider DTO test values.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    CacheRetention, CapabilitySupport, CatalogEntry, InputModality, ModelSelection, PrepareInput,
    Reasoning, ReasoningCapability, ToolCapability, ToolChoice, WireJsonDocument, WireJsonNode,
    WireJsonObject,
};

use super::prepare::SelectedCatalogView;

pub(super) const DIGEST: &str =
    "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
pub(super) const OTHER_DIGEST: &str =
    "sha256:1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub(super) fn supported_tools() -> ToolCapability {
    ToolCapability {
        tools: CapabilitySupport::Supported,
        auto_choice: CapabilitySupport::Supported,
        none_choice: CapabilitySupport::Supported,
        specific_choice: CapabilitySupport::Supported,
    }
}

pub(super) fn supported_reasoning() -> ReasoningCapability {
    ReasoningCapability {
        reasoning: CapabilitySupport::Supported,
        effort: CapabilitySupport::Supported,
        budget: CapabilitySupport::Supported,
        proof: CapabilitySupport::Supported,
    }
}

pub(super) fn catalog_entry(model: &str) -> CatalogEntry {
    CatalogEntry {
        selection: ModelSelection::Exact(model.to_owned()),
        current_model: model.to_owned(),
        display_name: Some("Model".to_owned()),
        input_modalities: vec![InputModality::Text, InputModality::Image],
        tool_capability: supported_tools(),
        reasoning_capability: supported_reasoning(),
        context_tokens: Some(4_096),
        max_output_tokens: Some(1_024),
        completion_operation: "complete".to_owned(),
    }
}

pub(super) fn empty_object() -> WireJsonDocument {
    WireJsonDocument {
        root: 0,
        nodes: vec![WireJsonNode::ObjectValue(WireJsonObject { fields: vec![] })],
    }
}

pub(super) fn prepare_input() -> PrepareInput {
    PrepareInput {
        provider_id: "provider".to_owned(),
        route_id: "route".to_owned(),
        catalog_digest: DIGEST.to_owned(),
        selection: ModelSelection::Exact("model".to_owned()),
        current_model: "model".to_owned(),
        operation_id: "complete".to_owned(),
        request_id: "request-1".to_owned(),
        turn_id: "turn-1".to_owned(),
        system: vec![],
        messages: vec![],
        tools: vec![],
        tool_choice: ToolChoice::Unset,
        reasoning: Reasoning::Unset,
        cache_retention: CacheRetention::Unset,
        max_output_tokens: None,
    }
}

pub(super) fn selected(entry: &CatalogEntry) -> SelectedCatalogView<'_> {
    SelectedCatalogView {
        provider_id: "provider",
        route_id: "route",
        catalog_digest: DIGEST,
        entry,
    }
}
