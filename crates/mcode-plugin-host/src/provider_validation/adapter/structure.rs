//! Structural validation for closed adapter contracts.
use std::collections::BTreeSet;

use super::types::{
    AdapterCollection, AdapterContractV1, AdapterEnumSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, ContractNodeBody,
    OrdinaryHeaderRule, PathSegment, TypedJsonConstant,
};
use super::types::{AdapterValidationError, AdapterValidationResult};
use crate::provider_validation::charge::{LogicalCharge, checked_len};
use crate::provider_validation::prepare::{
    denied_header, validate_header_name, validate_header_value,
};
use crate::provider_validation::scalar::{self, MIB};
use crate::provider_validation::wire_json::is_canonical_number;

const MAX_CONTRACT_NODES: usize = 4_096;
const MAX_CONTRACT_DEPTH: u8 = 32;
const MAX_PATH_DEPTH: u8 = 16;
const MAX_TABLES: usize = 64;
const MAX_HEADER_RULES: usize = 32;

pub(in crate::provider_validation) fn validate_contract(
    contract: &AdapterContractV1,
) -> AdapterValidationResult<()> {
    if contract.version != 1 || contract.wire_id.ordinal() != contract.decoder_kind.ordinal() {
        return Err(AdapterValidationError::InvalidContract);
    }
    validate_tree(contract)?;
    validate_header_rules(&contract.ordinary_header_rules)
}

fn validate_tree(contract: &AdapterContractV1) -> AdapterValidationResult<()> {
    let tree = &contract.tree;
    if tree.nodes.is_empty() || tree.nodes.len() > MAX_CONTRACT_NODES {
        return Err(AdapterValidationError::Limit);
    }
    let root = usize::try_from(tree.root).map_err(|_| AdapterValidationError::InvalidContract)?;
    if root != tree.nodes.len() - 1 {
        return Err(AdapterValidationError::InvalidContract);
    }
    let root_node = &tree.nodes[root];
    if root_node.parent.is_some()
        || root_node.segment.is_some()
        || root_node.presence != AdapterPresence::Required
        || root_node.presence_source.is_some()
        || !matches!(root_node.body, ContractNodeBody::Object(_))
    {
        return Err(AdapterValidationError::InvalidContract);
    }
    if tree.tables.len() > MAX_TABLES {
        return Err(AdapterValidationError::Limit);
    }

    validate_tables(contract)?;
    let mut references = vec![0_u8; tree.nodes.len()];
    let mut expected_parent = vec![None; tree.nodes.len()];
    let mut depths = Vec::with_capacity(tree.nodes.len());

    for (index, node) in tree.nodes.iter().enumerate() {
        validate_presence(node.presence, node.presence_source)?;
        validate_body(
            contract,
            index,
            &node.body,
            &mut references,
            &mut expected_parent,
        )?;
        depths.push(node_depth(&node.body, &depths)?);
    }

    let mut path_depths = vec![0_u8; tree.nodes.len()];
    for index in (0..root).rev() {
        let node = &tree.nodes[index];
        let parent = usize::try_from(node.parent.ok_or(AdapterValidationError::InvalidContract)?)
            .map_err(|_| AdapterValidationError::InvalidContract)?;
        if parent <= index || parent >= tree.nodes.len() || expected_parent[index] != Some(parent) {
            return Err(AdapterValidationError::InvalidContract);
        }
        path_depths[index] = path_depths[parent]
            .checked_add(u8::from(node.segment.is_some()))
            .ok_or(AdapterValidationError::Limit)?;
    }

    if references[root] != 0
        || references[..root].iter().any(|count| *count != 1)
        || depths.iter().any(|depth| *depth > MAX_CONTRACT_DEPTH)
        || path_depths[..root]
            .iter()
            .any(|depth| !(1..=MAX_PATH_DEPTH).contains(depth))
    {
        return Err(AdapterValidationError::InvalidContract);
    }
    charge_tree(contract)?;
    super::semantics::validate_semantics(contract)
}

fn validate_body(
    contract: &AdapterContractV1,
    index: usize,
    body: &ContractNodeBody,
    references: &mut [u8],
    expected_parent: &mut [Option<usize>],
) -> AdapterValidationResult<()> {
    match body {
        ContractNodeBody::Object(object) => {
            let mut previous: Option<&[u8]> = None;
            for child in &object.children {
                let child = add_reference(index, *child, references, expected_parent)?;
                let Some(PathSegment::Key(key)) = contract.tree.nodes[child].segment.as_ref()
                else {
                    return Err(AdapterValidationError::InvalidContract);
                };
                scalar::safe(key.as_str(), 128, true).map_err(map_contract_error)?;
                let bytes = key.as_bytes();
                if previous.is_some_and(|old| old >= bytes) {
                    return Err(AdapterValidationError::InvalidContract);
                }
                previous = Some(bytes);
            }
        }
        ContractNodeBody::Array(array) => {
            if array.min > array.max
                || array.max > 262_144
                || array.max > collection_bound(array.collection)
                || (array.collection == AdapterCollection::Blocks && array.min == 0)
            {
                return Err(AdapterValidationError::InvalidContract);
            }
            let child = add_reference(index, array.item, references, expected_parent)?;
            if contract.tree.nodes[child].segment != Some(PathSegment::ArrayItem) {
                return Err(AdapterValidationError::InvalidContract);
            }
            validate_array_item_shape(contract, array.collection, child)?;
        }
        ContractNodeBody::Switch(switch) => {
            if switch.cases.len() != usize::from(switch.source.variant_count()) {
                return Err(AdapterValidationError::InvalidContract);
            }
            for (ordinal, case) in switch.cases.iter().enumerate() {
                if usize::from(case.variant_ordinal) != ordinal {
                    return Err(AdapterValidationError::InvalidContract);
                }
                let child = add_reference(index, case.node, references, expected_parent)?;
                if contract.tree.nodes[child].segment.is_some() {
                    return Err(AdapterValidationError::InvalidContract);
                }
            }
        }
        ContractNodeBody::Value(value) => {
            validate_transform(contract, value.source, &value.transform)?
        }
        ContractNodeBody::Constant(constant) => validate_constant(&constant.value)?,
    }
    Ok(())
}

fn node_depth(body: &ContractNodeBody, depths: &[u8]) -> AdapterValidationResult<u8> {
    let child_depth = match body {
        ContractNodeBody::Object(object) => object
            .children
            .iter()
            .map(|child| depths[*child as usize])
            .max()
            .unwrap_or(0),
        ContractNodeBody::Array(array) => depths[array.item as usize],
        ContractNodeBody::Switch(switch) => switch
            .cases
            .iter()
            .map(|case| depths[case.node as usize])
            .max()
            .unwrap_or(0),
        ContractNodeBody::Value(_) | ContractNodeBody::Constant(_) => 0,
    };
    child_depth
        .checked_add(1)
        .ok_or(AdapterValidationError::Limit)
}

fn validate_array_item_shape(
    contract: &AdapterContractV1,
    collection: AdapterCollection,
    item: usize,
) -> AdapterValidationResult<()> {
    let body = &contract.tree.nodes[item].body;
    let valid = match collection {
        AdapterCollection::System => {
            count_scalar_source(contract, item, AdapterScalarSource::SystemItem) == 1
        }
        AdapterCollection::Messages => matches!(
            body,
            ContractNodeBody::Switch(value)
                if value.source == super::types::AdapterVariantSource::Message
        ),
        AdapterCollection::SystemMessages => matches!(
            body,
            ContractNodeBody::Switch(value)
                if value.source == super::types::AdapterVariantSource::SystemMessageEntry
        ),
        AdapterCollection::Blocks => matches!(
            body,
            ContractNodeBody::Switch(value)
                if matches!(
                    value.source,
                    super::types::AdapterVariantSource::UserBlock
                        | super::types::AdapterVariantSource::AssistantBlock
                        | super::types::AdapterVariantSource::ToolResultBlock
                )
        ),
        AdapterCollection::Tools => {
            count_scalar_source(contract, item, AdapterScalarSource::ToolName) == 1
                && count_scalar_source(contract, item, AdapterScalarSource::ToolDescription) == 1
                && count_scalar_source(contract, item, AdapterScalarSource::ToolSchema) == 1
        }
    };
    valid
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn count_scalar_source(
    contract: &AdapterContractV1,
    node: usize,
    source: AdapterScalarSource,
) -> usize {
    match &contract.tree.nodes[node].body {
        ContractNodeBody::Object(value) => value
            .children
            .iter()
            .map(|child| count_scalar_source(contract, *child as usize, source))
            .sum(),
        ContractNodeBody::Array(value) => {
            count_scalar_source(contract, value.item as usize, source)
        }
        ContractNodeBody::Switch(value) => value
            .cases
            .iter()
            .map(|case| count_scalar_source(contract, case.node as usize, source))
            .sum(),
        ContractNodeBody::Value(value) => usize::from(value.source == source),
        ContractNodeBody::Constant(_) => 0,
    }
}

fn add_reference(
    parent: usize,
    child: u32,
    references: &mut [u8],
    expected_parent: &mut [Option<usize>],
) -> AdapterValidationResult<usize> {
    let child = usize::try_from(child).map_err(|_| AdapterValidationError::InvalidContract)?;
    if child >= parent || child >= references.len() {
        return Err(AdapterValidationError::InvalidContract);
    }
    references[child] = references[child]
        .checked_add(1)
        .ok_or(AdapterValidationError::Limit)?;
    if references[child] != 1 {
        return Err(AdapterValidationError::InvalidContract);
    }
    expected_parent[child] = Some(parent);
    Ok(child)
}

fn validate_presence(
    presence: AdapterPresence,
    source: Option<AdapterPresenceSource>,
) -> AdapterValidationResult<()> {
    match (presence, source) {
        (AdapterPresence::Required, None)
        | (
            AdapterPresence::OmitIfNone,
            Some(
                AdapterPresenceSource::ReasoningProof
                | AdapterPresenceSource::ReasoningEffort
                | AdapterPresenceSource::ReasoningBudget
                | AdapterPresenceSource::MaxOutput,
            ),
        )
        | (
            AdapterPresence::OmitForUnset,
            Some(
                AdapterPresenceSource::ToolChoice
                | AdapterPresenceSource::Reasoning
                | AdapterPresenceSource::CacheRetention,
            ),
        ) => Ok(()),
        _ => Err(AdapterValidationError::InvalidContract),
    }
}

fn validate_tables(contract: &AdapterContractV1) -> AdapterValidationResult<()> {
    let mut used = vec![false; contract.tree.tables.len()];
    for node in &contract.tree.nodes {
        if let ContractNodeBody::Value(value) = &node.body
            && let AdapterTransform::EnumToken(index) = value.transform
        {
            let index = usize::from(index);
            if index >= MAX_TABLES || index >= contract.tree.tables.len() {
                return Err(AdapterValidationError::InvalidContract);
            }
            let table_source = contract.tree.tables[index].source;
            let source_matches = if value.source == AdapterScalarSource::BlockKind {
                matches!(
                    table_source,
                    AdapterEnumSource::UserBlockKind
                        | AdapterEnumSource::AssistantBlockKind
                        | AdapterEnumSource::ToolResultBlockKind
                )
            } else {
                enum_source(value.source) == Some(table_source)
            };
            if !source_matches {
                return Err(AdapterValidationError::InvalidContract);
            }
            used[index] = true;
        }
    }
    if used.iter().any(|used| !used) {
        return Err(AdapterValidationError::InvalidContract);
    }
    for table in &contract.tree.tables {
        if table.entries.len() != usize::from(table.source.variant_count())
            || table.entries.len() > 16
        {
            return Err(AdapterValidationError::InvalidContract);
        }
        let mut tokens = BTreeSet::new();
        for (ordinal, entry) in table.entries.iter().enumerate() {
            if usize::from(entry.variant_ordinal) != ordinal {
                return Err(AdapterValidationError::InvalidContract);
            }
            scalar::visible_ascii(&entry.token, 128).map_err(map_contract_error)?;
            if !tokens.insert(entry.token.as_str()) {
                return Err(AdapterValidationError::InvalidContract);
            }
        }
    }
    Ok(())
}

pub(in crate::provider_validation) fn validate_transform(
    contract: &AdapterContractV1,
    source: AdapterScalarSource,
    transform: &AdapterTransform,
) -> AdapterValidationResult<()> {
    use AdapterScalarSource as Source;
    use AdapterTransform as Transform;
    let legal = match source {
        Source::SelectedModel
        | Source::SystemItem
        | Source::BlockText
        | Source::ToolResultCallId
        | Source::ToolResultName
        | Source::ToolCallId
        | Source::ToolCallName
        | Source::ToolName
        | Source::ToolDescription
        | Source::ToolChoiceName => matches!(transform, Transform::Identity),
        Source::ToolResultIsError => matches!(transform, Transform::Identity),
        Source::SelectionKind
        | Source::MessageRole
        | Source::BlockKind
        | Source::ToolResultStatus
        | Source::ReasoningKind
        | Source::ImageMediaType
        | Source::ToolChoiceKind
        | Source::ReasoningMode
        | Source::ReasoningEffort
        | Source::CacheRetention => enum_transform_matches(contract, source, transform),
        Source::ImageWidth | Source::ImageHeight | Source::ImageFrames => {
            matches!(transform, Transform::CheckedU32)
        }
        Source::ReasoningBudget | Source::MaxOutput => matches!(transform, Transform::CheckedU64),
        Source::ToolCallArguments => {
            matches!(transform, Transform::JsonSubtree)
                || (matches!(transform, Transform::CanonicalJsonString)
                    && supports_canonical_arguments(contract.wire_id))
        }
        Source::ToolSchema => matches!(transform, Transform::JsonSubtree),
        Source::Proof | Source::ImageBytes => matches!(
            transform,
            Transform::Base64StandardPadded | Transform::Base64StandardUnpadded
        ),
        Source::SystemJoined => matches!(transform, Transform::JoinLf),
        Source::ImageDataUri => matches!(transform, Transform::DataUri),
        Source::MistralToolResultContent => {
            matches!(transform, Transform::MistralToolResultContent)
                && contract.wire_id == super::types::AdapterWireId::MistralConversations
        }
    };
    legal
        .then_some(())
        .ok_or(AdapterValidationError::InvalidContract)
}

fn enum_transform_matches(
    contract: &AdapterContractV1,
    source: AdapterScalarSource,
    transform: &AdapterTransform,
) -> bool {
    let AdapterTransform::EnumToken(index) = transform else {
        return false;
    };
    let Some(table) = contract.tree.tables.get(usize::from(*index)) else {
        return false;
    };
    if source == AdapterScalarSource::BlockKind {
        matches!(
            table.source,
            AdapterEnumSource::UserBlockKind
                | AdapterEnumSource::AssistantBlockKind
                | AdapterEnumSource::ToolResultBlockKind
        )
    } else {
        enum_source(source) == Some(table.source)
    }
}

fn validate_constant(value: &TypedJsonConstant) -> AdapterValidationResult<()> {
    match value {
        TypedJsonConstant::Null | TypedJsonConstant::Boolean(_) => Ok(()),
        TypedJsonConstant::Number(value) => is_canonical_number(value)
            .then_some(())
            .ok_or(AdapterValidationError::InvalidContract),
        TypedJsonConstant::String(value) => {
            scalar::safe(value, 64 * 1_024, false).map_err(map_contract_error)
        }
    }
}

fn validate_header_rules(rules: &[OrdinaryHeaderRule]) -> AdapterValidationResult<()> {
    if rules.len() > MAX_HEADER_RULES {
        return Err(AdapterValidationError::Limit);
    }
    let mut previous: Option<&[u8]> = None;
    for rule in rules {
        let name = match rule {
            OrdinaryHeaderRule::Fixed(rule) => {
                validate_header_value(&rule.value).map_err(map_contract_error)?;
                rule.name.as_str()
            }
            OrdinaryHeaderRule::OneOf(rule) => {
                if rule.values.is_empty() || rule.values.len() > 16 {
                    return Err(AdapterValidationError::InvalidContract);
                }
                let mut old: Option<&[u8]> = None;
                for value in &rule.values {
                    validate_header_value(value).map_err(map_contract_error)?;
                    if old.is_some_and(|old| old >= value.as_bytes()) {
                        return Err(AdapterValidationError::InvalidContract);
                    }
                    old = Some(value.as_bytes());
                }
                rule.name.as_str()
            }
        };
        validate_header_name(name).map_err(map_contract_error)?;
        if denied_header(name, &[]) || previous.is_some_and(|old| old >= name.as_bytes()) {
            return Err(AdapterValidationError::InvalidContract);
        }
        previous = Some(name.as_bytes());
    }
    Ok(())
}

fn charge_tree(contract: &AdapterContractV1) -> AdapterValidationResult<()> {
    let mut charge = LogicalCharge::new(MIB);
    charge.add(4).map_err(map_contract_error)?;
    charge.add(4).map_err(map_contract_error)?;
    for node in &contract.tree.nodes {
        charge_option_ref(&mut charge, node.parent)?;
        charge.add(4).map_err(map_contract_error)?;
        if let Some(segment) = &node.segment {
            charge.add(4).map_err(map_contract_error)?;
            if let PathSegment::Key(key) = segment {
                charge.string(key).map_err(map_contract_error)?;
            }
        }
        charge.add(4).map_err(map_contract_error)?;
        charge.add(4).map_err(map_contract_error)?;
        if node.presence_source.is_some() {
            charge.add(4).map_err(map_contract_error)?;
        }
        charge.add(4).map_err(map_contract_error)?;
        charge_body(&mut charge, &node.body)?;
    }
    charge.add(4).map_err(map_contract_error)?;
    for table in &contract.tree.tables {
        charge.add(4).map_err(map_contract_error)?;
        charge.add(4).map_err(map_contract_error)?;
        for entry in &table.entries {
            charge.add(1).map_err(map_contract_error)?;
            charge.string(&entry.token).map_err(map_contract_error)?;
        }
    }
    Ok(())
}

fn charge_body(charge: &mut LogicalCharge, body: &ContractNodeBody) -> AdapterValidationResult<()> {
    match body {
        ContractNodeBody::Object(object) => charge_refs(charge, &object.children),
        ContractNodeBody::Array(_) => charge.add(16).map_err(map_contract_error),
        ContractNodeBody::Switch(switch) => {
            charge.add(4).map_err(map_contract_error)?;
            charge.add(4).map_err(map_contract_error)?;
            for _case in &switch.cases {
                charge.add(5).map_err(map_contract_error)?;
            }
            Ok(())
        }
        ContractNodeBody::Value(value) => {
            charge.add(4).map_err(map_contract_error)?;
            charge.add(4).map_err(map_contract_error)?;
            if matches!(value.transform, AdapterTransform::EnumToken(_)) {
                charge.add(2).map_err(map_contract_error)?;
            }
            Ok(())
        }
        ContractNodeBody::Constant(constant) => {
            charge.add(4).map_err(map_contract_error)?;
            match &constant.value {
                TypedJsonConstant::Null => Ok(()),
                TypedJsonConstant::Boolean(_) => charge.add(1).map_err(map_contract_error),
                TypedJsonConstant::Number(value) | TypedJsonConstant::String(value) => {
                    charge.string(value).map_err(map_contract_error)
                }
            }
        }
    }
}

fn charge_refs(charge: &mut LogicalCharge, refs: &[u32]) -> AdapterValidationResult<()> {
    charge.add(4).map_err(map_contract_error)?;
    charge
        .add(checked_len(refs.len()).map_err(map_contract_error)? * 4)
        .map_err(map_contract_error)
}

fn charge_option_ref(
    charge: &mut LogicalCharge,
    value: Option<u32>,
) -> AdapterValidationResult<()> {
    charge.add(4).map_err(map_contract_error)?;
    if value.is_some() {
        charge.add(4).map_err(map_contract_error)?;
    }
    Ok(())
}

const fn collection_bound(collection: AdapterCollection) -> u32 {
    match collection {
        AdapterCollection::System | AdapterCollection::Tools => 1_024,
        AdapterCollection::Messages | AdapterCollection::Blocks => 4_096,
        AdapterCollection::SystemMessages => 5_120,
    }
}

pub(super) const fn enum_source(source: AdapterScalarSource) -> Option<AdapterEnumSource> {
    match source {
        AdapterScalarSource::SelectionKind => Some(AdapterEnumSource::SelectionKind),
        AdapterScalarSource::MessageRole => Some(AdapterEnumSource::MessageKind),
        AdapterScalarSource::BlockKind => None,
        AdapterScalarSource::ToolResultStatus => Some(AdapterEnumSource::ToolResultStatus),
        AdapterScalarSource::ReasoningKind => Some(AdapterEnumSource::ReasoningKind),
        AdapterScalarSource::ImageMediaType => Some(AdapterEnumSource::ImageMediaType),
        AdapterScalarSource::ToolChoiceKind => Some(AdapterEnumSource::ToolChoice),
        AdapterScalarSource::ReasoningMode => Some(AdapterEnumSource::ReasoningMode),
        AdapterScalarSource::ReasoningEffort => Some(AdapterEnumSource::ReasoningEffort),
        AdapterScalarSource::CacheRetention => Some(AdapterEnumSource::CacheRetention),
        _ => None,
    }
}

const fn supports_canonical_arguments(wire: super::types::AdapterWireId) -> bool {
    matches!(
        wire,
        super::types::AdapterWireId::OpenAiCompletions
            | super::types::AdapterWireId::OpenAiResponses
            | super::types::AdapterWireId::OpenAiCodexResponses
            | super::types::AdapterWireId::AzureOpenAiResponses
            | super::types::AdapterWireId::MistralConversations
    )
}

fn map_contract_error(
    error: crate::provider_validation::ValidationError,
) -> AdapterValidationError {
    match error {
        crate::provider_validation::ValidationError::InvalidArgument => {
            AdapterValidationError::InvalidContract
        }
        crate::provider_validation::ValidationError::Limit => AdapterValidationError::Limit,
    }
}
