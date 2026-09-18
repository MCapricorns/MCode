//! Adapter-derived JSON values and exact prepared-body comparison.
use crate::provider_validation::charge::LogicalCharge;
use crate::provider_validation::scalar::{self, MAX_LOGICAL_CHARGE, MAX_SAFE_TEXT_BYTES};
use crate::provider_validation::wire_json::{
    canonical_serialize_for_json_string, validate_wire_json,
};
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    WireJsonDocument, WireJsonNode,
};

use super::types::{AdapterValidationError, AdapterValidationResult, TypedJsonConstant};

const MAX_WIRE_NODES: usize = 262_144;
const MAX_WIRE_DEPTH: u8 = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::provider_validation) enum AdapterJson {
    Null,
    Boolean(bool),
    Number(String),
    String { value: String, derived: bool },
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl AdapterJson {
    pub(in crate::provider_validation) fn ordinary_string(value: impl Into<String>) -> Self {
        Self::String {
            value: value.into(),
            derived: false,
        }
    }

    pub(in crate::provider_validation) fn derived_string(value: impl Into<String>) -> Self {
        Self::String {
            value: value.into(),
            derived: true,
        }
    }

    pub(super) fn from_constant(value: &TypedJsonConstant) -> Self {
        match value {
            TypedJsonConstant::Null => Self::Null,
            TypedJsonConstant::Boolean(value) => Self::Boolean(*value),
            TypedJsonConstant::Number(value) => Self::Number(value.clone()),
            TypedJsonConstant::String(value) => Self::ordinary_string(value),
        }
    }
}

pub(in crate::provider_validation) fn from_wire(
    document: &WireJsonDocument,
) -> AdapterValidationResult<AdapterJson> {
    validate_wire_json(document, true).map_err(map_source_error)?;
    from_wire_node(document, document.root as usize)
}

pub(in crate::provider_validation) fn canonical_wire_text(
    document: &WireJsonDocument,
) -> AdapterValidationResult<String> {
    let bytes = canonical_serialize_for_json_string(document).map_err(map_source_error)?;
    String::from_utf8(bytes).map_err(|_| AdapterValidationError::SourceMismatch)
}

pub(in crate::provider_validation) fn compare_and_serialize(
    expected: &AdapterJson,
    prepared: &WireJsonDocument,
) -> AdapterValidationResult<Vec<u8>> {
    if prepared.nodes.is_empty() || prepared.nodes.len() > MAX_WIRE_NODES {
        return Err(AdapterValidationError::Limit);
    }
    let root = usize::try_from(prepared.root).map_err(|_| AdapterValidationError::BodyMismatch)?;
    if root != prepared.nodes.len() - 1 || !matches!(expected, AdapterJson::Object(_)) {
        return Err(AdapterValidationError::BodyMismatch);
    }
    let mut parents = vec![0_u8; prepared.nodes.len()];
    let depth = compare_node(expected, prepared, root, &mut parents)?;
    if depth > MAX_WIRE_DEPTH
        || parents[root] != 0
        || parents[..root].iter().any(|count| *count != 1)
    {
        return Err(AdapterValidationError::BodyMismatch);
    }
    validate_expected_charge(expected)?;
    let length = serialized_length(expected)?;
    if !(2..=MAX_LOGICAL_CHARGE).contains(&length) {
        return Err(AdapterValidationError::Limit);
    }
    let capacity = usize::try_from(length).map_err(|_| AdapterValidationError::Limit)?;
    let mut output = Vec::with_capacity(capacity);
    serialize(expected, &mut output);
    if output.len() != capacity {
        return Err(AdapterValidationError::BodyMismatch);
    }
    Ok(output)
}

fn from_wire_node(
    document: &WireJsonDocument,
    index: usize,
) -> AdapterValidationResult<AdapterJson> {
    match &document.nodes[index] {
        WireJsonNode::NullValue => Ok(AdapterJson::Null),
        WireJsonNode::BooleanValue(value) => Ok(AdapterJson::Boolean(*value)),
        WireJsonNode::NumberValue(value) => Ok(AdapterJson::Number(value.clone())),
        WireJsonNode::StringValue(value) => Ok(AdapterJson::ordinary_string(value)),
        WireJsonNode::ArrayValue(array) => array
            .items
            .iter()
            .map(|child| from_wire_node(document, *child as usize))
            .collect::<Result<Vec<_>, _>>()
            .map(AdapterJson::Array),
        WireJsonNode::ObjectValue(object) => object
            .fields
            .iter()
            .map(|field| {
                Ok((
                    field.key.clone(),
                    from_wire_node(document, field.value as usize)?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(AdapterJson::Object),
    }
}

fn compare_node(
    expected: &AdapterJson,
    prepared: &WireJsonDocument,
    index: usize,
    parents: &mut [u8],
) -> AdapterValidationResult<u8> {
    let actual = prepared
        .nodes
        .get(index)
        .ok_or(AdapterValidationError::BodyMismatch)?;
    match (expected, actual) {
        (AdapterJson::Null, WireJsonNode::NullValue) => Ok(1),
        (AdapterJson::Boolean(left), WireJsonNode::BooleanValue(right)) if left == right => Ok(1),
        (AdapterJson::Number(left), WireJsonNode::NumberValue(right)) if left == right => Ok(1),
        (AdapterJson::String { value, derived }, WireJsonNode::StringValue(actual))
            if value == actual =>
        {
            scalar::safe(
                actual,
                if *derived {
                    MAX_LOGICAL_CHARGE as usize
                } else {
                    MAX_SAFE_TEXT_BYTES
                },
                false,
            )
            .map_err(map_body_error)?;
            Ok(1)
        }
        (AdapterJson::Array(expected), WireJsonNode::ArrayValue(actual))
            if expected.len() == actual.items.len() =>
        {
            let mut maximum = 0;
            for (expected, child) in expected.iter().zip(&actual.items) {
                let child = add_parent(index, *child, parents)?;
                maximum = maximum.max(compare_node(expected, prepared, child, parents)?);
            }
            maximum.checked_add(1).ok_or(AdapterValidationError::Limit)
        }
        (AdapterJson::Object(expected), WireJsonNode::ObjectValue(actual))
            if expected.len() == actual.fields.len() =>
        {
            let mut maximum = 0;
            let mut previous: Option<&[u8]> = None;
            for ((expected_key, expected_value), field) in expected.iter().zip(&actual.fields) {
                if expected_key != &field.key
                    || previous.is_some_and(|old| old >= field.key.as_bytes())
                {
                    return Err(AdapterValidationError::BodyMismatch);
                }
                scalar::safe(&field.key, 256, false).map_err(map_body_error)?;
                previous = Some(field.key.as_bytes());
                let child = add_parent(index, field.value, parents)?;
                maximum = maximum.max(compare_node(expected_value, prepared, child, parents)?);
            }
            maximum.checked_add(1).ok_or(AdapterValidationError::Limit)
        }
        _ => Err(AdapterValidationError::BodyMismatch),
    }
}

fn add_parent(parent: usize, child: u32, parents: &mut [u8]) -> AdapterValidationResult<usize> {
    let child = usize::try_from(child).map_err(|_| AdapterValidationError::BodyMismatch)?;
    if child >= parent || child >= parents.len() {
        return Err(AdapterValidationError::BodyMismatch);
    }
    parents[child] = parents[child]
        .checked_add(1)
        .ok_or(AdapterValidationError::Limit)?;
    if parents[child] != 1 {
        return Err(AdapterValidationError::BodyMismatch);
    }
    Ok(child)
}

fn validate_expected_charge(value: &AdapterJson) -> AdapterValidationResult<()> {
    let mut charge = LogicalCharge::new(MAX_LOGICAL_CHARGE);
    charge.add(4).map_err(map_limit_error)?;
    charge.add(4).map_err(map_limit_error)?;
    charge_value(value, &mut charge)
}

fn charge_value(value: &AdapterJson, charge: &mut LogicalCharge) -> AdapterValidationResult<()> {
    charge.add(4).map_err(map_limit_error)?;
    match value {
        AdapterJson::Null => Ok(()),
        AdapterJson::Boolean(_) => charge.add(1).map_err(map_limit_error),
        AdapterJson::Number(value) | AdapterJson::String { value, .. } => {
            charge.string(value).map_err(map_limit_error)
        }
        AdapterJson::Array(items) => {
            charge.add(4).map_err(map_limit_error)?;
            for item in items {
                charge.add(4).map_err(map_limit_error)?;
                charge_value(item, charge)?;
            }
            Ok(())
        }
        AdapterJson::Object(fields) => {
            charge.add(4).map_err(map_limit_error)?;
            for (key, value) in fields {
                charge.string(key).map_err(map_limit_error)?;
                charge.add(4).map_err(map_limit_error)?;
                charge_value(value, charge)?;
            }
            Ok(())
        }
    }
}

fn serialized_length(value: &AdapterJson) -> AdapterValidationResult<u64> {
    match value {
        AdapterJson::Null => Ok(4),
        AdapterJson::Boolean(true) => Ok(4),
        AdapterJson::Boolean(false) => Ok(5),
        AdapterJson::Number(value) => checked_length(value.len()),
        AdapterJson::String { value, .. } => serialized_string_length(value),
        AdapterJson::Array(items) => {
            let mut length = 2_u64;
            for (index, item) in items.iter().enumerate() {
                if index != 0 {
                    length = checked_add(length, 1)?;
                }
                length = checked_add(length, serialized_length(item)?)?;
            }
            Ok(length)
        }
        AdapterJson::Object(fields) => {
            let mut length = 2_u64;
            for (index, (key, value)) in fields.iter().enumerate() {
                if index != 0 {
                    length = checked_add(length, 1)?;
                }
                length = checked_add(length, serialized_string_length(key)?)?;
                length = checked_add(length, 1)?;
                length = checked_add(length, serialized_length(value)?)?;
            }
            Ok(length)
        }
    }
}

fn serialized_string_length(value: &str) -> AdapterValidationResult<u64> {
    let mut length = 2_u64;
    for byte in value.as_bytes() {
        length = checked_add(
            length,
            if matches!(byte, b'"' | b'\\' | b'\t' | b'\n') {
                2
            } else {
                1
            },
        )?;
    }
    Ok(length)
}

fn checked_length(length: usize) -> AdapterValidationResult<u64> {
    u64::try_from(length).map_err(|_| AdapterValidationError::Limit)
}

fn checked_add(left: u64, right: u64) -> AdapterValidationResult<u64> {
    left.checked_add(right).ok_or(AdapterValidationError::Limit)
}

fn serialize(value: &AdapterJson, output: &mut Vec<u8>) {
    match value {
        AdapterJson::Null => output.extend_from_slice(b"null"),
        AdapterJson::Boolean(true) => output.extend_from_slice(b"true"),
        AdapterJson::Boolean(false) => output.extend_from_slice(b"false"),
        AdapterJson::Number(value) => output.extend_from_slice(value.as_bytes()),
        AdapterJson::String { value, .. } => serialize_string(value, output),
        AdapterJson::Array(items) => {
            output.push(b'[');
            for (position, item) in items.iter().enumerate() {
                if position != 0 {
                    output.push(b',');
                }
                serialize(item, output);
            }
            output.push(b']');
        }
        AdapterJson::Object(fields) => {
            output.push(b'{');
            for (position, (key, value)) in fields.iter().enumerate() {
                if position != 0 {
                    output.push(b',');
                }
                serialize_string(key, output);
                output.push(b':');
                serialize(value, output);
            }
            output.push(b'}');
        }
    }
}

fn serialize_string(value: &str, output: &mut Vec<u8>) {
    output.push(b'"');
    for byte in value.as_bytes() {
        match byte {
            b'"' => output.extend_from_slice(b"\\\""),
            b'\\' => output.extend_from_slice(b"\\\\"),
            b'\t' => output.extend_from_slice(b"\\t"),
            b'\n' => output.extend_from_slice(b"\\n"),
            byte => output.push(*byte),
        }
    }
    output.push(b'"');
}

fn map_source_error(error: crate::provider_validation::ValidationError) -> AdapterValidationError {
    match error {
        crate::provider_validation::ValidationError::InvalidArgument => {
            AdapterValidationError::SourceMismatch
        }
        crate::provider_validation::ValidationError::Limit => AdapterValidationError::Limit,
    }
}

fn map_body_error(error: crate::provider_validation::ValidationError) -> AdapterValidationError {
    match error {
        crate::provider_validation::ValidationError::InvalidArgument => {
            AdapterValidationError::BodyMismatch
        }
        crate::provider_validation::ValidationError::Limit => AdapterValidationError::Limit,
    }
}

fn map_limit_error(_error: crate::provider_validation::ValidationError) -> AdapterValidationError {
    AdapterValidationError::Limit
}
