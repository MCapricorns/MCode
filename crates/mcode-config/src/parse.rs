//! Strict, bounded JSON parsing with duplicate-key detection.

use std::collections::BTreeSet;
use std::fmt::{self, Formatter};

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

use crate::{ConfigError, ConfigErrorKind};

/// Bounds one strict authority document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParseLimits {
    /// Maximum nesting depth, counting the document root as depth one.
    pub(crate) max_depth: usize,
    /// Maximum JSON values plus object member names in one document.
    pub(crate) max_nodes: usize,
}

#[derive(Debug, Clone, Copy)]
enum ParseViolationKind {
    TooDeep,
    TooManyNodes,
    DuplicateKey,
}

struct ParseState {
    limits: ParseLimits,
    nodes: usize,
    violation: Option<ParseViolationKind>,
}

impl ParseState {
    fn enter_value<E: de::Error>(&mut self, depth: usize) -> Result<(), E> {
        if depth > self.limits.max_depth {
            return Err(self.reject(ParseViolationKind::TooDeep));
        }
        self.count_node()
    }

    fn count_node<E: de::Error>(&mut self) -> Result<(), E> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or_else(|| self.reject::<E>(ParseViolationKind::TooManyNodes))?;
        if self.nodes > self.limits.max_nodes {
            return Err(self.reject(ParseViolationKind::TooManyNodes));
        }
        Ok(())
    }

    fn reject<E: de::Error>(&mut self, kind: ParseViolationKind) -> E {
        if self.violation.is_none() {
            self.violation = Some(kind);
        }
        E::custom("strict configuration JSON rejected")
    }
}

struct ValueSeed<'state> {
    state: &'state mut ParseState,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        self.state.enter_value(self.depth)?;
        deserializer.deserialize_any(ValueVisitor {
            state: self.state,
            depth: self.depth,
        })
    }
}

struct ValueVisitor<'state> {
    state: &'state mut ParseState,
    depth: usize,
}

impl<'de> Visitor<'de> for ValueVisitor<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("a strict JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let child_depth = self
            .depth
            .checked_add(1)
            .ok_or_else(|| self.state.reject::<A::Error>(ParseViolationKind::TooDeep))?;
        let capacity = sequence
            .size_hint()
            .unwrap_or(0)
            .min(self.state.limits.max_nodes);
        let mut values = Vec::with_capacity(capacity);
        loop {
            let seed = ValueSeed {
                state: self.state,
                depth: child_depth,
            };
            match sequence.next_element_seed(seed)? {
                Some(value) => values.push(value),
                None => break,
            }
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let child_depth = self
            .depth
            .checked_add(1)
            .ok_or_else(|| self.state.reject::<A::Error>(ParseViolationKind::TooDeep))?;
        let mut keys = BTreeSet::new();
        let mut values = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            self.state.count_node::<A::Error>()?;
            if !keys.insert(key.clone()) {
                return Err(self.state.reject(ParseViolationKind::DuplicateKey));
            }
            let value = access.next_value_seed(ValueSeed {
                state: self.state,
                depth: child_depth,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

/// Parses one complete strict JSON value.
pub(crate) fn parse_strict_value(bytes: &[u8], limits: ParseLimits) -> Result<Value, ConfigError> {
    if std::str::from_utf8(bytes).is_err() {
        return Err(ConfigError::new(ConfigErrorKind::NonUtf8));
    }

    let mut state = ParseState {
        limits,
        nodes: 0,
        violation: None,
    };
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let parsed = ValueSeed {
        state: &mut state,
        depth: 1,
    }
    .deserialize(&mut deserializer);

    let value = match parsed {
        Ok(value) => value,
        Err(_) => return Err(classify_parse_failure(state.violation)),
    };
    if deserializer.end().is_err() {
        return Err(ConfigError::new(ConfigErrorKind::InvalidJson));
    }
    Ok(value)
}

fn classify_parse_failure(violation: Option<ParseViolationKind>) -> ConfigError {
    let Some(violation) = violation else {
        return ConfigError::new(ConfigErrorKind::InvalidJson);
    };
    let kind = match violation {
        ParseViolationKind::TooDeep => ConfigErrorKind::TooDeep,
        ParseViolationKind::TooManyNodes => ConfigErrorKind::TooManyNodes,
        ParseViolationKind::DuplicateKey => ConfigErrorKind::DuplicateKey,
    };
    ConfigError::new(kind)
}

#[cfg(test)]
mod tests {
    use super::{ParseLimits, parse_strict_value};
    use crate::ConfigErrorKind;

    const BOUNDARY_LIMITS: ParseLimits = ParseLimits {
        max_depth: 3,
        max_nodes: 16,
    };

    #[test]
    fn depth_limit_counts_the_document_root_as_one() {
        parse_strict_value(b"[[null]]", BOUNDARY_LIMITS).expect("depth three");

        let error = parse_strict_value(b"[[[null]]]", BOUNDARY_LIMITS)
            .expect_err("depth four must be rejected");
        assert_eq!(error.kind(), ConfigErrorKind::TooDeep);
    }
}
