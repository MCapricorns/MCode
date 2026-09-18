//! Prepare-input, message-reducer, image/proof, and header validation.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, CacheRetention, CatalogEntry, ImageView, InputModality, Message,
    OrdinaryHeader, PrepareInput, PreparedRequest, Reasoning, ReasoningBlock, ToolChoice,
    ToolDefinition, ToolResultBlock, UserBlock,
};

use super::catalog::{is_supported, same_selection, validate_catalog_entries};
use super::charge::{LogicalCharge, checked_len};
use super::scalar::{self, MAX_LOGICAL_CHARGE, MAX_SAFE_TEXT_BYTES};
use super::wire_json::validate_wire_json;
use super::{ValidationError, ValidationResult};

const MAX_SYSTEM_PARTS: usize = 1_024;
const MAX_MESSAGES: usize = 4_096;
const MAX_TOOLS: usize = 1_024;
const MAX_BLOCKS: usize = 4_096;
const MAX_IMAGE_BYTES: usize = 8 * 1_024 * 1_024;
const MAX_PROOF_BYTES: usize = 64 * 1_024;
const MAX_PROOFS_BYTES: u64 = 256 * 1_024;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_FRAMES: u32 = 64;
const MAX_HEADERS: usize = 32;
const MAX_HEADER_NAME_BYTES: usize = 64;
const MAX_HEADER_VALUE_BYTES: usize = 4_096;

const PERMANENT_DENY_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "cf-aig-authorization",
    "host",
    "content-length",
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "expect",
    "user-agent",
    "origin",
    "referer",
    "forwarded",
    "via",
    "accept-encoding",
    "content-encoding",
    "x-http-method-override",
    "x-method-override",
    "x-original-url",
    "x-rewrite-url",
];

#[derive(Debug, Clone, Copy)]
pub(super) struct SelectedCatalogView<'a> {
    pub(super) provider_id: &'a str,
    pub(super) route_id: &'a str,
    pub(super) catalog_digest: &'a str,
    pub(super) entry: &'a CatalogEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolResultStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MatchedToolResult<'a> {
    pub(super) call_id: &'a str,
    pub(super) name: &'a str,
    pub(super) status: ToolResultStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ValidatedPrepare<'a> {
    pub(super) logical_charge: u64,
    pub(super) matched_tool_results: BTreeMap<&'a str, MatchedToolResult<'a>>,
}

pub(super) fn validate_prepare_input<'a>(
    input: &'a PrepareInput,
    selected: SelectedCatalogView<'_>,
) -> ValidationResult<ValidatedPrepare<'a>> {
    validate_selected_binding(input, selected)?;
    validate_catalog_entries(std::slice::from_ref(selected.entry))?;

    let mut charge = LogicalCharge::new(MAX_LOGICAL_CHARGE);
    charge.string(&input.provider_id)?;
    charge.string(&input.route_id)?;
    charge.string(&input.catalog_digest)?;
    charge_selection(&mut charge, &input.selection)?;
    charge.string(&input.current_model)?;
    charge.string(&input.operation_id)?;
    charge.string(&input.request_id)?;
    charge.string(&input.turn_id)?;

    scalar::provider_id(&input.provider_id)?;
    scalar::route_id(&input.route_id)?;
    scalar::digest(&input.catalog_digest)?;
    scalar::model_id(&input.current_model)?;
    scalar::operation_id(&input.operation_id)?;
    scalar::request_id(&input.request_id)?;
    scalar::turn_id(&input.turn_id)?;

    validate_list_max(input.system.len(), MAX_SYSTEM_PARTS)?;
    charge.add(4)?;
    for part in &input.system {
        scalar::safe(part, MAX_SAFE_TEXT_BYTES, false)?;
        charge.string(part)?;
    }

    validate_list_max(input.tools.len(), MAX_TOOLS)?;
    charge.add(4)?;
    let definitions = validate_tools(&input.tools, &mut charge)?;

    validate_list_max(input.messages.len(), MAX_MESSAGES)?;
    charge.add(4)?;
    let message_facts =
        validate_messages(&input.messages, &definitions, selected.entry, &mut charge)?;

    validate_tool_choice(input, selected.entry, &mut charge)?;
    validate_reasoning(&input.reasoning, selected.entry, &mut charge)?;
    charge.add(4)?;
    match input.cache_retention {
        CacheRetention::Unset
        | CacheRetention::None
        | CacheRetention::Request
        | CacheRetention::Session => {}
    }
    charge.add(4)?;
    if let Some(maximum) = input.max_output_tokens {
        charge.add(8)?;
        if maximum == 0
            || selected.entry.max_output_tokens.is_none()
            || selected
                .entry
                .max_output_tokens
                .is_some_and(|limit| maximum > limit)
        {
            return Err(ValidationError::InvalidArgument);
        }
    }

    let uses_tools = !input.tools.is_empty() || message_facts.has_calls;
    if uses_tools && !is_supported(&selected.entry.tool_capability.tools) {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(ValidatedPrepare {
        logical_charge: charge.value(),
        matched_tool_results: message_facts.matched_tool_results,
    })
}

pub(super) fn validate_prepared_request(
    request: &PreparedRequest,
    reserved_names: &[&str],
) -> ValidationResult {
    validate_wire_json(&request.body, true)?;
    validate_ordinary_headers(&request.ordinary_headers, reserved_names)
}

pub(super) fn validate_ordinary_headers(
    headers: &[OrdinaryHeader],
    reserved_names: &[&str],
) -> ValidationResult {
    if headers.len() > MAX_HEADERS {
        return Err(ValidationError::Limit);
    }
    let mut previous: Option<(&str, &str)> = None;
    let mut names = BTreeSet::new();
    for header in headers {
        validate_header_name(&header.name)?;
        validate_header_value(&header.value)?;
        if denied_header(&header.name, reserved_names) || !names.insert(header.name.as_str()) {
            return Err(ValidationError::InvalidArgument);
        }
        let key = (header.name.as_str(), header.value.as_str());
        if previous.is_some_and(|old| old >= key) {
            return Err(ValidationError::InvalidArgument);
        }
        previous = Some(key);
    }
    Ok(())
}

fn validate_selected_binding(
    input: &PrepareInput,
    selected: SelectedCatalogView<'_>,
) -> ValidationResult {
    scalar::provider_id(selected.provider_id)?;
    scalar::route_id(selected.route_id)?;
    scalar::digest(selected.catalog_digest)?;
    if input.provider_id != selected.provider_id
        || input.route_id != selected.route_id
        || input.catalog_digest != selected.catalog_digest
        || !same_selection(&input.selection, &selected.entry.selection)
        || input.current_model != selected.entry.current_model
        || input.operation_id != selected.entry.completion_operation
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

fn validate_tools<'a>(
    tools: &'a [ToolDefinition],
    charge: &mut LogicalCharge,
) -> ValidationResult<BTreeSet<&'a str>> {
    let mut definitions = BTreeSet::new();
    for tool in tools {
        scalar::label(&tool.name, 128)?;
        scalar::safe(&tool.description, MAX_SAFE_TEXT_BYTES, false)?;
        let schema = validate_wire_json(&tool.input_schema, true)?;
        if !definitions.insert(tool.name.as_str()) {
            return Err(ValidationError::InvalidArgument);
        }
        charge.string(&tool.name)?;
        charge.string(&tool.description)?;
        charge.add(schema.logical_charge)?;
    }
    Ok(definitions)
}

#[derive(Debug, Clone)]
struct MessageFacts<'a> {
    has_calls: bool,
    matched_tool_results: BTreeMap<&'a str, MatchedToolResult<'a>>,
}

fn validate_messages<'a>(
    messages: &'a [Message],
    definitions: &BTreeSet<&str>,
    selected: &CatalogEntry,
    charge: &mut LogicalCharge,
) -> ValidationResult<MessageFacts<'a>> {
    let mut pending: VecDeque<(&str, &str)> = VecDeque::new();
    let mut call_ids = BTreeSet::new();
    let mut proof_bytes = 0_u64;
    let mut has_calls = false;
    let mut matched_tool_results = BTreeMap::new();

    for message in messages {
        charge.add(4)?;
        match message {
            Message::User(user) => {
                if !pending.is_empty() {
                    return Err(ValidationError::InvalidArgument);
                }
                validate_nonempty_blocks(user.blocks.len())?;
                charge.add(4)?;
                for block in &user.blocks {
                    charge.add(4)?;
                    match block {
                        UserBlock::Text(text) => validate_text(&text.text, charge)?,
                        UserBlock::Image(image) => validate_image(image, selected, charge)?,
                    }
                }
            }
            Message::Assistant(assistant) => {
                if !pending.is_empty() {
                    return Err(ValidationError::InvalidArgument);
                }
                validate_nonempty_blocks(assistant.blocks.len())?;
                charge.add(4)?;
                for block in &assistant.blocks {
                    charge.add(4)?;
                    match block {
                        AssistantBlock::Text(text) => validate_text(&text.text, charge)?,
                        AssistantBlock::Reasoning(reasoning) => {
                            validate_reasoning_block(reasoning, selected, charge, &mut proof_bytes)?
                        }
                        AssistantBlock::ToolCall(call) => {
                            scalar::tracking_id(&call.call_id)?;
                            scalar::label(&call.name, 128)?;
                            if !call_ids.insert(call.call_id.as_str())
                                || !definitions.contains(call.name.as_str())
                            {
                                return Err(ValidationError::InvalidArgument);
                            }
                            let statistics = validate_wire_json(&call.arguments, true)?;
                            charge.string(&call.call_id)?;
                            charge.string(&call.name)?;
                            charge.add(statistics.logical_charge)?;
                            pending.push_back((&call.call_id, &call.name));
                            has_calls = true;
                        }
                    }
                }
            }
            Message::ToolResult(result) => {
                let Some((call_id, name)) = pending.pop_front() else {
                    return Err(ValidationError::InvalidArgument);
                };
                if result.call_id != call_id {
                    return Err(ValidationError::InvalidArgument);
                }
                scalar::tracking_id(&result.call_id)?;
                validate_nonempty_blocks(result.blocks.len())?;
                charge.string(&result.call_id)?;
                charge.add(4)?;
                for block in &result.blocks {
                    charge.add(4)?;
                    match block {
                        ToolResultBlock::Text(text) => validate_text(&text.text, charge)?,
                        ToolResultBlock::Image(image) => validate_image(image, selected, charge)?,
                    }
                }
                charge.add(1)?;
                let matched = MatchedToolResult {
                    call_id,
                    name,
                    status: if result.is_error {
                        ToolResultStatus::Error
                    } else {
                        ToolResultStatus::Success
                    },
                };
                if matched_tool_results.insert(call_id, matched).is_some() {
                    return Err(ValidationError::InvalidArgument);
                }
            }
        }
    }
    if !pending.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(MessageFacts {
        has_calls,
        matched_tool_results,
    })
}

fn validate_text(text: &str, charge: &mut LogicalCharge) -> ValidationResult {
    scalar::safe(text, MAX_SAFE_TEXT_BYTES, true)?;
    charge.string(text)
}

// Stamps remain untrusted comparison values; sidecar authority is deferred beyond this local slice.
fn validate_reasoning_block(
    block: &ReasoningBlock,
    selected: &CatalogEntry,
    charge: &mut LogicalCharge,
    proof_bytes: &mut u64,
) -> ValidationResult {
    if !is_supported(&selected.reasoning_capability.reasoning) {
        return Err(ValidationError::InvalidArgument);
    }
    charge.add(4)?;
    validate_text(&block.text, charge)?;
    charge.add(4)?;
    let Some(proof) = &block.proof else {
        return Ok(());
    };
    if !same_reasoning_kind(&block.kind, &proof.reasoning_kind)
        || !is_supported(&selected.reasoning_capability.proof)
    {
        return Err(ValidationError::InvalidArgument);
    }
    scalar::stamp(&proof.stamp, "prf1-")?;
    scalar::request_id(&proof.source_request_id)?;
    scalar::turn_id(&proof.source_turn_id)?;
    if proof.source_content_index > 63 {
        return Err(ValidationError::InvalidArgument);
    }
    if proof.proof.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    if proof.proof.len() > MAX_PROOF_BYTES {
        return Err(ValidationError::Limit);
    }
    *proof_bytes = proof_bytes
        .checked_add(checked_len(proof.proof.len())?)
        .ok_or(ValidationError::Limit)?;
    if *proof_bytes > MAX_PROOFS_BYTES {
        return Err(ValidationError::Limit);
    }
    charge.string(&proof.stamp)?;
    charge.string(&proof.source_request_id)?;
    charge.string(&proof.source_turn_id)?;
    charge.add(1)?;
    charge.add(4)?;
    charge.add(4)?;
    charge.add(checked_len(proof.proof.len())?)
}

// Image bytes and metadata receive no sidecar authority in this local validator.
fn validate_image(
    image: &ImageView,
    selected: &CatalogEntry,
    charge: &mut LogicalCharge,
) -> ValidationResult {
    scalar::stamp(&image.stamp, "img1-")?;
    if image.bytes.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    if image.bytes.len() > MAX_IMAGE_BYTES {
        return Err(ValidationError::Limit);
    }
    if !(1..=MAX_IMAGE_DIMENSION).contains(&image.metadata.width)
        || !(1..=MAX_IMAGE_DIMENSION).contains(&image.metadata.height)
        || !(1..=MAX_IMAGE_FRAMES).contains(&image.metadata.frames)
        || !selected
            .input_modalities
            .iter()
            .any(|modality| matches!(modality, InputModality::Image))
    {
        return Err(ValidationError::InvalidArgument);
    }
    charge.string(&image.stamp)?;
    charge.add(4)?;
    charge.add(4)?;
    charge.add(checked_len(image.bytes.len())?)?;
    charge.add(12)
}

fn validate_tool_choice(
    input: &PrepareInput,
    selected: &CatalogEntry,
    charge: &mut LogicalCharge,
) -> ValidationResult {
    charge.add(4)?;
    match &input.tool_choice {
        ToolChoice::Unset => {}
        ToolChoice::Auto => {
            if input.tools.is_empty()
                || !is_supported(&selected.tool_capability.tools)
                || !is_supported(&selected.tool_capability.auto_choice)
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
        ToolChoice::None => {
            if !is_supported(&selected.tool_capability.none_choice) {
                return Err(ValidationError::InvalidArgument);
            }
        }
        ToolChoice::Specific(choice) => {
            charge.string(&choice.name)?;
            scalar::label(&choice.name, 128)?;
            if input.tools.len() != 1
                || input.tools[0].name != choice.name
                || !is_supported(&selected.tool_capability.tools)
                || !is_supported(&selected.tool_capability.specific_choice)
            {
                return Err(ValidationError::InvalidArgument);
            }
        }
    }
    Ok(())
}

fn validate_reasoning(
    reasoning: &Reasoning,
    selected: &CatalogEntry,
    charge: &mut LogicalCharge,
) -> ValidationResult {
    charge.add(4)?;
    match reasoning {
        Reasoning::Unset => Ok(()),
        Reasoning::Disabled => {
            if is_supported(&selected.reasoning_capability.reasoning) {
                Ok(())
            } else {
                Err(ValidationError::InvalidArgument)
            }
        }
        Reasoning::Enabled(enabled) => {
            if !is_supported(&selected.reasoning_capability.reasoning) {
                return Err(ValidationError::InvalidArgument);
            }
            charge.add(4)?;
            if enabled.effort.is_some() {
                charge.add(4)?;
                if !is_supported(&selected.reasoning_capability.effort) {
                    return Err(ValidationError::InvalidArgument);
                }
            }
            charge.add(4)?;
            if let Some(budget) = enabled.budget_tokens {
                charge.add(8)?;
                if budget == 0 || !is_supported(&selected.reasoning_capability.budget) {
                    return Err(ValidationError::InvalidArgument);
                }
            }
            Ok(())
        }
    }
}

fn validate_nonempty_blocks(length: usize) -> ValidationResult {
    if length == 0 {
        return Err(ValidationError::InvalidArgument);
    }
    validate_list_max(length, MAX_BLOCKS)
}

fn validate_list_max(length: usize, maximum: usize) -> ValidationResult {
    if length > maximum {
        return Err(ValidationError::Limit);
    }
    Ok(())
}

fn charge_selection(
    charge: &mut LogicalCharge,
    selection: &crate::provider_wit::exports::mcode::provider_pack::provider_api::ModelSelection,
) -> ValidationResult {
    charge.add(4)?;
    match selection {
        crate::provider_wit::exports::mcode::provider_pack::provider_api::ModelSelection::Exact(
            value,
        ) => scalar::model_id(value)?,
        crate::provider_wit::exports::mcode::provider_pack::provider_api::ModelSelection::Alias(
            value,
        ) => scalar::model_alias(value)?,
    }
    let value = match selection {
        crate::provider_wit::exports::mcode::provider_pack::provider_api::ModelSelection::Exact(
            value,
        )
        | crate::provider_wit::exports::mcode::provider_pack::provider_api::ModelSelection::Alias(
            value,
        ) => value,
    };
    charge.string(value)
}

fn same_reasoning_kind(
    left: &crate::provider_wit::exports::mcode::provider_pack::provider_api::ReasoningKind,
    right: &crate::provider_wit::exports::mcode::provider_pack::provider_api::ReasoningKind,
) -> bool {
    use crate::provider_wit::exports::mcode::provider_pack::provider_api::ReasoningKind;
    matches!(
        (left, right),
        (ReasoningKind::Thinking, ReasoningKind::Thinking)
            | (ReasoningKind::Summary, ReasoningKind::Summary)
    )
}

pub(super) fn validate_header_name(name: &str) -> ValidationResult {
    let bytes = name.as_bytes();
    if bytes.len() > MAX_HEADER_NAME_BYTES {
        return Err(ValidationError::Limit);
    }
    if bytes.is_empty()
        || bytes.iter().any(|byte| {
            !byte.is_ascii_lowercase()
                && !byte.is_ascii_digit()
                && !matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

pub(super) fn validate_header_value(value: &str) -> ValidationResult {
    let bytes = value.as_bytes();
    if bytes.len() > MAX_HEADER_VALUE_BYTES {
        return Err(ValidationError::Limit);
    }
    if bytes
        .first()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        || bytes
            .last()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        || bytes
            .iter()
            .any(|byte| !matches!(byte, b'\t' | b' '..=b'~'))
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

pub(super) fn denied_header(name: &str, reserved_names: &[&str]) -> bool {
    PERMANENT_DENY_HEADERS.contains(&name)
        || name.starts_with("x-forwarded-")
        || name.starts_with("x-amz-")
        || reserved_names.contains(&name)
}
