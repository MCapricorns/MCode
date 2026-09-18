//! Generic interpretation of a validated adapter contract.
use std::collections::BTreeSet;

use crate::provider_validation::prepare::{
    SelectedCatalogView, ValidatedPrepare, validate_prepare_input,
};
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, CacheRetention, Message, ModelSelection, PrepareInput, Reasoning, ToolChoice,
    ToolResultBlock, UserBlock,
};

use super::json::AdapterJson;
pub(in crate::provider_validation) use super::policy::{
    StatusProjection, requires_result_name, status_projection,
};
use super::policy::{catalog_entry, expected_debts, validate_capabilities, validate_identity};
use super::types::{
    AdapterCollection, AdapterContractV1, AdapterEnumSource, AdapterModelSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterValidationError,
    AdapterValidationResult, AdapterVariantSource, AdapterWireId, ContractNodeBody,
    ValidatedCatalogEntryView,
};

pub(super) fn evaluate_contract(
    contract: &AdapterContractV1,
    selected: ValidatedCatalogEntryView<'_>,
    original: &PrepareInput,
) -> AdapterValidationResult<AdapterJson> {
    validate_identity(selected, original)?;
    validate_capabilities(selected, original)?;
    let entry = catalog_entry(selected);
    let prepared = validate_prepare_input(
        original,
        SelectedCatalogView {
            provider_id: selected.provider_id,
            route_id: selected.route_id,
            catalog_digest: selected.catalog_digest,
            entry: &entry,
        },
    )
    .map_err(map_source_error)?;

    let expected = expected_debts(contract.wire_id, original);
    let mut evaluator = Evaluator {
        contract,
        original,
        prepared,
        consumed: BTreeSet::new(),
        optional_consumed: BTreeSet::new(),
    };
    let value = evaluator
        .eval_node(contract.tree.root as usize, Scope::Root)?
        .ok_or(AdapterValidationError::InvalidContract)?;
    if evaluator.consumed != expected {
        return Err(AdapterValidationError::SourceMismatch);
    }
    Ok(value)
}

pub(super) struct Evaluator<'a> {
    pub(super) contract: &'a AdapterContractV1,
    pub(super) original: &'a PrepareInput,
    pub(super) prepared: ValidatedPrepare<'a>,
    pub(super) consumed: BTreeSet<String>,
    pub(super) optional_consumed: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Scope<'a> {
    Root,
    System(usize, &'a str),
    SystemEntry(usize, &'a str),
    Message(usize, &'a Message),
    MessageEntry(usize, &'a Message),
    UserBlock(usize, usize, &'a UserBlock),
    AssistantBlock(usize, usize, &'a AssistantBlock),
    ToolResultBlock(usize, usize, &'a ToolResultBlock),
    Tool(usize),
}

impl<'a> Evaluator<'a> {
    fn eval_node(
        &mut self,
        index: usize,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<Option<AdapterJson>> {
        let contract = self.contract;
        let node = &contract.tree.nodes[index];
        if self.omit(node.presence, node.presence_source, scope)? {
            return Ok(None);
        }
        let value = match &node.body {
            ContractNodeBody::Object(object) => {
                let mut fields = Vec::new();
                for child_index in &object.children {
                    let child = &self.contract.tree.nodes[*child_index as usize];
                    let Some(super::types::PathSegment::Key(key)) = child.segment.as_ref() else {
                        return Err(AdapterValidationError::InvalidContract);
                    };
                    let key = key.clone();
                    if let Some(value) = self.eval_node(*child_index as usize, scope)? {
                        fields.push((key, value));
                    }
                }
                AdapterJson::Object(fields)
            }
            ContractNodeBody::Array(array) => {
                let scopes = self.collection_scopes(array.collection, scope)?;
                let length =
                    u32::try_from(scopes.len()).map_err(|_| AdapterValidationError::Limit)?;
                if !(array.min..=array.max).contains(&length) {
                    return Err(AdapterValidationError::SourceMismatch);
                }
                let mut items = Vec::with_capacity(scopes.len());
                for item_scope in scopes {
                    let item = self
                        .eval_node(array.item as usize, item_scope)?
                        .ok_or(AdapterValidationError::InvalidContract)?;
                    items.push(item);
                }
                AdapterJson::Array(items)
            }
            ContractNodeBody::Switch(switch) => {
                let ordinal = self.variant_ordinal(switch.source, scope)?;
                let case = switch
                    .cases
                    .get(usize::from(ordinal))
                    .ok_or(AdapterValidationError::InvalidContract)?;
                self.eval_node(case.node as usize, scope)?
                    .ok_or(AdapterValidationError::InvalidContract)?
            }
            ContractNodeBody::Value(value) => self.scalar(value.source, &value.transform, scope)?,
            ContractNodeBody::Constant(constant) => AdapterJson::from_constant(&constant.value),
        };
        Ok(Some(value))
    }

    fn omit(
        &mut self,
        presence: AdapterPresence,
        source: Option<AdapterPresenceSource>,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<bool> {
        match (presence, source) {
            (AdapterPresence::Required, None) => Ok(false),
            (AdapterPresence::OmitIfNone, Some(source)) => {
                let (absent, debt) = self.option_presence(source, scope)?;
                if absent {
                    self.consume(debt)?;
                }
                Ok(absent)
            }
            (AdapterPresence::OmitForUnset, Some(source)) => {
                let (unset, debt) = self.unset_presence(source, scope)?;
                if unset {
                    self.consume(debt)?;
                }
                Ok(unset)
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    fn option_presence(
        &self,
        source: AdapterPresenceSource,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<(bool, String)> {
        match source {
            AdapterPresenceSource::ReasoningProof => {
                let Scope::AssistantBlock(message, block, AssistantBlock::Reasoning(reasoning)) =
                    scope
                else {
                    return Err(AdapterValidationError::InvalidContract);
                };
                Ok((
                    reasoning.proof.is_none(),
                    format!("m{message}.b{block}.proof"),
                ))
            }
            AdapterPresenceSource::ReasoningEffort => {
                let Reasoning::Enabled(enabled) = &self.original.reasoning else {
                    return Err(AdapterValidationError::InvalidContract);
                };
                Ok((enabled.effort.is_none(), "reasoning.effort".to_owned()))
            }
            AdapterPresenceSource::ReasoningBudget => {
                let Reasoning::Enabled(enabled) = &self.original.reasoning else {
                    return Err(AdapterValidationError::InvalidContract);
                };
                Ok((
                    enabled.budget_tokens.is_none(),
                    "reasoning.budget".to_owned(),
                ))
            }
            AdapterPresenceSource::MaxOutput => Ok((
                self.original.max_output_tokens.is_none(),
                "max-output".to_owned(),
            )),
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    fn unset_presence(
        &self,
        source: AdapterPresenceSource,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<(bool, String)> {
        if !matches!(scope, Scope::Root) {
            return Err(AdapterValidationError::InvalidContract);
        }
        match source {
            AdapterPresenceSource::ToolChoice => Ok((
                matches!(self.original.tool_choice, ToolChoice::Unset),
                "tool-choice.variant".to_owned(),
            )),
            AdapterPresenceSource::Reasoning => Ok((
                matches!(self.original.reasoning, Reasoning::Unset),
                "reasoning.variant".to_owned(),
            )),
            AdapterPresenceSource::CacheRetention => Ok((
                matches!(self.original.cache_retention, CacheRetention::Unset),
                "cache.variant".to_owned(),
            )),
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    fn collection_scopes(
        &mut self,
        collection: AdapterCollection,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<Vec<Scope<'a>>> {
        match (collection, scope) {
            (AdapterCollection::System, Scope::Root) => {
                self.consume("system.collection")?;
                Ok(self
                    .original
                    .system
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Scope::System(index, value))
                    .collect())
            }
            (AdapterCollection::Messages, Scope::Root) => {
                self.consume("messages.collection")?;
                Ok(self
                    .original
                    .messages
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Scope::Message(index, value))
                    .collect())
            }
            (AdapterCollection::Tools, Scope::Root) => {
                self.consume("tools.collection")?;
                Ok((0..self.original.tools.len()).map(Scope::Tool).collect())
            }
            (AdapterCollection::Blocks, Scope::Message(message, value))
            | (AdapterCollection::Blocks, Scope::MessageEntry(message, value)) => {
                self.consume(format!("m{message}.blocks"))?;
                Ok(match value {
                    Message::User(value) => value
                        .blocks
                        .iter()
                        .enumerate()
                        .map(|(block, value)| Scope::UserBlock(message, block, value))
                        .collect(),
                    Message::Assistant(value) => value
                        .blocks
                        .iter()
                        .enumerate()
                        .map(|(block, value)| Scope::AssistantBlock(message, block, value))
                        .collect(),
                    Message::ToolResult(value) => value
                        .blocks
                        .iter()
                        .enumerate()
                        .map(|(block, value)| Scope::ToolResultBlock(message, block, value))
                        .collect(),
                })
            }
            (AdapterCollection::SystemMessages, Scope::Root)
                if matches!(
                    self.contract.wire_id,
                    AdapterWireId::OpenAiCompletions | AdapterWireId::MistralConversations
                ) =>
            {
                self.consume("system.collection")?;
                self.consume("messages.collection")?;
                let length = self
                    .original
                    .system
                    .len()
                    .checked_add(self.original.messages.len())
                    .ok_or(AdapterValidationError::Limit)?;
                if length > 5_120 {
                    return Err(AdapterValidationError::Limit);
                }
                Ok(self
                    .original
                    .system
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Scope::SystemEntry(index, value))
                    .chain(
                        self.original
                            .messages
                            .iter()
                            .enumerate()
                            .map(|(index, value)| Scope::MessageEntry(index, value)),
                    )
                    .collect())
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    fn variant_ordinal(
        &mut self,
        source: AdapterVariantSource,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<u8> {
        match (source, scope) {
            (AdapterVariantSource::ModelSelection, Scope::Root) => {
                self.consume("selection.variant")?;
                Ok(selection_ordinal(&self.original.selection))
            }
            (AdapterVariantSource::SystemMessageEntry, Scope::SystemEntry(..)) => Ok(0),
            (AdapterVariantSource::SystemMessageEntry, Scope::MessageEntry(..)) => Ok(1),
            (AdapterVariantSource::Message, Scope::Message(index, message))
            | (AdapterVariantSource::Message, Scope::MessageEntry(index, message)) => {
                self.consume(format!("m{index}.variant"))?;
                Ok(message_ordinal(message))
            }
            (AdapterVariantSource::UserBlock, Scope::UserBlock(message, block, value)) => {
                self.consume(format!("m{message}.b{block}.variant"))?;
                Ok(u8::from(matches!(value, UserBlock::Image(_))))
            }
            (
                AdapterVariantSource::AssistantBlock,
                Scope::AssistantBlock(message, block, value),
            ) => {
                self.consume(format!("m{message}.b{block}.variant"))?;
                Ok(match value {
                    AssistantBlock::Text(_) => 0,
                    AssistantBlock::Reasoning(_) => 1,
                    AssistantBlock::ToolCall(_) => 2,
                })
            }
            (
                AdapterVariantSource::ToolResultBlock,
                Scope::ToolResultBlock(message, block, value),
            ) => {
                self.consume(format!("m{message}.b{block}.variant"))?;
                Ok(u8::from(matches!(value, ToolResultBlock::Image(_))))
            }
            (
                AdapterVariantSource::ToolResultStatus,
                Scope::Message(message, Message::ToolResult(result)),
            )
            | (
                AdapterVariantSource::ToolResultStatus,
                Scope::MessageEntry(message, Message::ToolResult(result)),
            ) => {
                if status_projection(self.contract.wire_id) != StatusProjection::Switch {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume(format!("m{message}.status"))?;
                Ok(u8::from(result.is_error))
            }
            (AdapterVariantSource::ToolChoice, Scope::Root) => {
                self.consume("tool-choice.variant")?;
                Ok(tool_choice_ordinal(&self.original.tool_choice))
            }
            (AdapterVariantSource::Reasoning, Scope::Root) => {
                self.consume("reasoning.variant")?;
                Ok(reasoning_ordinal(&self.original.reasoning))
            }
            (AdapterVariantSource::CacheRetention, Scope::Root) => {
                self.consume("cache.variant")?;
                Ok(cache_ordinal(&self.original.cache_retention))
            }
            _ => Err(AdapterValidationError::InvalidContract),
        }
    }

    fn scalar(
        &mut self,
        source: AdapterScalarSource,
        transform: &AdapterTransform,
        scope: Scope<'a>,
    ) -> AdapterValidationResult<AdapterJson> {
        use AdapterScalarSource as Source;
        match source {
            Source::SelectedModel => {
                require_root(scope)?;
                self.consume("selection.payload")?;
                let value = match self.contract.model_source {
                    AdapterModelSource::RequestedSelection => {
                        selection_payload(&self.original.selection)
                    }
                    AdapterModelSource::CurrentModel => &self.original.current_model,
                };
                identity_string(transform, value)
            }
            Source::SelectionKind => {
                require_root(scope)?;
                self.consume_optional("selection.kind")?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::SelectionKind,
                    selection_ordinal(&self.original.selection),
                )
            }
            Source::SystemItem => {
                let (index, value) = match scope {
                    Scope::System(index, value) | Scope::SystemEntry(index, value) => {
                        (index, value)
                    }
                    _ => return Err(AdapterValidationError::InvalidContract),
                };
                self.consume(format!("system.{index}"))?;
                identity_string(transform, value)
            }
            Source::SystemJoined => {
                require_root(scope)?;
                if !matches!(transform, AdapterTransform::JoinLf) {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume("system.collection")?;
                let mut length = 0_u64;
                for (index, item) in self.original.system.iter().enumerate() {
                    self.consume(format!("system.{index}"))?;
                    length = length
                        .checked_add(
                            u64::try_from(item.len()).map_err(|_| AdapterValidationError::Limit)?,
                        )
                        .ok_or(AdapterValidationError::Limit)?;
                    if index != 0 {
                        length = length.checked_add(1).ok_or(AdapterValidationError::Limit)?;
                    }
                }
                if length > crate::provider_validation::scalar::MAX_LOGICAL_CHARGE {
                    return Err(AdapterValidationError::Limit);
                }
                let capacity =
                    usize::try_from(length).map_err(|_| AdapterValidationError::Limit)?;
                let mut joined = String::with_capacity(capacity);
                for (index, item) in self.original.system.iter().enumerate() {
                    if index != 0 {
                        joined.push('\n');
                    }
                    joined.push_str(item);
                }
                if joined.len() != capacity {
                    return Err(AdapterValidationError::SourceMismatch);
                }
                Ok(AdapterJson::derived_string(joined))
            }
            Source::MessageRole => {
                let (message_index, message) = scope_message(scope)?;
                self.consume_optional(format!("m{message_index}.role"))?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::MessageKind,
                    message_ordinal(message),
                )
            }
            Source::BlockKind => self.block_kind(transform, scope),
            Source::BlockText => self.block_text(transform, scope),
            Source::ToolResultCallId => {
                let (message, result) = tool_result(scope)?;
                self.consume(format!("m{message}.call-id"))?;
                identity_string(transform, &result.call_id)
            }
            Source::ToolResultIsError => {
                let (message, result) = tool_result(scope)?;
                if status_projection(self.contract.wire_id) != StatusProjection::Boolean
                    || !matches!(transform, AdapterTransform::Identity)
                {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume(format!("m{message}.status"))?;
                Ok(AdapterJson::Boolean(result.is_error))
            }
            Source::ToolResultStatus => {
                let (message, result) = tool_result(scope)?;
                if status_projection(self.contract.wire_id) != StatusProjection::Scalar {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume(format!("m{message}.status"))?;
                let value = self.enum_token(
                    transform,
                    AdapterEnumSource::ToolResultStatus,
                    u8::from(result.is_error),
                )?;
                let table = enum_table(self.contract, transform)?;
                if table.entries[0].token != "success" || table.entries[1].token != "error" {
                    return Err(AdapterValidationError::InvalidContract);
                }
                Ok(value)
            }
            Source::ToolResultName => {
                let (message, result) = tool_result(scope)?;
                if !requires_result_name(self.contract.wire_id) {
                    return Err(AdapterValidationError::InvalidContract);
                }
                self.consume(format!("m{message}.result-name"))?;
                let matched = self.matched_result(&result.call_id)?;
                identity_string(transform, matched.name)
            }
            Source::MistralToolResultContent => self.mistral_content(transform, scope),
            Source::ToolCallId | Source::ToolCallName | Source::ToolCallArguments => {
                self.tool_call_scalar(source, transform, scope)
            }
            Source::ToolName | Source::ToolDescription | Source::ToolSchema => {
                self.tool_scalar(source, transform, scope)
            }
            Source::ReasoningKind | Source::Proof => {
                self.reasoning_scalar(source, transform, scope)
            }
            Source::ImageBytes
            | Source::ImageMediaType
            | Source::ImageWidth
            | Source::ImageHeight
            | Source::ImageFrames
            | Source::ImageDataUri => self.image_scalar(source, transform, scope),
            Source::ToolChoiceKind | Source::ToolChoiceName => {
                self.tool_choice_scalar(source, transform, scope)
            }
            Source::ReasoningMode | Source::ReasoningEffort | Source::ReasoningBudget => {
                self.reasoning_control_scalar(source, transform, scope)
            }
            Source::CacheRetention => {
                require_root(scope)?;
                self.consume_optional("cache.kind")?;
                self.enum_token(
                    transform,
                    AdapterEnumSource::CacheRetention,
                    cache_ordinal(&self.original.cache_retention),
                )
            }
            Source::MaxOutput => {
                require_root(scope)?;
                let value = self
                    .original
                    .max_output_tokens
                    .ok_or(AdapterValidationError::InvalidContract)?;
                self.consume("max-output")?;
                checked_u64(transform, value)
            }
        }
    }

    pub(super) fn enum_token(
        &self,
        transform: &AdapterTransform,
        source: AdapterEnumSource,
        ordinal: u8,
    ) -> AdapterValidationResult<AdapterJson> {
        let table = enum_table(self.contract, transform)?;
        if table.source != source {
            return Err(AdapterValidationError::InvalidContract);
        }
        let entry = table
            .entries
            .get(usize::from(ordinal))
            .ok_or(AdapterValidationError::InvalidContract)?;
        Ok(AdapterJson::ordinary_string(&entry.token))
    }

    pub(super) fn consume(&mut self, debt: impl Into<String>) -> AdapterValidationResult<()> {
        if self.consumed.insert(debt.into()) {
            Ok(())
        } else {
            Err(AdapterValidationError::InvalidContract)
        }
    }

    pub(super) fn consume_optional(
        &mut self,
        source: impl Into<String>,
    ) -> AdapterValidationResult<()> {
        if self.optional_consumed.insert(source.into()) {
            Ok(())
        } else {
            Err(AdapterValidationError::InvalidContract)
        }
    }

    pub(super) fn matched_result(
        &self,
        call_id: &str,
    ) -> AdapterValidationResult<&crate::provider_validation::prepare::MatchedToolResult<'_>> {
        self.prepared
            .matched_tool_results
            .get(call_id)
            .ok_or(AdapterValidationError::SourceMismatch)
    }
}

fn selection_ordinal(value: &ModelSelection) -> u8 {
    u8::from(matches!(value, ModelSelection::Alias(_)))
}

fn selection_payload(value: &ModelSelection) -> &str {
    match value {
        ModelSelection::Exact(value) | ModelSelection::Alias(value) => value,
    }
}

fn message_ordinal(value: &Message) -> u8 {
    match value {
        Message::User(_) => 0,
        Message::Assistant(_) => 1,
        Message::ToolResult(_) => 2,
    }
}

fn tool_choice_ordinal(value: &ToolChoice) -> u8 {
    match value {
        ToolChoice::Unset => 0,
        ToolChoice::Auto => 1,
        ToolChoice::None => 2,
        ToolChoice::Specific(_) => 3,
    }
}

fn reasoning_ordinal(value: &Reasoning) -> u8 {
    match value {
        Reasoning::Unset => 0,
        Reasoning::Disabled => 1,
        Reasoning::Enabled(_) => 2,
    }
}

fn cache_ordinal(value: &CacheRetention) -> u8 {
    match value {
        CacheRetention::Unset => 0,
        CacheRetention::None => 1,
        CacheRetention::Request => 2,
        CacheRetention::Session => 3,
    }
}

fn identity_string(
    transform: &AdapterTransform,
    value: &str,
) -> AdapterValidationResult<AdapterJson> {
    if !matches!(transform, AdapterTransform::Identity) {
        return Err(AdapterValidationError::InvalidContract);
    }
    Ok(AdapterJson::ordinary_string(value))
}

fn checked_u64(transform: &AdapterTransform, value: u64) -> AdapterValidationResult<AdapterJson> {
    if !matches!(transform, AdapterTransform::CheckedU64) {
        return Err(AdapterValidationError::InvalidContract);
    }
    Ok(AdapterJson::Number(value.to_string()))
}

fn enum_table<'a>(
    contract: &'a AdapterContractV1,
    transform: &AdapterTransform,
) -> AdapterValidationResult<&'a super::types::EnumTokenTable> {
    let AdapterTransform::EnumToken(index) = transform else {
        return Err(AdapterValidationError::InvalidContract);
    };
    contract
        .tree
        .tables
        .get(usize::from(*index))
        .ok_or(AdapterValidationError::InvalidContract)
}

fn require_root(scope: Scope<'_>) -> AdapterValidationResult<()> {
    matches!(scope, Scope::Root)
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn scope_message<'a>(scope: Scope<'a>) -> AdapterValidationResult<(usize, &'a Message)> {
    match scope {
        Scope::Message(index, value) | Scope::MessageEntry(index, value) => Ok((index, value)),
        _ => Err(AdapterValidationError::InvalidContract),
    }
}

fn tool_result<'a>(
    scope: Scope<'a>,
) -> AdapterValidationResult<(
    usize,
    &'a crate::provider_wit::exports::mcode::provider_pack::provider_api::ToolResultMessage,
)> {
    match scope {
        Scope::Message(index, Message::ToolResult(value))
        | Scope::MessageEntry(index, Message::ToolResult(value)) => Ok((index, value)),
        _ => Err(AdapterValidationError::InvalidContract),
    }
}

fn map_source_error(error: crate::provider_validation::ValidationError) -> AdapterValidationError {
    match error {
        crate::provider_validation::ValidationError::InvalidArgument => {
            AdapterValidationError::SourceMismatch
        }
        crate::provider_validation::ValidationError::Limit => AdapterValidationError::Limit,
    }
}
