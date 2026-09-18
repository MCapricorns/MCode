//! Request policy, source debt, and per-wire projection rules.
use std::collections::BTreeSet;

use crate::provider_validation::catalog::{is_supported, same_selection};
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, CatalogEntry, Message, PrepareInput, Reasoning, ToolChoice, ToolResultBlock,
    UserBlock,
};

use super::types::{
    AdapterValidationError, AdapterValidationResult, AdapterWireId, ValidatedCatalogEntryView,
};

pub(super) fn validate_identity(
    selected: ValidatedCatalogEntryView<'_>,
    original: &PrepareInput,
) -> AdapterValidationResult<()> {
    if original.provider_id != selected.provider_id
        || original.route_id != selected.route_id
        || original.catalog_digest != selected.catalog_digest
        || !same_selection(&original.selection, selected.selection)
        || original.current_model != selected.current_model
        || original.operation_id != selected.completion_operation
    {
        return Err(AdapterValidationError::SourceMismatch);
    }
    Ok(())
}

pub(super) fn validate_capabilities(
    selected: ValidatedCatalogEntryView<'_>,
    original: &PrepareInput,
) -> AdapterValidationResult<()> {
    let mut has_calls = false;
    let mut has_images = false;
    let mut has_reasoning_blocks = false;
    let mut has_proofs = false;
    for message in &original.messages {
        match message {
            Message::User(message) => {
                has_images |= message
                    .blocks
                    .iter()
                    .any(|block| matches!(block, UserBlock::Image(_)));
            }
            Message::Assistant(message) => {
                has_calls |= message
                    .blocks
                    .iter()
                    .any(|block| matches!(block, AssistantBlock::ToolCall(_)));
                has_reasoning_blocks |= message
                    .blocks
                    .iter()
                    .any(|block| matches!(block, AssistantBlock::Reasoning(_)));
                has_proofs |= message.blocks.iter().any(
                    |block| matches!(block, AssistantBlock::Reasoning(value) if value.proof.is_some()),
                );
            }
            Message::ToolResult(message) => {
                has_images |= message
                    .blocks
                    .iter()
                    .any(|block| matches!(block, ToolResultBlock::Image(_)));
            }
        }
    }
    if (!original.tools.is_empty() || has_calls) && !is_supported(&selected.tool_capability.tools) {
        return Err(AdapterValidationError::CapabilityMismatch);
    }
    if has_images
        && !selected.input_modalities.iter().any(|value| {
            matches!(
            value,
            crate::provider_wit::exports::mcode::provider_pack::provider_api::InputModality::Image
        )
        })
    {
        return Err(AdapterValidationError::CapabilityMismatch);
    }
    if (has_reasoning_blocks || !matches!(original.reasoning, Reasoning::Unset))
        && !is_supported(&selected.reasoning_capability.reasoning)
    {
        return Err(AdapterValidationError::CapabilityMismatch);
    }
    if has_proofs && !is_supported(&selected.reasoning_capability.proof) {
        return Err(AdapterValidationError::CapabilityMismatch);
    }
    if let Reasoning::Enabled(enabled) = &original.reasoning {
        if enabled.effort.is_some() && !is_supported(&selected.reasoning_capability.effort) {
            return Err(AdapterValidationError::CapabilityMismatch);
        }
        if enabled.budget_tokens.is_some() && !is_supported(&selected.reasoning_capability.budget) {
            return Err(AdapterValidationError::CapabilityMismatch);
        }
    }
    match original.tool_choice {
        ToolChoice::Auto if !is_supported(&selected.tool_capability.auto_choice) => {
            return Err(AdapterValidationError::CapabilityMismatch);
        }
        ToolChoice::None if !is_supported(&selected.tool_capability.none_choice) => {
            return Err(AdapterValidationError::CapabilityMismatch);
        }
        ToolChoice::Specific(_) if !is_supported(&selected.tool_capability.specific_choice) => {
            return Err(AdapterValidationError::CapabilityMismatch);
        }
        _ => {}
    }
    if original.max_output_tokens.is_some_and(|value| {
        value == 0 || selected.max_output_tokens.is_none_or(|limit| value > limit)
    }) {
        return Err(AdapterValidationError::CapabilityMismatch);
    }
    Ok(())
}

pub(super) fn catalog_entry(selected: ValidatedCatalogEntryView<'_>) -> CatalogEntry {
    CatalogEntry {
        selection: selected.selection.clone(),
        current_model: selected.current_model.to_owned(),
        display_name: None,
        input_modalities: selected.input_modalities.to_vec(),
        tool_capability: *selected.tool_capability,
        reasoning_capability: *selected.reasoning_capability,
        context_tokens: selected.context_tokens,
        max_output_tokens: selected.max_output_tokens,
        completion_operation: selected.completion_operation.to_owned(),
    }
}

pub(super) fn expected_debts(wire: AdapterWireId, original: &PrepareInput) -> BTreeSet<String> {
    let mut debts = BTreeSet::from([
        "selection.variant".to_owned(),
        "selection.payload".to_owned(),
        "system.collection".to_owned(),
        "messages.collection".to_owned(),
        "tools.collection".to_owned(),
        "tool-choice.variant".to_owned(),
        "reasoning.variant".to_owned(),
        "cache.variant".to_owned(),
        "max-output".to_owned(),
    ]);
    for index in 0..original.system.len() {
        debts.insert(format!("system.{index}"));
    }
    for index in 0..original.tools.len() {
        for field in ["name", "description", "schema"] {
            debts.insert(format!("tool{index}.{field}"));
        }
    }
    for (message_index, message) in original.messages.iter().enumerate() {
        debts.insert(format!("m{message_index}.variant"));
        debts.insert(format!("m{message_index}.blocks"));
        match message {
            Message::User(message) => {
                for (block, value) in message.blocks.iter().enumerate() {
                    debts.insert(format!("m{message_index}.b{block}.variant"));
                    match value {
                        UserBlock::Text(_) => {
                            debts.insert(format!("m{message_index}.b{block}.text"));
                        }
                        UserBlock::Image(_) => {
                            debts.insert(format!("m{message_index}.b{block}.bytes"));
                            debts.insert(format!("m{message_index}.b{block}.media"));
                        }
                    }
                }
            }
            Message::Assistant(message) => {
                for (block, value) in message.blocks.iter().enumerate() {
                    debts.insert(format!("m{message_index}.b{block}.variant"));
                    match value {
                        AssistantBlock::Text(_) => {
                            debts.insert(format!("m{message_index}.b{block}.text"));
                        }
                        AssistantBlock::Reasoning(_) => {
                            debts.insert(format!("m{message_index}.b{block}.text"));
                            debts.insert(format!("m{message_index}.b{block}.reasoning-kind"));
                            debts.insert(format!("m{message_index}.b{block}.proof"));
                        }
                        AssistantBlock::ToolCall(_) => {
                            for field in ["call-id", "call-name", "arguments"] {
                                debts.insert(format!("m{message_index}.b{block}.{field}"));
                            }
                        }
                    }
                }
            }
            Message::ToolResult(message) => {
                debts.insert(format!("m{message_index}.call-id"));
                if status_projection(wire) != StatusProjection::None {
                    debts.insert(format!("m{message_index}.status"));
                }
                if requires_result_name(wire) {
                    debts.insert(format!("m{message_index}.result-name"));
                }
                for (block, value) in message.blocks.iter().enumerate() {
                    debts.insert(format!("m{message_index}.b{block}.variant"));
                    match value {
                        ToolResultBlock::Text(_) => {
                            debts.insert(format!("m{message_index}.b{block}.text"));
                        }
                        ToolResultBlock::Image(_) => {
                            debts.insert(format!("m{message_index}.b{block}.bytes"));
                            debts.insert(format!("m{message_index}.b{block}.media"));
                        }
                    }
                }
            }
        }
    }
    if matches!(original.tool_choice, ToolChoice::Specific(_)) {
        debts.insert("tool-choice.name".to_owned());
    }
    if matches!(original.reasoning, Reasoning::Enabled(_)) {
        debts.insert("reasoning.effort".to_owned());
        debts.insert("reasoning.budget".to_owned());
    }
    debts
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::provider_validation) enum StatusProjection {
    Boolean,
    None,
    Scalar,
    Switch,
    Composite,
}

pub(in crate::provider_validation) const fn status_projection(
    wire: AdapterWireId,
) -> StatusProjection {
    match wire {
        AdapterWireId::AnthropicMessages | AdapterWireId::PiMessages => StatusProjection::Boolean,
        AdapterWireId::BedrockConverseStream => StatusProjection::Scalar,
        AdapterWireId::GoogleGenerativeAi | AdapterWireId::GoogleVertex => StatusProjection::Switch,
        AdapterWireId::MistralConversations => StatusProjection::Composite,
        AdapterWireId::OpenAiCompletions
        | AdapterWireId::OpenAiResponses
        | AdapterWireId::OpenAiCodexResponses
        | AdapterWireId::AzureOpenAiResponses => StatusProjection::None,
    }
}

pub(in crate::provider_validation) const fn requires_result_name(wire: AdapterWireId) -> bool {
    matches!(
        wire,
        AdapterWireId::PiMessages
            | AdapterWireId::GoogleGenerativeAi
            | AdapterWireId::GoogleVertex
            | AdapterWireId::MistralConversations
    )
}
