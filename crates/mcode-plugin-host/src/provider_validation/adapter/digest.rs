//! Domain-separated adapter contract, body, and ordinary-header digests.
use sha2::{Digest, Sha256};

use crate::provider_validation::charge::checked_u32_len;
use crate::provider_wit::exports::mcode::provider_pack::provider_api::OrdinaryHeader;

use super::types::{
    AdapterCollection, AdapterContractV1, AdapterEnumSource, AdapterModelSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterValidationError,
    AdapterValidationResult, AdapterVariantSource, ContractNodeBody, OrdinaryHeaderRule,
    PathSegment, TypedJsonConstant,
};

const CONTRACT_DOMAIN: &[u8] = b"mcode-provider-adapter-contract-v1\0";
const BODY_DOMAIN: &[u8] = b"mcode-provider-wire-body-v1\0";
const HEADER_DOMAIN: &[u8] = b"mcode-provider-ordinary-headers-v1\0";

pub(in crate::provider_validation) fn contract_digest(
    contract: &AdapterContractV1,
) -> AdapterValidationResult<String> {
    let mut hash = Sha256::new();
    hash.update(CONTRACT_DOMAIN);
    hash.update([contract.version]);
    hash.update([contract.wire_id.ordinal()]);
    hash.update([model_source_ordinal(contract.model_source)]);
    hash.update(contract.tree.root.to_be_bytes());
    hash_count(&mut hash, contract.tree.nodes.len())?;
    for node in &contract.tree.nodes {
        hash_option_u32(&mut hash, node.parent);
        match &node.segment {
            None => hash.update([0]),
            Some(segment) => {
                hash.update([1]);
                match segment {
                    PathSegment::Key(value) => {
                        hash.update([0]);
                        hash_string(&mut hash, value)?;
                    }
                    PathSegment::ArrayItem => hash.update([1]),
                }
            }
        }
        hash.update([presence_ordinal(node.presence)]);
        hash_option_tag(&mut hash, node.presence_source.map(presence_source_ordinal));
        hash_node_body(&mut hash, &node.body)?;
    }
    hash_count(&mut hash, contract.tree.tables.len())?;
    for table in &contract.tree.tables {
        hash.update([enum_source_ordinal(table.source)]);
        hash_count(&mut hash, table.entries.len())?;
        for entry in &table.entries {
            hash.update([entry.variant_ordinal]);
            hash_string(&mut hash, &entry.token)?;
        }
    }
    hash_count(&mut hash, contract.ordinary_header_rules.len())?;
    for rule in &contract.ordinary_header_rules {
        match rule {
            OrdinaryHeaderRule::Fixed(rule) => {
                hash.update([0]);
                hash_string(&mut hash, &rule.name)?;
                hash_string(&mut hash, &rule.value)?;
            }
            OrdinaryHeaderRule::OneOf(rule) => {
                hash.update([1]);
                hash_string(&mut hash, &rule.name)?;
                hash_count(&mut hash, rule.values.len())?;
                for value in &rule.values {
                    hash_string(&mut hash, value)?;
                }
                hash.update([u8::from(rule.required)]);
            }
        }
    }
    hash.update([contract.decoder_kind.ordinal()]);
    Ok(digest_text(&hash.finalize()))
}

pub(in crate::provider_validation) fn body_digest(body: &[u8]) -> AdapterValidationResult<String> {
    let length = u64::try_from(body.len()).map_err(|_| AdapterValidationError::Limit)?;
    let mut hash = Sha256::new();
    hash.update(BODY_DOMAIN);
    hash.update(length.to_be_bytes());
    hash.update(body);
    Ok(digest_text(&hash.finalize()))
}

pub(in crate::provider_validation) fn ordinary_header_digest(
    headers: &[OrdinaryHeader],
) -> AdapterValidationResult<String> {
    let mut hash = Sha256::new();
    hash.update(HEADER_DOMAIN);
    hash_count(&mut hash, headers.len())?;
    for header in headers {
        hash_string(&mut hash, &header.name)?;
        hash_string(&mut hash, &header.value)?;
    }
    Ok(digest_text(&hash.finalize()))
}

fn hash_node_body(hash: &mut Sha256, body: &ContractNodeBody) -> AdapterValidationResult<()> {
    match body {
        ContractNodeBody::Object(object) => {
            hash.update([0]);
            hash_count(hash, object.children.len())?;
            for child in &object.children {
                hash.update(child.to_be_bytes());
            }
        }
        ContractNodeBody::Array(array) => {
            hash.update([1, collection_ordinal(array.collection)]);
            hash.update(array.item.to_be_bytes());
            hash.update(array.min.to_be_bytes());
            hash.update(array.max.to_be_bytes());
        }
        ContractNodeBody::Switch(value) => {
            hash.update([2, variant_source_ordinal(value.source)]);
            hash_count(hash, value.cases.len())?;
            for case in &value.cases {
                hash.update([case.variant_ordinal]);
                hash.update(case.node.to_be_bytes());
            }
        }
        ContractNodeBody::Value(value) => {
            hash.update([3, scalar_source_ordinal(value.source)]);
            hash_transform(hash, &value.transform);
        }
        ContractNodeBody::Constant(constant) => {
            hash.update([4]);
            match &constant.value {
                TypedJsonConstant::Null => hash.update([0]),
                TypedJsonConstant::Boolean(value) => hash.update([1, u8::from(*value)]),
                TypedJsonConstant::Number(value) => {
                    hash.update([2]);
                    hash_string(hash, value)?;
                }
                TypedJsonConstant::String(value) => {
                    hash.update([3]);
                    hash_string(hash, value)?;
                }
            }
        }
    }
    Ok(())
}

fn hash_transform(hash: &mut Sha256, transform: &AdapterTransform) {
    let (tag, table) = match transform {
        AdapterTransform::Identity => (0, None),
        AdapterTransform::CheckedU32 => (1, None),
        AdapterTransform::CheckedU64 => (2, None),
        AdapterTransform::JsonSubtree => (3, None),
        AdapterTransform::CanonicalJsonString => (4, None),
        AdapterTransform::MistralToolResultContent => (5, None),
        AdapterTransform::JoinLf => (6, None),
        AdapterTransform::Base64StandardPadded => (7, None),
        AdapterTransform::Base64StandardUnpadded => (8, None),
        AdapterTransform::DataUri => (9, None),
        AdapterTransform::EnumToken(index) => (10, Some(*index)),
    };
    hash.update([tag]);
    if let Some(index) = table {
        hash.update(index.to_be_bytes());
    }
}

fn hash_count(hash: &mut Sha256, length: usize) -> AdapterValidationResult<()> {
    hash.update(
        checked_u32_len(length)
            .map_err(|_| AdapterValidationError::Limit)?
            .to_be_bytes(),
    );
    Ok(())
}

fn hash_string(hash: &mut Sha256, value: &str) -> AdapterValidationResult<()> {
    hash_count(hash, value.len())?;
    hash.update(value.as_bytes());
    Ok(())
}

fn hash_option_u32(hash: &mut Sha256, value: Option<u32>) {
    match value {
        None => hash.update([0]),
        Some(value) => {
            hash.update([1]);
            hash.update(value.to_be_bytes());
        }
    }
}

fn hash_option_tag(hash: &mut Sha256, value: Option<u8>) {
    match value {
        None => hash.update([0]),
        Some(value) => hash.update([1, value]),
    }
}

const fn model_source_ordinal(value: AdapterModelSource) -> u8 {
    match value {
        AdapterModelSource::RequestedSelection => 0,
        AdapterModelSource::CurrentModel => 1,
    }
}

const fn collection_ordinal(value: AdapterCollection) -> u8 {
    match value {
        AdapterCollection::System => 0,
        AdapterCollection::Messages => 1,
        AdapterCollection::SystemMessages => 2,
        AdapterCollection::Blocks => 3,
        AdapterCollection::Tools => 4,
    }
}

const fn variant_source_ordinal(value: AdapterVariantSource) -> u8 {
    match value {
        AdapterVariantSource::ModelSelection => 0,
        AdapterVariantSource::SystemMessageEntry => 1,
        AdapterVariantSource::Message => 2,
        AdapterVariantSource::UserBlock => 3,
        AdapterVariantSource::AssistantBlock => 4,
        AdapterVariantSource::ToolResultBlock => 5,
        AdapterVariantSource::ToolResultStatus => 6,
        AdapterVariantSource::ToolChoice => 7,
        AdapterVariantSource::Reasoning => 8,
        AdapterVariantSource::CacheRetention => 9,
    }
}

const fn scalar_source_ordinal(value: AdapterScalarSource) -> u8 {
    match value {
        AdapterScalarSource::SelectedModel => 0,
        AdapterScalarSource::SelectionKind => 1,
        AdapterScalarSource::SystemItem => 2,
        AdapterScalarSource::SystemJoined => 3,
        AdapterScalarSource::MessageRole => 4,
        AdapterScalarSource::BlockKind => 5,
        AdapterScalarSource::BlockText => 6,
        AdapterScalarSource::ToolResultCallId => 7,
        AdapterScalarSource::ToolResultIsError => 8,
        AdapterScalarSource::ToolResultStatus => 9,
        AdapterScalarSource::ToolResultName => 10,
        AdapterScalarSource::MistralToolResultContent => 11,
        AdapterScalarSource::ToolCallId => 12,
        AdapterScalarSource::ToolCallName => 13,
        AdapterScalarSource::ToolCallArguments => 14,
        AdapterScalarSource::ToolName => 15,
        AdapterScalarSource::ToolDescription => 16,
        AdapterScalarSource::ToolSchema => 17,
        AdapterScalarSource::ReasoningKind => 18,
        AdapterScalarSource::Proof => 19,
        AdapterScalarSource::ImageBytes => 20,
        AdapterScalarSource::ImageMediaType => 21,
        AdapterScalarSource::ImageWidth => 22,
        AdapterScalarSource::ImageHeight => 23,
        AdapterScalarSource::ImageFrames => 24,
        AdapterScalarSource::ImageDataUri => 25,
        AdapterScalarSource::ToolChoiceKind => 26,
        AdapterScalarSource::ToolChoiceName => 27,
        AdapterScalarSource::ReasoningMode => 28,
        AdapterScalarSource::ReasoningEffort => 29,
        AdapterScalarSource::ReasoningBudget => 30,
        AdapterScalarSource::CacheRetention => 31,
        AdapterScalarSource::MaxOutput => 32,
    }
}

const fn presence_ordinal(value: AdapterPresence) -> u8 {
    match value {
        AdapterPresence::Required => 0,
        AdapterPresence::OmitIfNone => 1,
        AdapterPresence::OmitForUnset => 2,
    }
}

const fn presence_source_ordinal(value: AdapterPresenceSource) -> u8 {
    match value {
        AdapterPresenceSource::ReasoningProof => 0,
        AdapterPresenceSource::ReasoningEffort => 1,
        AdapterPresenceSource::ReasoningBudget => 2,
        AdapterPresenceSource::MaxOutput => 3,
        AdapterPresenceSource::ToolChoice => 4,
        AdapterPresenceSource::Reasoning => 5,
        AdapterPresenceSource::CacheRetention => 6,
    }
}

const fn enum_source_ordinal(value: AdapterEnumSource) -> u8 {
    match value {
        AdapterEnumSource::SelectionKind => 0,
        AdapterEnumSource::MessageKind => 1,
        AdapterEnumSource::UserBlockKind => 2,
        AdapterEnumSource::AssistantBlockKind => 3,
        AdapterEnumSource::ToolResultBlockKind => 4,
        AdapterEnumSource::ToolResultStatus => 5,
        AdapterEnumSource::ReasoningKind => 6,
        AdapterEnumSource::ImageMediaType => 7,
        AdapterEnumSource::ToolChoice => 8,
        AdapterEnumSource::ReasoningMode => 9,
        AdapterEnumSource::ReasoningEffort => 10,
        AdapterEnumSource::CacheRetention => 11,
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
