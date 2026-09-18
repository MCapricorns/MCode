//! Strict canonical tool-argument JSON tests.
use super::ValidationError;
use super::decoder::tool_json::validate_tool_arguments;

fn nested_array(depth_after_root: usize) -> String {
    format!(
        "{{\"x\":{}null{}}}",
        "[".repeat(depth_after_root),
        "]".repeat(depth_after_root)
    )
}

fn null_array(items: usize) -> String {
    let body = std::iter::repeat_n("null", items)
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"x\":[{body}]}}")
}

#[test]
fn canonical_duplicate_free_objects_are_required() {
    for accepted in [
        "{}",
        r#"{"a":1,"b":[true,false,null],"text":"line\nnext"}"#,
        r#"{"emoji":"😀"}"#,
    ] {
        assert_eq!(validate_tool_arguments(accepted), Ok(()));
    }

    for rejected in [
        "[]",
        r#"{"a":1,"a":2}"#,
        r#"{"b":1,"a":2}"#,
        r#"{ "a":1}"#,
        r#"{"a":"\u0061"}"#,
        r#"{"a":1.0}"#,
        r#"{"a":1} trailing"#,
        r#"{"a":"\uD800"}"#,
        r#"{"a":"bad\rtext"}"#,
    ] {
        assert_eq!(
            validate_tool_arguments(rejected),
            Err(ValidationError::InvalidArgument),
            "fixture: {rejected}"
        );
    }
}

#[test]
fn decoded_string_cap_rejects_the_first_byte_over_64_kib() {
    let exact = format!(r#"{{"value":"{}"}}"#, "a".repeat(65_536));
    assert_eq!(validate_tool_arguments(&exact), Ok(()));

    let oversized = format!(r#"{{"value":"{}"}}"#, "a".repeat(65_537));
    assert_eq!(
        validate_tool_arguments(&oversized),
        Err(ValidationError::Limit)
    );
}

#[test]
fn decoded_key_cap_rejects_the_first_byte_over_256() {
    let exact = format!(r#"{{"{}":null}}"#, "k".repeat(256));
    assert_eq!(validate_tool_arguments(&exact), Ok(()));

    let oversized = format!(r#"{{"{}":null}}"#, "k".repeat(257));
    assert_eq!(
        validate_tool_arguments(&oversized),
        Err(ValidationError::Limit)
    );
}

#[test]
fn decoded_bidi_control_is_rejected() {
    for value in [r#"{"value":"\u202e"}"#, "{\"value\":\"\u{202e}\"}"] {
        assert_eq!(
            validate_tool_arguments(value),
            Err(ValidationError::InvalidArgument)
        );
    }
}

#[test]
fn exponent_zero_is_rejected_but_nonzero_exponent_is_canonical() {
    assert_eq!(
        validate_tool_arguments(r#"{"value":1e0}"#),
        Err(ValidationError::InvalidArgument)
    );
    assert_eq!(validate_tool_arguments(r#"{"value":1e5}"#), Ok(()));
}

#[test]
fn escaped_keys_are_ordered_by_decoded_utf8_bytes() {
    assert_eq!(validate_tool_arguments(r##"{"\"":1,"#":2}"##), Ok(()));
    assert_eq!(
        validate_tool_arguments(r##"{"#":2,"\"":1}"##),
        Err(ValidationError::InvalidArgument)
    );
    assert_eq!(
        validate_tool_arguments(r#"{"\u0061":1}"#),
        Err(ValidationError::InvalidArgument)
    );
    assert_eq!(validate_tool_arguments(r#"{"a":1,"é":2}"#), Ok(()));
    assert_eq!(
        validate_tool_arguments(r#"{"é":2,"a":1}"#),
        Err(ValidationError::InvalidArgument)
    );
}

#[test]
fn depth_64_is_accepted_and_depth_65_is_rejected() {
    assert_eq!(validate_tool_arguments(&nested_array(62)), Ok(()));
    assert_eq!(
        validate_tool_arguments(&nested_array(63)),
        Err(ValidationError::Limit)
    );
}

#[test]
fn node_16384_is_accepted_and_node_16385_is_rejected() {
    assert_eq!(validate_tool_arguments(&null_array(16_382)), Ok(()));
    assert_eq!(
        validate_tool_arguments(&null_array(16_383)),
        Err(ValidationError::Limit)
    );
}

#[test]
fn logical_charge_and_input_bytes_are_independently_bounded() {
    let exact_input = format!(r#"{{"x":"{}"}}"#, "a".repeat(1_024 * 1_024 - 8));
    assert_eq!(
        validate_tool_arguments(&exact_input),
        Err(ValidationError::Limit)
    );

    let oversized = format!(r#"{{"x":"{}"}}"#, "a".repeat(1_024 * 1_024 - 7));
    assert_eq!(
        validate_tool_arguments(&oversized),
        Err(ValidationError::Limit)
    );
}
