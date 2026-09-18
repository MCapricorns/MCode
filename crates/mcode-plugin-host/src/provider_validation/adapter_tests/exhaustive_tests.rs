//! Behavior coverage for the nonempty exhaustive adapter fixture.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, Message, ReasoningKind, WireJsonNode,
};

use super::super::adapter::types::AdapterValidationError;
use super::super::adapter::validate_adapter;
use super::exhaustive_fixture::exhaustive_fixture;

#[test]
fn nonempty_fixture_binds_messages_tools_images_reasoning_and_prepared_body() {
    let fixture = exhaustive_fixture(true);
    let validated = validate_adapter(
        &fixture.contract,
        fixture.selected(),
        &fixture.original,
        &fixture.body,
        &fixture.headers,
    )
    .expect("exhaustive adapter fixture");

    assert_eq!(
        validated.body_digest,
        "sha256:75b9dc7dbf83dc33f50a85702e2eaf5fdf2986412fe910e2cd939f36f5e0871f"
    );
}

#[test]
fn proof_option_wrapper_omits_only_the_bound_proof_path() {
    let present = exhaustive_fixture(true);
    let absent = exhaustive_fixture(false);
    assert!(
        validate_adapter(
            &absent.contract,
            absent.selected(),
            &absent.original,
            &absent.body,
            &absent.headers,
        )
        .is_ok()
    );
    assert_eq!(
        validate_adapter(
            &absent.contract,
            absent.selected(),
            &absent.original,
            &present.body,
            &absent.headers,
        ),
        Err(AdapterValidationError::BodyMismatch)
    );
}

#[test]
fn every_materialized_scalar_is_compared_with_its_production_projection() {
    let fixture = exhaustive_fixture(true);
    for index in 0..fixture.body.nodes.len() {
        let mut body = fixture.body.clone();
        let mutated = match &mut body.nodes[index] {
            WireJsonNode::NullValue
            | WireJsonNode::ArrayValue(_)
            | WireJsonNode::ObjectValue(_) => false,
            WireJsonNode::BooleanValue(value) => {
                *value = !*value;
                true
            }
            WireJsonNode::NumberValue(value) => {
                *value = if value == "0" { "1" } else { "0" }.to_owned();
                true
            }
            WireJsonNode::StringValue(value) => {
                value.push('x');
                true
            }
        };
        if !mutated {
            continue;
        }
        assert_eq!(
            validate_adapter(
                &fixture.contract,
                fixture.selected(),
                &fixture.original,
                &body,
                &fixture.headers,
            ),
            Err(AdapterValidationError::BodyMismatch),
            "wire node {index}"
        );
    }
}

#[test]
fn crossed_proof_sidecar_kind_is_rejected_before_expansion() {
    let fixture = exhaustive_fixture(true);
    let mut crossed = fixture.original.clone();
    let Message::Assistant(assistant) = &mut crossed.messages[1] else {
        panic!("assistant message")
    };
    let AssistantBlock::Reasoning(reasoning) = &mut assistant.blocks[1] else {
        panic!("reasoning block")
    };
    reasoning
        .proof
        .as_mut()
        .expect("proof sidecar")
        .reasoning_kind = ReasoningKind::Summary;
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &crossed,
            &fixture.body,
            &fixture.headers,
        ),
        Err(AdapterValidationError::SourceMismatch)
    );
}

#[test]
fn ordered_tool_results_are_reduced_before_contract_expansion() {
    let fixture = exhaustive_fixture(true);
    let mut crossed = fixture.original.clone();
    crossed.messages.swap(2, 3);
    assert!(matches!(crossed.messages[2], Message::ToolResult(_)));
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &crossed,
            &fixture.body,
            &fixture.headers,
        ),
        Err(AdapterValidationError::SourceMismatch)
    );
}

#[test]
fn checked_u64_sources_preserve_the_largest_constructible_value() {
    let mut fixture = exhaustive_fixture(true);
    fixture.entry.max_output_tokens = Some(u64::MAX);
    fixture.original.max_output_tokens = Some(u64::MAX);
    let crate::provider_wit::exports::mcode::provider_pack::provider_api::Reasoning::Enabled(
        enabled,
    ) = &mut fixture.original.reasoning
    else {
        panic!("enabled reasoning")
    };
    enabled.budget_tokens = Some(u64::MAX);
    for node in &mut fixture.body.nodes {
        let WireJsonNode::NumberValue(value) = node else {
            continue;
        };
        if value == "64" || value == "128" {
            *value = u64::MAX.to_string();
        }
    }
    assert!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &fixture.body,
            &fixture.headers,
        )
        .is_ok()
    );
}
