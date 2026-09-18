//! End-to-end dummy adapter mutation tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, AssistantMessage, CapabilitySupport, EnabledReasoning, ImageMediaType,
    ImageMetadata, ImageView, InputModality, Message, ModelSelection, OrdinaryHeader, PrepareInput,
    Reasoning, ReasoningBlock, ReasoningEffort, ReasoningKind, ReasoningProofView,
    SpecificToolChoice, ToolChoice, ToolDefinition, UserBlock, UserMessage, WireJsonNode,
};

use super::super::adapter::types::{
    AdapterDecoderKind, AdapterModelSource, AdapterValidationError, AdapterWireId,
    OrdinaryHeaderRule,
};
use super::super::adapter::{validate_adapter, validate_contract};
use super::fixtures::minimal_fixture;

#[test]
fn trusted_dummy_contract_binds_body_headers_and_three_digests() {
    let fixture = minimal_fixture();
    let validated = validate_adapter(
        &fixture.contract,
        fixture.selected(),
        &fixture.original,
        &fixture.body,
        &fixture.headers,
    )
    .expect("trusted dummy adapter");

    assert_eq!(validated.wire_id, AdapterWireId::AnthropicMessages);
    assert_eq!(
        validated.decoder_kind,
        AdapterDecoderKind::AnthropicMessages
    );
    assert_eq!(
        validated.body_digest,
        "sha256:5babf31530429a7d49925e00556e41cf49417de1a14c0c5245f57b8339d0da92"
    );
    assert_eq!(
        validated.ordinary_header_digest,
        "sha256:fc89e11ee25a851ca4a10348424e61d4cb7fd153cb460bae6fb100a337bb5b96"
    );
    assert_eq!(
        validated.contract_digest,
        "sha256:93714081ba242650fb87d629a603faff78d9968abab0d0c9c069f16d577a122c"
    );
}

#[test]
fn selected_and_original_identity_fields_are_bound_before_expansion() {
    let fixture = minimal_fixture();
    let mutations: [fn(&mut PrepareInput); 6] = [
        |input| input.provider_id = "other".to_owned(),
        |input| input.route_id = "other".to_owned(),
        |input| input.catalog_digest = super::super::test_support::OTHER_DIGEST.to_owned(),
        |input| input.selection = ModelSelection::Alias("model".to_owned()),
        |input| input.current_model = "other".to_owned(),
        |input| input.operation_id = "other".to_owned(),
    ];
    for mutate in mutations {
        let mut original = fixture.original.clone();
        mutate(&mut original);
        assert_eq!(
            validate_adapter(
                &fixture.contract,
                fixture.selected(),
                &original,
                &fixture.body,
                &fixture.headers,
            ),
            Err(AdapterValidationError::SourceMismatch)
        );
    }
}

#[test]
fn capability_fields_are_independent_and_fail_closed() {
    let mut fixture = minimal_fixture();
    fixture.original.system.push("system".to_owned());
    fixture
        .body
        .nodes
        .insert(4, WireJsonNode::StringValue("system".to_owned()));
    let WireJsonNode::ArrayValue(system) = &mut fixture.body.nodes[5] else {
        panic!("system array")
    };
    system.items.push(4);
    fixture.body.root += 1;
    let WireJsonNode::ObjectValue(root) = fixture.body.nodes.last_mut().expect("root") else {
        panic!("root object")
    };
    for field in &mut root.fields {
        if field.value >= 4 {
            field.value += 1;
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

    fixture.entry.input_modalities = vec![InputModality::Unknown];
    assert!(fixture.entry.input_modalities.len() == 1);
    fixture.entry.tool_capability.tools = CapabilitySupport::Unknown;
    fixture.entry.reasoning_capability.reasoning = CapabilitySupport::Unsupported;
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

#[test]
fn every_selected_capability_and_limit_is_checked_independently() {
    let mut fixture = minimal_fixture();
    let tool = ToolDefinition {
        name: "tool".to_owned(),
        description: "tool".to_owned(),
        input_schema: super::super::test_support::empty_object(),
    };

    fixture.original.tools = vec![tool.clone()];
    fixture.entry.tool_capability.tools = CapabilitySupport::Unknown;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.tools = vec![tool.clone()];
    fixture.original.tool_choice = ToolChoice::Auto;
    fixture.entry.tool_capability.auto_choice = CapabilitySupport::Unsupported;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.tool_choice = ToolChoice::None;
    fixture.entry.tool_capability.none_choice = CapabilitySupport::Unknown;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.tools = vec![tool];
    fixture.original.tool_choice = ToolChoice::Specific(SpecificToolChoice {
        name: "tool".to_owned(),
    });
    fixture.entry.tool_capability.specific_choice = CapabilitySupport::Unsupported;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.reasoning = Reasoning::Disabled;
    fixture.entry.reasoning_capability.reasoning = CapabilitySupport::Unknown;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.reasoning = Reasoning::Enabled(EnabledReasoning {
        effort: Some(ReasoningEffort::Low),
        budget_tokens: None,
    });
    fixture.entry.reasoning_capability.effort = CapabilitySupport::Unsupported;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.reasoning = Reasoning::Enabled(EnabledReasoning {
        effort: None,
        budget_tokens: Some(1),
    });
    fixture.entry.reasoning_capability.budget = CapabilitySupport::Unknown;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.messages = vec![Message::Assistant(AssistantMessage {
        blocks: vec![AssistantBlock::Reasoning(ReasoningBlock {
            kind: ReasoningKind::Thinking,
            text: "thinking".to_owned(),
            proof: Some(ReasoningProofView {
                stamp: "prf1-0123456789abcdef0123456789abcdef".to_owned(),
                source_request_id: "request-0".to_owned(),
                source_turn_id: "turn-0".to_owned(),
                source_content_index: 0,
                reasoning_kind: ReasoningKind::Thinking,
                proof: vec![1],
            }),
        })],
    })];
    fixture.entry.reasoning_capability.proof = CapabilitySupport::Unsupported;
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.messages = vec![Message::User(UserMessage {
        blocks: vec![UserBlock::Image(ImageView {
            stamp: "img1-0123456789abcdef0123456789abcdef".to_owned(),
            media_type: ImageMediaType::Png,
            bytes: vec![1],
            metadata: ImageMetadata {
                width: 1,
                height: 1,
                frames: 1,
            },
        })],
    })];
    fixture.entry.input_modalities = vec![InputModality::Text];
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.original.max_output_tokens = Some(2);
    fixture.entry.max_output_tokens = Some(1);
    assert_capability_mismatch(&fixture);

    fixture = minimal_fixture();
    fixture.entry.context_tokens = Some(0);
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &fixture.body,
            &fixture.headers,
        ),
        Err(AdapterValidationError::SourceMismatch)
    );
}

fn assert_capability_mismatch(fixture: &super::fixtures::DummyFixture) {
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &fixture.body,
            &fixture.headers,
        ),
        Err(AdapterValidationError::CapabilityMismatch)
    );
}

#[test]
fn body_and_header_mutations_are_rejected_exactly() {
    let fixture = minimal_fixture();
    let mut body = fixture.body.clone();
    let WireJsonNode::StringValue(model) = &mut body.nodes[1] else {
        panic!("model string")
    };
    *model = "other".to_owned();
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &body,
            &fixture.headers,
        ),
        Err(AdapterValidationError::BodyMismatch)
    );

    let wrong_headers = vec![OrdinaryHeader {
        name: "x-mode".to_owned(),
        value: "other".to_owned(),
    }];
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &fixture.body,
            &wrong_headers,
        ),
        Err(AdapterValidationError::HeaderMismatch)
    );
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &fixture.body,
            &[],
        ),
        Err(AdapterValidationError::HeaderMismatch)
    );
}

#[test]
fn contract_version_decoder_tree_table_and_header_order_mutations_fail() {
    let fixture = minimal_fixture();

    let mut version = fixture.contract.clone();
    version.version = 2;
    assert_eq!(
        validate_contract(&version),
        Err(AdapterValidationError::InvalidContract)
    );

    let mut decoder = fixture.contract.clone();
    decoder.decoder_kind = AdapterDecoderKind::PiMessages;
    assert_eq!(
        validate_contract(&decoder),
        Err(AdapterValidationError::InvalidContract)
    );

    let mut root = fixture.contract.clone();
    root.tree.root -= 1;
    assert_eq!(
        validate_contract(&root),
        Err(AdapterValidationError::InvalidContract)
    );

    let mut table = fixture.contract.clone();
    table.tree.tables[0].entries.swap(0, 1);
    assert_eq!(
        validate_contract(&table),
        Err(AdapterValidationError::InvalidContract)
    );

    let mut headers = fixture.contract.clone();
    headers.ordinary_header_rules.swap(0, 1);
    assert_eq!(
        validate_contract(&headers),
        Err(AdapterValidationError::InvalidContract)
    );
}

#[test]
fn fixed_headers_require_guest_absence_and_optional_one_of_is_closed() {
    let mut fixture = minimal_fixture();
    fixture.headers = vec![OrdinaryHeader {
        name: "accept".to_owned(),
        value: "application/json".to_owned(),
    }];
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &fixture.body,
            &fixture.headers,
        ),
        Err(AdapterValidationError::HeaderMismatch)
    );

    let OrdinaryHeaderRule::OneOf(rule) = &mut fixture.contract.ordinary_header_rules[1] else {
        panic!("one-of rule")
    };
    rule.required = false;
    fixture.headers.clear();
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

#[test]
fn model_source_changes_only_the_selected_model_projection() {
    let mut fixture = minimal_fixture();
    fixture.contract.model_source = AdapterModelSource::RequestedSelection;
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
