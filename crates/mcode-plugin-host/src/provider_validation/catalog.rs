//! Catalog DTO validation and canonical catalog hashing.
use std::cmp::Ordering;

use mcode_config::Sha256Digest;
use sha2::{Digest, Sha256};

use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AuthInteractionRequest, AuthInteractionResponse, CapabilitySupport, CatalogEntry,
    CatalogMetadataEntry, CatalogPage, CatalogRequest, CatalogRevision, CatalogSourceView,
    DescriptorRequest, InputModality, ModelSelection, ProviderDescriptor, ReasoningCapability,
    ToolCapability,
};

use super::charge::checked_u32_len;
use super::scalar;
use super::{ValidationError, ValidationResult};

const MAX_CATALOG_ENTRIES: usize = 4_096;
const MAX_CATALOG_PAGE_ENTRIES: usize = 256;
const CATALOG_DOMAIN: &[u8] = b"mcode-provider-catalog-v1\0";

pub(super) fn validate_auth_interaction(
    request: &AuthInteractionRequest,
    response: &AuthInteractionResponse,
    expected_provider: &str,
    expected_route: &str,
) -> ValidationResult {
    scalar::provider_id(&request.provider_id)?;
    scalar::route_id(&request.route_id)?;
    if request.provider_id != expected_provider || request.route_id != expected_route {
        return Err(ValidationError::InvalidArgument);
    }
    let AuthInteractionResponse::Instructions(instructions) = response else {
        return Ok(());
    };
    scalar::label(&instructions.title, 256)?;
    if instructions.steps.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    if instructions.steps.len() > 32 {
        return Err(ValidationError::Limit);
    }
    for step in &instructions.steps {
        scalar::safe(step, 4_096, true)?;
    }
    Ok(())
}

pub(super) fn validate_catalog_source(source: &CatalogSourceView) -> ValidationResult {
    match source {
        CatalogSourceView::Embedded => Ok(()),
        CatalogSourceView::Verified(view) => {
            validate_revision(&view.revision)?;
            if view.entries.len() > MAX_CATALOG_ENTRIES {
                return Err(ValidationError::Limit);
            }
            validate_metadata_entries(&view.entries)
        }
    }
}

pub(super) fn validate_descriptor(
    request: &DescriptorRequest,
    descriptor: &ProviderDescriptor,
) -> ValidationResult {
    scalar::provider_id(&request.provider_id)?;
    scalar::route_id(&request.route_id)?;
    validate_catalog_source(&request.catalog_source)?;
    scalar::provider_id(&descriptor.provider_id)?;
    scalar::route_id(&descriptor.route_id)?;
    scalar::digest(&descriptor.catalog_digest)?;
    if descriptor.model_count as usize > MAX_CATALOG_ENTRIES {
        return Err(ValidationError::Limit);
    }
    if request.provider_id != descriptor.provider_id || request.route_id != descriptor.route_id {
        return Err(ValidationError::InvalidArgument);
    }
    validate_source_revision(&request.catalog_source, descriptor.source_revision.as_ref())
}

pub(super) fn validate_catalog_request(
    descriptor_request: &DescriptorRequest,
    descriptor: &ProviderDescriptor,
    request: &CatalogRequest,
) -> ValidationResult {
    validate_descriptor(descriptor_request, descriptor)?;
    scalar::provider_id(&request.provider_id)?;
    scalar::route_id(&request.route_id)?;
    scalar::digest(&request.catalog_digest)?;
    validate_catalog_source(&request.catalog_source)?;
    if request.limit == 0 || request.limit as usize > MAX_CATALOG_PAGE_ENTRIES {
        return Err(ValidationError::Limit);
    }
    if request.provider_id != descriptor.provider_id
        || request.route_id != descriptor.route_id
        || request.catalog_digest != descriptor.catalog_digest
        || request.offset > descriptor.model_count
        || !same_source(&descriptor_request.catalog_source, &request.catalog_source)
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

pub(super) fn validate_catalog_page(
    descriptor_request: &DescriptorRequest,
    descriptor: &ProviderDescriptor,
    request: &CatalogRequest,
    page: &CatalogPage,
) -> ValidationResult {
    validate_catalog_request(descriptor_request, descriptor, request)?;
    scalar::provider_id(&page.provider_id)?;
    scalar::route_id(&page.route_id)?;
    scalar::digest(&page.catalog_digest)?;
    validate_source_revision(&request.catalog_source, page.source_revision.as_ref())?;
    if page.provider_id != request.provider_id
        || page.route_id != request.route_id
        || page.catalog_digest != request.catalog_digest
        || page.declared_count != descriptor.model_count
        || page.offset != request.offset
    {
        return Err(ValidationError::InvalidArgument);
    }
    if page.entries.len() > request.limit as usize {
        return Err(ValidationError::Limit);
    }
    validate_catalog_entries(&page.entries)?;

    let entry_count = u32::try_from(page.entries.len()).map_err(|_| ValidationError::Limit)?;
    let computed = page
        .offset
        .checked_add(entry_count)
        .ok_or(ValidationError::Limit)?;
    if computed > page.declared_count {
        return Err(ValidationError::InvalidArgument);
    }
    let expected_next = (computed < page.declared_count).then_some(computed);
    if expected_next.is_some() && page.entries.is_empty() {
        return Err(ValidationError::InvalidArgument);
    }
    if page.next_offset != expected_next {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

pub(super) fn validate_catalog_entries(entries: &[CatalogEntry]) -> ValidationResult {
    if entries.len() > MAX_CATALOG_ENTRIES {
        return Err(ValidationError::Limit);
    }
    let mut previous: Option<(u8, &[u8])> = None;
    let mut payloads = std::collections::BTreeSet::new();
    for entry in entries {
        validate_catalog_entry(entry)?;
        let key = selection_key(&entry.selection);
        if previous.is_some_and(|old| compare_key(old, key) != Ordering::Less) {
            return Err(ValidationError::InvalidArgument);
        }
        if !payloads.insert(key.1) {
            return Err(ValidationError::InvalidArgument);
        }
        previous = Some(key);
    }
    Ok(())
}

pub(super) fn catalog_digest(
    provider_id: &str,
    route_id: &str,
    entries: &[CatalogEntry],
) -> ValidationResult<Sha256Digest> {
    scalar::provider_id(provider_id)?;
    scalar::route_id(route_id)?;
    validate_catalog_entries(entries)?;
    let count = checked_u32_len(entries.len())?;

    let mut hash = Sha256::new();
    hash.update(CATALOG_DOMAIN);
    hash_string(&mut hash, provider_id)?;
    hash_string(&mut hash, route_id)?;
    hash.update(count.to_be_bytes());
    for entry in entries {
        hash_catalog_entry(&mut hash, entry)?;
    }
    let digest = hash.finalize();
    let text = digest_text(&digest);
    Sha256Digest::parse(text).map_err(|_| ValidationError::InvalidArgument)
}

pub(super) fn same_selection(left: &ModelSelection, right: &ModelSelection) -> bool {
    match (left, right) {
        (ModelSelection::Exact(left), ModelSelection::Exact(right))
        | (ModelSelection::Alias(left), ModelSelection::Alias(right)) => left == right,
        _ => false,
    }
}

pub(super) fn is_supported(value: &CapabilitySupport) -> bool {
    matches!(value, CapabilitySupport::Supported)
}

pub(super) fn same_tool_capability(left: &ToolCapability, right: &ToolCapability) -> bool {
    capability_ordinal(&left.tools) == capability_ordinal(&right.tools)
        && capability_ordinal(&left.auto_choice) == capability_ordinal(&right.auto_choice)
        && capability_ordinal(&left.none_choice) == capability_ordinal(&right.none_choice)
        && capability_ordinal(&left.specific_choice) == capability_ordinal(&right.specific_choice)
}

pub(super) fn same_reasoning_capability(
    left: &ReasoningCapability,
    right: &ReasoningCapability,
) -> bool {
    capability_ordinal(&left.reasoning) == capability_ordinal(&right.reasoning)
        && capability_ordinal(&left.effort) == capability_ordinal(&right.effort)
        && capability_ordinal(&left.budget) == capability_ordinal(&right.budget)
        && capability_ordinal(&left.proof) == capability_ordinal(&right.proof)
}

pub(super) fn same_modalities(left: &[InputModality], right: &[InputModality]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| modality_ordinal(left) == modality_ordinal(right))
}

fn validate_catalog_entry(entry: &CatalogEntry) -> ValidationResult {
    validate_selection(&entry.selection)?;
    scalar::model_id(&entry.current_model)?;
    if let Some(name) = &entry.display_name {
        scalar::label(name, 256)?;
    }
    validate_modalities(&entry.input_modalities)?;
    validate_positive(entry.context_tokens)?;
    validate_positive(entry.max_output_tokens)?;
    scalar::operation_id(&entry.completion_operation)?;
    if let ModelSelection::Exact(model) = &entry.selection
        && model != &entry.current_model
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

fn validate_metadata_entries(entries: &[CatalogMetadataEntry]) -> ValidationResult {
    let mut previous: Option<(u8, &[u8])> = None;
    let mut payloads = std::collections::BTreeSet::new();
    for entry in entries {
        validate_selection(&entry.selection)?;
        if let Some(name) = &entry.display_name {
            scalar::label(name, 256)?;
        }
        validate_modalities(&entry.input_modalities)?;
        validate_positive(entry.context_tokens)?;
        validate_positive(entry.max_output_tokens)?;
        let key = selection_key(&entry.selection);
        if previous.is_some_and(|old| compare_key(old, key) != Ordering::Less)
            || !payloads.insert(key.1)
        {
            return Err(ValidationError::InvalidArgument);
        }
        previous = Some(key);
    }
    Ok(())
}

fn validate_selection(selection: &ModelSelection) -> ValidationResult {
    match selection {
        ModelSelection::Exact(value) => scalar::model_id(value),
        ModelSelection::Alias(value) => scalar::model_alias(value),
    }
}

fn validate_modalities(modalities: &[InputModality]) -> ValidationResult {
    if modalities.is_empty() || modalities.len() > 3 {
        return Err(ValidationError::Limit);
    }
    if modalities.len() > 1
        && modalities
            .iter()
            .any(|item| matches!(item, InputModality::Unknown))
    {
        return Err(ValidationError::InvalidArgument);
    }
    if modalities
        .windows(2)
        .any(|pair| modality_ordinal(&pair[0]) >= modality_ordinal(&pair[1]))
    {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

fn validate_positive(value: Option<u64>) -> ValidationResult {
    if value == Some(0) {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

fn validate_revision(revision: &CatalogRevision) -> ValidationResult {
    if revision.last_modified == 0 || revision.last_modified > i64::MAX as u64 {
        return Err(ValidationError::InvalidArgument);
    }
    scalar::digest(&revision.canonical_content_digest)
}

fn validate_source_revision(
    source: &CatalogSourceView,
    revision: Option<&CatalogRevision>,
) -> ValidationResult {
    match (source, revision) {
        (CatalogSourceView::Embedded, None) => Ok(()),
        (CatalogSourceView::Verified(view), Some(actual)) => {
            validate_revision(actual)?;
            if same_revision(&view.revision, actual) {
                Ok(())
            } else {
                Err(ValidationError::InvalidArgument)
            }
        }
        _ => Err(ValidationError::InvalidArgument),
    }
}

fn same_source(left: &CatalogSourceView, right: &CatalogSourceView) -> bool {
    match (left, right) {
        (CatalogSourceView::Embedded, CatalogSourceView::Embedded) => true,
        (CatalogSourceView::Verified(left), CatalogSourceView::Verified(right)) => {
            same_revision(&left.revision, &right.revision)
                && same_metadata_entries(&left.entries, &right.entries)
        }
        _ => false,
    }
}

fn same_revision(left: &CatalogRevision, right: &CatalogRevision) -> bool {
    left.last_modified == right.last_modified
        && left.canonical_content_digest == right.canonical_content_digest
}

fn same_metadata_entries(left: &[CatalogMetadataEntry], right: &[CatalogMetadataEntry]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            same_selection(&left.selection, &right.selection)
                && left.display_name == right.display_name
                && same_modalities(&left.input_modalities, &right.input_modalities)
                && same_tool_capability(&left.tool_capability, &right.tool_capability)
                && same_reasoning_capability(
                    &left.reasoning_capability,
                    &right.reasoning_capability,
                )
                && left.context_tokens == right.context_tokens
                && left.max_output_tokens == right.max_output_tokens
        })
}

fn selection_key(selection: &ModelSelection) -> (u8, &[u8]) {
    match selection {
        ModelSelection::Exact(value) => (0, value.as_bytes()),
        ModelSelection::Alias(value) => (1, value.as_bytes()),
    }
}

fn compare_key(left: (u8, &[u8]), right: (u8, &[u8])) -> Ordering {
    left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1))
}

fn hash_catalog_entry(hash: &mut Sha256, entry: &CatalogEntry) -> ValidationResult {
    let (tag, payload) = selection_key(&entry.selection);
    hash.update([tag]);
    hash_bytes(hash, payload)?;
    hash_string(hash, &entry.current_model)?;
    hash_option_string(hash, entry.display_name.as_deref())?;
    hash.update(checked_u32_len(entry.input_modalities.len())?.to_be_bytes());
    for modality in &entry.input_modalities {
        hash.update([modality_ordinal(modality)]);
    }
    hash_tool_capability(hash, &entry.tool_capability);
    hash_reasoning_capability(hash, &entry.reasoning_capability);
    hash_option_u64(hash, entry.context_tokens);
    hash_option_u64(hash, entry.max_output_tokens);
    hash_string(hash, &entry.completion_operation)
}

fn hash_tool_capability(hash: &mut Sha256, value: &ToolCapability) {
    hash.update([
        capability_ordinal(&value.tools),
        capability_ordinal(&value.auto_choice),
        capability_ordinal(&value.none_choice),
        capability_ordinal(&value.specific_choice),
    ]);
}

fn hash_reasoning_capability(hash: &mut Sha256, value: &ReasoningCapability) {
    hash.update([
        capability_ordinal(&value.reasoning),
        capability_ordinal(&value.effort),
        capability_ordinal(&value.budget),
        capability_ordinal(&value.proof),
    ]);
}

fn hash_string(hash: &mut Sha256, value: &str) -> ValidationResult {
    hash_bytes(hash, value.as_bytes())
}

fn hash_bytes(hash: &mut Sha256, value: &[u8]) -> ValidationResult {
    hash.update(checked_u32_len(value.len())?.to_be_bytes());
    hash.update(value);
    Ok(())
}

fn hash_option_string(hash: &mut Sha256, value: Option<&str>) -> ValidationResult {
    match value {
        None => hash.update([0]),
        Some(value) => {
            hash.update([1]);
            hash_string(hash, value)?;
        }
    }
    Ok(())
}

fn hash_option_u64(hash: &mut Sha256, value: Option<u64>) {
    match value {
        None => hash.update([0]),
        Some(value) => {
            hash.update([1]);
            hash.update(value.to_be_bytes());
        }
    }
}

fn capability_ordinal(value: &CapabilitySupport) -> u8 {
    match value {
        CapabilitySupport::Unknown => 0,
        CapabilitySupport::Unsupported => 1,
        CapabilitySupport::Supported => 2,
    }
}

fn modality_ordinal(value: &InputModality) -> u8 {
    match value {
        InputModality::Unknown => 0,
        InputModality::Text => 1,
        InputModality::Image => 2,
    }
}

fn digest_text(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(71);
    result.push_str("sha256:");
    for byte in bytes {
        result.push(char::from(HEX[(byte >> 4) as usize]));
        result.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    result
}
