//! Flat wire-JSON tree validation and canonical serialization.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    WireJsonDocument, WireJsonNode,
};

use super::charge::{LogicalCharge, checked_len};
use super::scalar::{self, MAX_LOGICAL_CHARGE, MAX_SAFE_TEXT_BYTES};
use super::{ValidationError, ValidationResult};

const MAX_WIRE_NODES: usize = 262_144;
const MAX_WIRE_DEPTH: u8 = 64;
const MAX_KEY_BYTES: usize = 256;
const MAX_NUMBER_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WireJsonStats {
    pub(super) logical_charge: u64,
    pub(super) depth: u8,
}

pub(super) fn validate_wire_json(
    document: &WireJsonDocument,
    object_root: bool,
) -> ValidationResult<WireJsonStats> {
    if document.nodes.is_empty() || document.nodes.len() > MAX_WIRE_NODES {
        return Err(ValidationError::Limit);
    }
    let final_index =
        u32::try_from(document.nodes.len() - 1).map_err(|_| ValidationError::Limit)?;
    if document.root != final_index {
        return Err(ValidationError::InvalidArgument);
    }
    if object_root && !matches!(document.nodes.last(), Some(WireJsonNode::ObjectValue(_))) {
        return Err(ValidationError::InvalidArgument);
    }

    let mut parents = vec![0_u32; document.nodes.len()];
    let mut depths = Vec::with_capacity(document.nodes.len());
    let mut charge = LogicalCharge::new(MAX_LOGICAL_CHARGE);
    charge.add(4)?;
    charge.add(4)?;

    for (index, node) in document.nodes.iter().enumerate() {
        let depth = validate_node(index, node, &mut parents, &depths, &mut charge)?;
        if depth > MAX_WIRE_DEPTH {
            return Err(ValidationError::Limit);
        }
        depths.push(depth);
    }

    if parents.last() != Some(&0) || parents[..parents.len() - 1].iter().any(|count| *count != 1) {
        return Err(ValidationError::InvalidArgument);
    }

    Ok(WireJsonStats {
        logical_charge: charge.value(),
        depth: depths[document.root as usize],
    })
}

pub(super) fn canonical_serialize(document: &WireJsonDocument) -> ValidationResult<Vec<u8>> {
    validate_wire_json(document, false)?;
    serialize_sized(
        document,
        serialized_node_length(document, document.root as usize)?,
    )
}

pub(super) fn canonical_serialize_for_json_string(
    document: &WireJsonDocument,
) -> ValidationResult<Vec<u8>> {
    validate_wire_json(document, true)?;
    let (length, escapes) = serialized_node_stats(document, document.root as usize)?;
    let enclosing_length = 2_u64
        .checked_add(length)
        .and_then(|value| value.checked_add(escapes))
        .ok_or(ValidationError::Limit)?;
    if enclosing_length > MAX_LOGICAL_CHARGE {
        return Err(ValidationError::Limit);
    }
    serialize_sized(document, length)
}

fn serialize_sized(document: &WireJsonDocument, length: u64) -> ValidationResult<Vec<u8>> {
    if length > MAX_LOGICAL_CHARGE {
        return Err(ValidationError::Limit);
    }
    let capacity = usize::try_from(length).map_err(|_| ValidationError::Limit)?;
    let mut output = Vec::with_capacity(capacity);
    serialize_node(document, document.root as usize, &mut output);
    if output.len() != capacity {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(output)
}

pub(super) fn is_canonical_number(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !(1..=MAX_NUMBER_BYTES).contains(&bytes.len()) || !bytes.is_ascii() {
        return false;
    }
    if bytes == b"0" {
        return true;
    }

    let mut index = usize::from(bytes[0] == b'-');
    if index == bytes.len() {
        return false;
    }
    if bytes[index] == b'0' {
        index += 1;
        if index == bytes.len() || bytes[index] != b'.' {
            return false;
        }
        index += 1;
        if !consume_fraction(bytes, &mut index) {
            return false;
        }
    } else if (b'1'..=b'9').contains(&bytes[index]) {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index < bytes.len() && bytes[index] == b'.' {
            index += 1;
            if !consume_fraction(bytes, &mut index) {
                return false;
            }
        }
    } else {
        return false;
    }

    if index < bytes.len() && bytes[index] == b'e' {
        index += 1;
        if index < bytes.len() && bytes[index] == b'-' {
            index += 1;
        }
        if index == bytes.len() || !(b'1'..=b'9').contains(&bytes[index]) {
            return false;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    index == bytes.len()
}

fn validate_node(
    index: usize,
    node: &WireJsonNode,
    parents: &mut [u32],
    depths: &[u8],
    charge: &mut LogicalCharge,
) -> ValidationResult<u8> {
    charge.add(4)?;
    match node {
        WireJsonNode::NullValue => Ok(1),
        WireJsonNode::BooleanValue(_) => {
            charge.add(1)?;
            Ok(1)
        }
        WireJsonNode::NumberValue(value) => {
            charge.string(value)?;
            if !is_canonical_number(value) {
                return Err(ValidationError::InvalidArgument);
            }
            Ok(1)
        }
        WireJsonNode::StringValue(value) => {
            charge.string(value)?;
            scalar::safe(value, MAX_SAFE_TEXT_BYTES, false)?;
            Ok(1)
        }
        WireJsonNode::ArrayValue(array) => {
            charge.add(4)?;
            for child in &array.items {
                charge.add(4)?;
                add_parent(index, *child, parents)?;
            }
            container_depth(&array.items, depths)
        }
        WireJsonNode::ObjectValue(object) => {
            charge.add(4)?;
            let mut previous: Option<&[u8]> = None;
            let mut child_depth = 0;
            for field in &object.fields {
                charge.string(&field.key)?;
                charge.add(4)?;
                scalar::safe(&field.key, MAX_KEY_BYTES, false)?;
                let key = field.key.as_bytes();
                if previous.is_some_and(|old| old >= key) {
                    return Err(ValidationError::InvalidArgument);
                }
                previous = Some(key);
                let child = add_parent(index, field.value, parents)?;
                child_depth = child_depth.max(depths[child]);
            }
            child_depth.checked_add(1).ok_or(ValidationError::Limit)
        }
    }
}

fn add_parent(index: usize, child: u32, parents: &mut [u32]) -> ValidationResult<usize> {
    let child = usize::try_from(child).map_err(|_| ValidationError::InvalidArgument)?;
    if child >= index {
        return Err(ValidationError::InvalidArgument);
    }
    parents[child] = parents[child]
        .checked_add(1)
        .ok_or(ValidationError::Limit)?;
    if parents[child] > 1 {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(child)
}

fn container_depth(children: &[u32], depths: &[u8]) -> ValidationResult<u8> {
    let child_depth = children
        .iter()
        .map(|child| depths[*child as usize])
        .max()
        .unwrap_or(0);
    child_depth.checked_add(1).ok_or(ValidationError::Limit)
}

fn consume_fraction(bytes: &[u8], index: &mut usize) -> bool {
    let start = *index;
    while *index < bytes.len() && bytes[*index].is_ascii_digit() {
        *index += 1;
    }
    *index > start && bytes[*index - 1] != b'0'
}

fn serialized_node_length(document: &WireJsonDocument, index: usize) -> ValidationResult<u64> {
    serialized_node_stats(document, index).map(|stats| stats.0)
}

fn serialized_node_stats(
    document: &WireJsonDocument,
    index: usize,
) -> ValidationResult<(u64, u64)> {
    match &document.nodes[index] {
        WireJsonNode::NullValue => Ok((4, 0)),
        WireJsonNode::BooleanValue(true) => Ok((4, 0)),
        WireJsonNode::BooleanValue(false) => Ok((5, 0)),
        WireJsonNode::NumberValue(value) => Ok((checked_len(value.len())?, 0)),
        WireJsonNode::StringValue(value) => serialized_string_stats(value),
        WireJsonNode::ArrayValue(array) => {
            let mut length = 2_u64;
            let mut escapes = 0_u64;
            for (position, child) in array.items.iter().enumerate() {
                if position != 0 {
                    length = checked_add(length, 1)?;
                }
                let child = serialized_node_stats(document, *child as usize)?;
                length = checked_add(length, child.0)?;
                escapes = checked_add(escapes, child.1)?;
            }
            Ok((length, escapes))
        }
        WireJsonNode::ObjectValue(object) => {
            let mut length = 2_u64;
            let mut escapes = 0_u64;
            for (position, field) in object.fields.iter().enumerate() {
                if position != 0 {
                    length = checked_add(length, 1)?;
                }
                let key = serialized_string_stats(&field.key)?;
                length = checked_add(length, key.0)?;
                escapes = checked_add(escapes, key.1)?;
                length = checked_add(length, 1)?;
                let child = serialized_node_stats(document, field.value as usize)?;
                length = checked_add(length, child.0)?;
                escapes = checked_add(escapes, child.1)?;
            }
            Ok((length, escapes))
        }
    }
}

fn serialized_string_stats(value: &str) -> ValidationResult<(u64, u64)> {
    let mut length = 2_u64;
    let mut escapes = 2_u64;
    for byte in value.as_bytes() {
        let escaped = matches!(byte, b'"' | b'\\' | b'\t' | b'\n');
        length = checked_add(length, if escaped { 2 } else { 1 })?;
        if escaped {
            escapes = checked_add(escapes, u64::from(matches!(byte, b'"' | b'\\')) + 1)?;
        }
    }
    Ok((length, escapes))
}

fn checked_add(left: u64, right: u64) -> ValidationResult<u64> {
    left.checked_add(right).ok_or(ValidationError::Limit)
}

fn serialize_node(document: &WireJsonDocument, index: usize, output: &mut Vec<u8>) {
    match &document.nodes[index] {
        WireJsonNode::NullValue => output.extend_from_slice(b"null"),
        WireJsonNode::BooleanValue(true) => output.extend_from_slice(b"true"),
        WireJsonNode::BooleanValue(false) => output.extend_from_slice(b"false"),
        WireJsonNode::NumberValue(value) => output.extend_from_slice(value.as_bytes()),
        WireJsonNode::StringValue(value) => serialize_string(value, output),
        WireJsonNode::ArrayValue(array) => {
            output.push(b'[');
            for (position, child) in array.items.iter().enumerate() {
                if position != 0 {
                    output.push(b',');
                }
                serialize_node(document, *child as usize, output);
            }
            output.push(b']');
        }
        WireJsonNode::ObjectValue(object) => {
            output.push(b'{');
            for (position, field) in object.fields.iter().enumerate() {
                if position != 0 {
                    output.push(b',');
                }
                serialize_string(&field.key, output);
                output.push(b':');
                serialize_node(document, field.value as usize, output);
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
