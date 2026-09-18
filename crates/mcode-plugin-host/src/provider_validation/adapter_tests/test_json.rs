//! Wire-JSON construction from hand-authored test expectations.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    WireJsonArray, WireJsonDocument, WireJsonField, WireJsonNode, WireJsonObject,
};

use super::super::adapter::json::AdapterJson;

pub(super) fn wire_document(value: &AdapterJson) -> WireJsonDocument {
    let mut nodes = Vec::new();
    let root = append(value, &mut nodes);
    WireJsonDocument { root, nodes }
}

fn append(value: &AdapterJson, nodes: &mut Vec<WireJsonNode>) -> u32 {
    let node = match value {
        AdapterJson::Null => WireJsonNode::NullValue,
        AdapterJson::Boolean(value) => WireJsonNode::BooleanValue(*value),
        AdapterJson::Number(value) => WireJsonNode::NumberValue(value.clone()),
        AdapterJson::String { value, .. } => WireJsonNode::StringValue(value.clone()),
        AdapterJson::Array(items) => {
            let items = items.iter().map(|item| append(item, nodes)).collect();
            WireJsonNode::ArrayValue(WireJsonArray { items })
        }
        AdapterJson::Object(fields) => {
            let fields = fields
                .iter()
                .map(|(key, value)| WireJsonField {
                    key: key.clone(),
                    value: append(value, nodes),
                })
                .collect();
            WireJsonNode::ObjectValue(WireJsonObject { fields })
        }
    };
    let index = u32::try_from(nodes.len()).expect("test wire node count");
    nodes.push(node);
    index
}
