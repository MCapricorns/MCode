//! Strict canonical JSON validation for completed tool arguments.
use std::collections::BTreeSet;

use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    WireJsonArray, WireJsonDocument, WireJsonField, WireJsonNode, WireJsonObject,
};

use crate::provider_validation::wire_json::{canonical_serialize, validate_wire_json};
use crate::provider_validation::{ValidationError, ValidationResult};

const MAX_TOOL_ARGUMENT_BYTES: usize = 1_024 * 1_024;
const MAX_TOOL_ARGUMENT_CHARGE: u64 = 1_024 * 1_024;
const MAX_TOOL_ARGUMENT_DEPTH: u8 = 64;
const MAX_TOOL_ARGUMENT_NODES: usize = 16_384;

pub(in crate::provider_validation) fn validate_tool_arguments(value: &str) -> ValidationResult {
    if value.len() > MAX_TOOL_ARGUMENT_BYTES {
        return Err(ValidationError::Limit);
    }

    let document = Parser::new(value).parse()?;
    if document.nodes.len() > MAX_TOOL_ARGUMENT_NODES {
        return Err(ValidationError::Limit);
    }
    let stats = validate_wire_json(&document, true)?;
    if stats.depth > MAX_TOOL_ARGUMENT_DEPTH || stats.logical_charge > MAX_TOOL_ARGUMENT_CHARGE {
        return Err(ValidationError::Limit);
    }
    if canonical_serialize(&document)?.as_slice() != value.as_bytes() {
        return Err(ValidationError::InvalidArgument);
    }
    Ok(())
}

struct Parser<'input> {
    input: &'input str,
    position: usize,
    nodes: Vec<WireJsonNode>,
}

impl<'input> Parser<'input> {
    const fn new(input: &'input str) -> Self {
        Self {
            input,
            position: 0,
            nodes: Vec::new(),
        }
    }

    fn parse(mut self) -> ValidationResult<WireJsonDocument> {
        self.skip_whitespace();
        let root = self.parse_value(1)?;
        self.skip_whitespace();
        if self.position != self.input.len() {
            return Err(ValidationError::InvalidArgument);
        }
        Ok(WireJsonDocument {
            root,
            nodes: self.nodes,
        })
    }

    fn parse_value(&mut self, depth: u8) -> ValidationResult<u32> {
        if depth > MAX_TOOL_ARGUMENT_DEPTH {
            return Err(ValidationError::Limit);
        }
        self.skip_whitespace();
        match self.peek_byte() {
            Some(b'n') => {
                self.consume_literal(b"null")?;
                self.push_node(WireJsonNode::NullValue)
            }
            Some(b't') => {
                self.consume_literal(b"true")?;
                self.push_node(WireJsonNode::BooleanValue(true))
            }
            Some(b'f') => {
                self.consume_literal(b"false")?;
                self.push_node(WireJsonNode::BooleanValue(false))
            }
            Some(b'"') => {
                let value = self.parse_string()?;
                self.push_node(WireJsonNode::StringValue(value))
            }
            Some(b'[') => self.parse_array(depth),
            Some(b'{') => self.parse_object(depth),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            _ => Err(ValidationError::InvalidArgument),
        }
    }

    fn parse_array(&mut self, depth: u8) -> ValidationResult<u32> {
        self.expect_byte(b'[')?;
        self.skip_whitespace();
        let mut items = Vec::new();
        if self.consume_byte(b']') {
            return self.push_node(WireJsonNode::ArrayValue(WireJsonArray { items }));
        }
        let child_depth = depth.checked_add(1).ok_or(ValidationError::Limit)?;
        loop {
            items.push(self.parse_value(child_depth)?);
            self.skip_whitespace();
            if self.consume_byte(b']') {
                break;
            }
            self.expect_byte(b',')?;
        }
        self.push_node(WireJsonNode::ArrayValue(WireJsonArray { items }))
    }

    fn parse_object(&mut self, depth: u8) -> ValidationResult<u32> {
        self.expect_byte(b'{')?;
        self.skip_whitespace();
        let mut fields = Vec::new();
        let mut keys = BTreeSet::new();
        if self.consume_byte(b'}') {
            return self.push_node(WireJsonNode::ObjectValue(WireJsonObject { fields }));
        }
        let child_depth = depth.checked_add(1).ok_or(ValidationError::Limit)?;
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            if !keys.insert(key.clone()) {
                return Err(ValidationError::InvalidArgument);
            }
            self.skip_whitespace();
            self.expect_byte(b':')?;
            let value = self.parse_value(child_depth)?;
            fields.push(WireJsonField { key, value });
            self.skip_whitespace();
            if self.consume_byte(b'}') {
                break;
            }
            self.expect_byte(b',')?;
        }
        self.push_node(WireJsonNode::ObjectValue(WireJsonObject { fields }))
    }

    fn parse_number(&mut self) -> ValidationResult<u32> {
        let start = self.position;
        while self
            .peek_byte()
            .is_some_and(|byte| !matches!(byte, b',' | b']' | b'}' | b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.position += 1;
        }
        let value = self.input[start..self.position].to_owned();
        self.push_node(WireJsonNode::NumberValue(value))
    }

    fn parse_string(&mut self) -> ValidationResult<String> {
        self.expect_byte(b'"')?;
        let mut output = String::new();
        loop {
            let byte = self.peek_byte().ok_or(ValidationError::InvalidArgument)?;
            match byte {
                b'"' => {
                    self.position += 1;
                    return Ok(output);
                }
                b'\\' => {
                    self.position += 1;
                    self.parse_escape(&mut output)?;
                }
                0..=0x1f => return Err(ValidationError::InvalidArgument),
                _ => {
                    let character = self.next_char()?;
                    output.push(character);
                }
            }
        }
    }

    fn parse_escape(&mut self, output: &mut String) -> ValidationResult {
        let escape = self.peek_byte().ok_or(ValidationError::InvalidArgument)?;
        self.position += 1;
        match escape {
            b'"' => output.push('"'),
            b'\\' => output.push('\\'),
            b'/' => output.push('/'),
            b'b' => output.push('\u{0008}'),
            b'f' => output.push('\u{000c}'),
            b'n' => output.push('\n'),
            b'r' => output.push('\r'),
            b't' => output.push('\t'),
            b'u' => self.parse_unicode_escape(output)?,
            _ => return Err(ValidationError::InvalidArgument),
        }
        Ok(())
    }

    fn parse_unicode_escape(&mut self, output: &mut String) -> ValidationResult {
        let first = self.parse_hex_quad()?;
        let scalar = if (0xd800..=0xdbff).contains(&first) {
            self.expect_byte(b'\\')?;
            self.expect_byte(b'u')?;
            let second = self.parse_hex_quad()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(ValidationError::InvalidArgument);
            }
            0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00)
        } else if (0xdc00..=0xdfff).contains(&first) {
            return Err(ValidationError::InvalidArgument);
        } else {
            u32::from(first)
        };
        let character = char::from_u32(scalar).ok_or(ValidationError::InvalidArgument)?;
        output.push(character);
        Ok(())
    }

    fn parse_hex_quad(&mut self) -> ValidationResult<u16> {
        let end = self.position.checked_add(4).ok_or(ValidationError::Limit)?;
        let bytes = self
            .input
            .as_bytes()
            .get(self.position..end)
            .ok_or(ValidationError::InvalidArgument)?;
        let mut value = 0_u16;
        for byte in bytes {
            value = value
                .checked_mul(16)
                .and_then(|value| hex_value(*byte).map(|digit| value + u16::from(digit)))
                .ok_or(ValidationError::InvalidArgument)?;
        }
        self.position = end;
        Ok(value)
    }

    fn push_node(&mut self, node: WireJsonNode) -> ValidationResult<u32> {
        if self.nodes.len() >= MAX_TOOL_ARGUMENT_NODES {
            return Err(ValidationError::Limit);
        }
        let index = u32::try_from(self.nodes.len()).map_err(|_| ValidationError::Limit)?;
        self.nodes.push(node);
        Ok(index)
    }

    fn consume_literal(&mut self, literal: &[u8]) -> ValidationResult {
        let end = self
            .position
            .checked_add(literal.len())
            .ok_or(ValidationError::Limit)?;
        if self.input.as_bytes().get(self.position..end) != Some(literal) {
            return Err(ValidationError::InvalidArgument);
        }
        self.position = end;
        Ok(())
    }

    fn expect_byte(&mut self, expected: u8) -> ValidationResult {
        if !self.consume_byte(expected) {
            return Err(ValidationError::InvalidArgument);
        }
        Ok(())
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.peek_byte() != Some(expected) {
            return false;
        }
        self.position += 1;
        true
    }

    fn peek_byte(&self) -> Option<u8> {
        self.input.as_bytes().get(self.position).copied()
    }

    fn next_char(&mut self) -> ValidationResult<char> {
        let character = self.input[self.position..]
            .chars()
            .next()
            .ok_or(ValidationError::InvalidArgument)?;
        self.position += character.len_utf8();
        Ok(character)
    }

    fn skip_whitespace(&mut self) {
        while self
            .peek_byte()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.position += 1;
        }
    }
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
