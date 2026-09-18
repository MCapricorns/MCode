//! Wire-JSON graph, grammar, charge, and serializer tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    WireJsonArray, WireJsonDocument, WireJsonField, WireJsonNode, WireJsonObject,
};

use super::ValidationError;
use super::wire_json::{canonical_serialize, is_canonical_number, validate_wire_json};

fn document(nodes: Vec<WireJsonNode>) -> WireJsonDocument {
    WireJsonDocument {
        root: u32::try_from(nodes.len().saturating_sub(1)).expect("test node count"),
        nodes,
    }
}

#[test]
fn node_count_accepts_one_and_n_and_rejects_zero_and_n_plus_one() {
    let empty = WireJsonDocument {
        root: 0,
        nodes: vec![],
    };
    assert_eq!(
        validate_wire_json(&empty, false),
        Err(ValidationError::Limit)
    );
    assert!(validate_wire_json(&super::test_support::empty_object(), true).is_ok());

    let mut maximum = vec![WireJsonNode::NullValue; 262_143];
    maximum.push(WireJsonNode::ArrayValue(WireJsonArray {
        items: (0..262_143).collect(),
    }));
    assert!(validate_wire_json(&document(maximum), false).is_ok());

    let too_many = document(vec![WireJsonNode::NullValue; 262_145]);
    assert_eq!(
        validate_wire_json(&too_many, false),
        Err(ValidationError::Limit)
    );
}

#[test]
fn graph_requires_postorder_single_parent_and_final_reachable_root() {
    let shared = document(vec![
        WireJsonNode::NullValue,
        WireJsonNode::ArrayValue(WireJsonArray { items: vec![0, 0] }),
    ]);
    assert!(validate_wire_json(&shared, false).is_err());

    let forward = document(vec![
        WireJsonNode::ArrayValue(WireJsonArray { items: vec![1] }),
        WireJsonNode::NullValue,
    ]);
    assert!(validate_wire_json(&forward, false).is_err());

    let mut wrong_root = super::test_support::empty_object();
    wrong_root.root = 1;
    assert!(validate_wire_json(&wrong_root, true).is_err());

    let orphan = document(vec![
        WireJsonNode::NullValue,
        WireJsonNode::ObjectValue(WireJsonObject { fields: vec![] }),
    ]);
    assert!(validate_wire_json(&orphan, false).is_err());
}

#[test]
fn depth_accepts_64_and_rejects_65() {
    let nested = |depth: usize| {
        let mut nodes = vec![WireJsonNode::NullValue];
        for index in 1..depth {
            nodes.push(WireJsonNode::ArrayValue(WireJsonArray {
                items: vec![u32::try_from(index - 1).expect("test index")],
            }));
        }
        document(nodes)
    };
    assert_eq!(
        validate_wire_json(&nested(64), false)
            .expect("depth 64")
            .depth,
        64
    );
    assert_eq!(
        validate_wire_json(&nested(65), false),
        Err(ValidationError::Limit)
    );
}

#[test]
fn number_tokens_use_exact_canonical_grammar_without_float_conversion() {
    for valid in ["0", "1", "-1", "1.01", "-0.1", "1e1", "1e-10", "1.2e3"] {
        assert!(is_canonical_number(valid), "{valid}");
    }
    for invalid in [
        "", "-0", "00", "01", "+1", "1.", "1.0", "1E1", "1e0", "1e+1", "NaN", "inf",
    ] {
        assert!(!is_canonical_number(invalid), "{invalid}");
    }
    assert!(is_canonical_number(&"1".repeat(128)));
    assert!(!is_canonical_number(&"1".repeat(129)));
}

#[test]
fn object_keys_are_safe_sorted_and_unique() {
    let valid = document(vec![
        WireJsonNode::NullValue,
        WireJsonNode::BooleanValue(true),
        WireJsonNode::ObjectValue(WireJsonObject {
            fields: vec![
                WireJsonField {
                    key: "a".to_owned(),
                    value: 0,
                },
                WireJsonField {
                    key: "b".to_owned(),
                    value: 1,
                },
            ],
        }),
    ]);
    assert!(validate_wire_json(&valid, true).is_ok());

    let mut reversed = valid.clone();
    let WireJsonNode::ObjectValue(root) = reversed.nodes.last_mut().expect("root") else {
        panic!("object root")
    };
    root.fields.reverse();
    assert!(validate_wire_json(&reversed, true).is_err());

    let mut duplicate = valid.clone();
    let WireJsonNode::ObjectValue(root) = duplicate.nodes.last_mut().expect("root") else {
        panic!("object root")
    };
    root.fields[1].key = "a".to_owned();
    assert!(validate_wire_json(&duplicate, true).is_err());

    let mut oversized = valid;
    let WireJsonNode::ObjectValue(root) = oversized.nodes.last_mut().expect("root") else {
        panic!("object root")
    };
    root.fields[1].key = "x".repeat(257);
    assert!(validate_wire_json(&oversized, true).is_err());
}

#[test]
fn ordinary_strings_enforce_safe_64_kib_and_document_charge() {
    let maximum = document(vec![WireJsonNode::StringValue("x".repeat(65_536))]);
    assert!(validate_wire_json(&maximum, false).is_ok());
    let too_long = document(vec![WireJsonNode::StringValue("x".repeat(65_537))]);
    assert!(validate_wire_json(&too_long, false).is_err());
    let bidi = document(vec![WireJsonNode::StringValue("bad\u{202e}".to_owned())]);
    assert!(validate_wire_json(&bidi, false).is_err());

    let mut charged = vec![WireJsonNode::StringValue("x".repeat(65_536)); 128];
    charged.push(WireJsonNode::ArrayValue(WireJsonArray {
        items: (0..128).collect(),
    }));
    assert_eq!(
        validate_wire_json(&document(charged), false),
        Err(ValidationError::Limit)
    );
}

#[test]
fn canonical_serializer_preserves_order_and_number_and_uses_exact_escapes() {
    let value = document(vec![
        WireJsonNode::NumberValue("1.02e3".to_owned()),
        WireJsonNode::StringValue("q\"\\/\t\n雪".to_owned()),
        WireJsonNode::ObjectValue(WireJsonObject {
            fields: vec![
                WireJsonField {
                    key: "a".to_owned(),
                    value: 0,
                },
                WireJsonField {
                    key: "b".to_owned(),
                    value: 1,
                },
            ],
        }),
    ]);
    let first = canonical_serialize(&value).expect("canonical JSON");
    let second = canonical_serialize(&value).expect("deterministic JSON");
    assert_eq!(
        first,
        "{\"a\":1.02e3,\"b\":\"q\\\"\\\\/\\t\\n雪\"}".as_bytes()
    );
    assert_eq!(first, second);
}

#[test]
fn canonical_serializer_accepts_empty_object_and_exact_body_cap() {
    const MAX_BODY: usize = 8 * 1_024 * 1_024;

    assert_eq!(
        canonical_serialize(&super::test_support::empty_object()).expect("empty object"),
        b"{}"
    );

    let exact = escaped_body_boundary_document();
    assert_eq!(
        canonical_serialize(&exact)
            .expect("exact canonical body boundary")
            .len(),
        MAX_BODY
    );

    let mut over = exact;
    let WireJsonNode::StringValue(value) = &mut over.nodes[0] else {
        panic!("boundary string")
    };
    value.push('x');
    assert_eq!(canonical_serialize(&over), Err(ValidationError::Limit));
}

#[test]
fn prepared_body_requires_object_root() {
    let scalar = document(vec![WireJsonNode::NullValue]);
    assert!(validate_wire_json(&scalar, false).is_ok());
    assert!(validate_wire_json(&scalar, true).is_err());
}

fn escaped_body_boundary_document() -> WireJsonDocument {
    let mut nodes = Vec::new();
    for index in 0..65 {
        let length = if index < 13 { 64_527 } else { 64_526 };
        nodes.push(WireJsonNode::StringValue("\"".repeat(length)));
    }
    nodes.push(WireJsonNode::ArrayValue(WireJsonArray {
        items: (0..65).collect(),
    }));
    nodes.push(WireJsonNode::ObjectValue(WireJsonObject {
        fields: vec![WireJsonField {
            key: "v".to_owned(),
            value: 65,
        }],
    }));
    document(nodes)
}
