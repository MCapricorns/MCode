//! Tool specification shared between the tool registry and LLM providers.

use serde::{Deserialize, Serialize};

/// Serializable description of a tool, sent to LLM providers.
///
/// Produced by the tool registry (`mcode-tools`) from `schemars`-derived
/// schemas; plugins declare tools in the same shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Tool name, unique within a registry (last registration wins).
    pub name: String,
    /// Human/model-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's arguments (`serde_json::Value`).
    pub params_schema: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_spec_roundtrip() {
        let spec = ToolSpec {
            name: "read".into(),
            description: "Read a file from disk".into(),
            params_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: ToolSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn tool_spec_preserves_arbitrary_schema() {
        // Schemas are opaque to core: any JSON must pass through verbatim.
        let schema = json!({"anyOf": [{"type": "string"}, {"enum": [1, 2.5, null, true]}]});
        let spec = ToolSpec {
            name: "t".into(),
            description: String::new(),
            params_schema: schema.clone(),
        };
        let back: ToolSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        assert_eq!(back.params_schema, schema);
    }

    #[test]
    fn tool_spec_rejects_unknown_outer_fields() {
        let encoded = json!({
            "name": "read",
            "description": "read a file",
            "params_schema": {"unknown": {"payload": true}},
            "unknown": true
        });
        assert!(serde_json::from_value::<ToolSpec>(encoded).is_err());
    }
}
