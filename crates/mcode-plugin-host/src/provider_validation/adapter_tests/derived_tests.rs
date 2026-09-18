//! Checked derived-string and provenance tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    ImageMediaType, ImageMetadata, ImageView, WireJsonDocument, WireJsonField, WireJsonNode,
    WireJsonObject,
};

use super::super::adapter::json::{
    AdapterJson, canonical_wire_text, compare_and_serialize, from_wire,
};
use super::super::adapter::source::{
    data_uri, encode_bytes, mistral_text, validate_mistral_aggregate,
};
use super::super::adapter::types::{AdapterTransform, AdapterValidationError};

#[test]
fn base64_padded_and_unpadded_lengths_and_bytes_are_exact() {
    assert_eq!(
        encode_bytes(&AdapterTransform::Base64StandardPadded, &[0xff]).expect("padded"),
        AdapterJson::derived_string("/w==")
    );
    assert_eq!(
        encode_bytes(&AdapterTransform::Base64StandardUnpadded, &[0xff]).expect("unpadded"),
        AdapterJson::derived_string("/w")
    );
    assert_eq!(
        encode_bytes(&AdapterTransform::Base64StandardPadded, b"abc").expect("multiple of three"),
        AdapterJson::derived_string("YWJj")
    );
}

#[test]
fn image_data_uri_uses_every_closed_lowercase_mime() {
    for (media_type, expected) in [
        (ImageMediaType::Png, "data:image/png;base64,AQ=="),
        (ImageMediaType::Jpeg, "data:image/jpeg;base64,AQ=="),
        (ImageMediaType::Gif, "data:image/gif;base64,AQ=="),
        (ImageMediaType::Webp, "data:image/webp;base64,AQ=="),
        (ImageMediaType::Tiff, "data:image/tiff;base64,AQ=="),
    ] {
        let image = ImageView {
            stamp: "img1-0123456789abcdef0123456789abcdef".to_owned(),
            media_type,
            bytes: vec![1],
            metadata: ImageMetadata {
                width: 1,
                height: 1,
                frames: 1,
            },
        };
        assert_eq!(data_uri(&image).expect("bounded data URI"), expected);
    }
}

#[test]
fn data_uri_accepts_the_largest_padded_value_and_rejects_the_next_group() {
    let mut image = ImageView {
        stamp: "img1-0123456789abcdef0123456789abcdef".to_owned(),
        media_type: ImageMediaType::Png,
        bytes: vec![0; 6_291_438],
        metadata: ImageMetadata {
            width: 1,
            height: 1,
            frames: 1,
        },
    };
    assert_eq!(
        data_uri(&image)
            .expect("largest bounded PNG data URI")
            .len(),
        8_388_606
    );
    image.bytes.push(0);
    assert_eq!(data_uri(&image), Err(AdapterValidationError::Limit));
}

#[test]
fn mistral_text_join_trim_prefix_and_placeholders_are_exact() {
    assert_eq!(
        mistral_text(&[" a ", "b"], false, false).expect("bounded text"),
        "a \nb"
    );
    assert_eq!(
        mistral_text(&["\u{feff}\t \n"], false, true).expect("bounded text"),
        "[tool error] (no tool output)"
    );
    assert_eq!(
        mistral_text(&[], true, false).expect("bounded text"),
        "(see attached image)"
    );
    assert_eq!(
        mistral_text(&["result"], true, true).expect("bounded text"),
        "[tool error] result"
    );
}

#[test]
fn mistral_join_accepts_the_exact_derived_cap_and_rejects_the_next_byte() {
    let mut parts = vec!["x".repeat(65_536); 127];
    parts.push("x".repeat(65_409));
    let refs: Vec<_> = parts.iter().map(String::as_str).collect();
    assert_eq!(
        mistral_text(&refs, false, false)
            .expect("exact Mistral text boundary")
            .len(),
        8 * 1_024 * 1_024
    );

    parts.last_mut().expect("last part").push('x');
    let refs: Vec<_> = parts.iter().map(String::as_str).collect();
    assert_eq!(
        mistral_text(&refs, false, false),
        Err(AdapterValidationError::Limit)
    );
}

#[test]
fn mistral_composite_sizing_accepts_exact_cap_and_checks_constructible_overflows() {
    const MAX: u64 = 8 * 1_024 * 1_024;
    assert!(validate_mistral_aggregate(MAX - 64, MAX - 62, &[]).is_ok());
    assert_eq!(
        validate_mistral_aggregate(MAX - 63, MAX - 61, &[]),
        Err(AdapterValidationError::Limit)
    );
    assert!(validate_mistral_aggregate(0, 2, &[MAX - 130]).is_ok());
    assert_eq!(
        validate_mistral_aggregate(0, 2, &[MAX - 129]),
        Err(AdapterValidationError::Limit)
    );
    for (text, serialized, images) in [
        (u64::MAX, 0, Vec::new()),
        (0, u64::MAX, Vec::new()),
        (0, 0, vec![u64::MAX]),
        (1, 1, vec![u64::MAX - 35]),
        (1, 1, vec![u64::MAX - 60]),
    ] {
        assert_eq!(
            validate_mistral_aggregate(text, serialized, &images),
            Err(AdapterValidationError::Limit)
        );
    }
}

#[test]
fn canonical_arguments_text_preserves_tree_order_tokens_and_escapes() {
    let arguments = WireJsonDocument {
        root: 2,
        nodes: vec![
            WireJsonNode::NumberValue("1.02e3".to_owned()),
            WireJsonNode::StringValue("q\"\\\t\n雪".to_owned()),
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
        ],
    };
    assert_eq!(
        canonical_wire_text(&arguments).expect("canonical arguments"),
        "{\"a\":1.02e3,\"b\":\"q\\\"\\\\\\t\\n雪\"}"
    );
    let first = canonical_wire_text(&arguments).expect("first serialization");
    let second = canonical_wire_text(&arguments).expect("second serialization");
    assert_eq!(first, second);
}

#[test]
fn derived_safe_is_not_subject_to_the_ordinary_64_kib_cap() {
    let value = "x".repeat(65_537);
    let prepared = object_with_value(value.clone());
    let derived = AdapterJson::Object(vec![(
        "value".to_owned(),
        AdapterJson::derived_string(&value),
    )]);
    assert!(compare_and_serialize(&derived, &prepared).is_ok());

    let ordinary = AdapterJson::Object(vec![(
        "value".to_owned(),
        AdapterJson::ordinary_string(value),
    )]);
    assert!(compare_and_serialize(&ordinary, &prepared).is_err());
}

#[test]
fn adapter_wire_projection_requires_an_object_root() {
    let scalar = WireJsonDocument {
        root: 0,
        nodes: vec![WireJsonNode::NumberValue("1".to_owned())],
    };
    assert_eq!(
        from_wire(&scalar),
        Err(AdapterValidationError::SourceMismatch)
    );
}

#[test]
fn canonical_arguments_reject_when_enclosing_json_string_exceeds_the_cap() {
    let value = "\"".repeat(65_536);
    let mut nodes = Vec::new();
    let mut fields = Vec::new();
    for index in 0..63_u32 {
        nodes.push(WireJsonNode::StringValue(value.clone()));
        fields.push(WireJsonField {
            key: format!("k{index:02}"),
            value: index,
        });
    }
    nodes.push(WireJsonNode::ObjectValue(WireJsonObject { fields }));
    let document = WireJsonDocument { root: 63, nodes };
    assert!(matches!(
        canonical_wire_text(&document),
        Err(AdapterValidationError::Limit)
    ));
}

#[test]
fn prepared_body_serialization_accepts_empty_exact_cap_and_rejects_cap_plus_one() {
    const MAX_BODY: usize = 8 * 1_024 * 1_024;
    assert_eq!(
        compare_and_serialize(
            &AdapterJson::Object(vec![]),
            &super::super::test_support::empty_object(),
        )
        .expect("empty prepared object"),
        b"{}"
    );

    let exact_value = "\"".repeat((MAX_BODY - 12) / 2);
    let exact = AdapterJson::Object(vec![(
        "value".to_owned(),
        AdapterJson::derived_string(&exact_value),
    )]);
    let exact_prepared = object_with_value(exact_value.clone());
    assert_eq!(
        compare_and_serialize(&exact, &exact_prepared)
            .expect("exact body boundary")
            .len(),
        MAX_BODY
    );

    let over_value = format!("{exact_value}x");
    let over = AdapterJson::Object(vec![(
        "value".to_owned(),
        AdapterJson::derived_string(&over_value),
    )]);
    let over_prepared = object_with_value(over_value);
    assert_eq!(
        compare_and_serialize(&over, &over_prepared),
        Err(AdapterValidationError::Limit)
    );
}

fn object_with_value(value: String) -> WireJsonDocument {
    WireJsonDocument {
        root: 1,
        nodes: vec![
            WireJsonNode::StringValue(value),
            WireJsonNode::ObjectValue(WireJsonObject {
                fields: vec![WireJsonField {
                    key: "value".to_owned(),
                    value: 0,
                }],
            }),
        ],
    }
}
