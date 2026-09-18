//! Prepare-input reducer, capability, sidecar-local, and header tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, AssistantMessage, CapabilitySupport, EnabledReasoning, ImageMediaType,
    ImageMetadata, ImageView, Message, OrdinaryHeader, PreparedRequest, Reasoning, ReasoningBlock,
    ReasoningEffort, ReasoningKind, ReasoningProofView, SpecificToolChoice, TextBlock,
    ToolCallBlock, ToolChoice, ToolDefinition, ToolResultBlock, ToolResultMessage, UserBlock,
    UserMessage,
};

use super::ValidationError;
use super::prepare::{
    ToolResultStatus, validate_ordinary_headers, validate_prepare_input, validate_prepared_request,
};
use super::test_support::{catalog_entry, empty_object, prepare_input, selected};

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: format!("Use {name}"),
        input_schema: empty_object(),
    }
}

fn text(value: &str) -> TextBlock {
    TextBlock {
        text: value.to_owned(),
    }
}

fn call(call_id: &str, name: &str) -> AssistantBlock {
    AssistantBlock::ToolCall(ToolCallBlock {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        arguments: empty_object(),
    })
}

fn result(call_id: &str) -> Message {
    Message::ToolResult(ToolResultMessage {
        call_id: call_id.to_owned(),
        blocks: vec![ToolResultBlock::Text(text("result"))],
        is_error: false,
    })
}

#[test]
fn minimal_prepare_input_is_valid_and_crossed_identity_is_rejected() {
    let entry = catalog_entry("model");
    let mut input = prepare_input();
    assert!(validate_prepare_input(&input, selected(&entry)).is_ok());

    input.catalog_digest = super::test_support::OTHER_DIGEST.to_owned();
    assert!(validate_prepare_input(&input, selected(&entry)).is_err());
}

#[test]
fn message_reducer_requires_immediate_ordered_nonempty_results() {
    let entry = catalog_entry("model");
    let mut input = prepare_input();
    input.tools = vec![tool("alpha"), tool("beta")];
    input.messages = vec![
        Message::Assistant(AssistantMessage {
            blocks: vec![call("call-1", "alpha"), call("call-2", "beta")],
        }),
        result("call-1"),
        result("call-2"),
    ];
    let Message::ToolResult(second_result) = &mut input.messages[2] else {
        panic!("tool result")
    };
    second_result.is_error = true;
    let validated = validate_prepare_input(&input, selected(&entry)).expect("ordered results");
    assert_eq!(validated.matched_tool_results.len(), 2);
    let first = validated
        .matched_tool_results
        .get("call-1")
        .expect("first matched result");
    let second = validated
        .matched_tool_results
        .get("call-2")
        .expect("second matched result");
    assert_eq!(first.call_id, "call-1");
    assert_eq!(first.name, "alpha");
    assert_eq!(first.status, ToolResultStatus::Success);
    assert_eq!(second.status, ToolResultStatus::Error);

    let mut crossed = input.clone();
    crossed.messages.swap(1, 2);
    assert!(validate_prepare_input(&crossed, selected(&entry)).is_err());

    let mut missing = input.clone();
    missing.messages.pop();
    assert!(validate_prepare_input(&missing, selected(&entry)).is_err());

    let mut interrupted = input.clone();
    interrupted.messages[1] = Message::User(UserMessage {
        blocks: vec![UserBlock::Text(text("interrupt"))],
    });
    assert!(validate_prepare_input(&interrupted, selected(&entry)).is_err());

    let mut empty = input;
    let Message::ToolResult(result) = &mut empty.messages[1] else {
        panic!("tool result")
    };
    result.blocks.clear();
    assert!(validate_prepare_input(&empty, selected(&entry)).is_err());
}

#[test]
fn tool_definition_names_and_call_ids_are_globally_unique() {
    let entry = catalog_entry("model");
    let mut duplicate_tools = prepare_input();
    duplicate_tools.tools = vec![tool("alpha"), tool("alpha")];
    assert!(validate_prepare_input(&duplicate_tools, selected(&entry)).is_err());

    let mut duplicate_calls = prepare_input();
    duplicate_calls.tools = vec![tool("alpha")];
    duplicate_calls.messages = vec![
        Message::Assistant(AssistantMessage {
            blocks: vec![call("call-1", "alpha")],
        }),
        result("call-1"),
        Message::Assistant(AssistantMessage {
            blocks: vec![call("call-1", "alpha")],
        }),
        result("call-1"),
    ];
    assert!(validate_prepare_input(&duplicate_calls, selected(&entry)).is_err());

    let mut unknown = prepare_input();
    unknown.tools = vec![tool("alpha")];
    unknown.messages = vec![Message::Assistant(AssistantMessage {
        blocks: vec![call("call-1", "beta")],
    })];
    assert!(validate_prepare_input(&unknown, selected(&entry)).is_err());
}

#[test]
fn collection_zero_and_one_boundaries_use_the_prepare_admission_path() {
    let entry = catalog_entry("model");
    let empty = prepare_input();
    assert!(validate_prepare_input(&empty, selected(&entry)).is_ok());

    let mut one_system = prepare_input();
    one_system.system.push("system".to_owned());
    assert!(validate_prepare_input(&one_system, selected(&entry)).is_ok());

    let mut one_tool = prepare_input();
    one_tool.tools.push(tool("alpha"));
    assert!(validate_prepare_input(&one_tool, selected(&entry)).is_ok());

    let mut one_message = prepare_input();
    one_message.messages.push(Message::User(UserMessage {
        blocks: vec![UserBlock::Text(text("one"))],
    }));
    assert!(validate_prepare_input(&one_message, selected(&entry)).is_ok());

    let mut zero_blocks = one_message;
    let Message::User(user) = &mut zero_blocks.messages[0] else {
        panic!("user message")
    };
    user.blocks.clear();
    assert_eq!(
        validate_prepare_input(&zero_blocks, selected(&entry)),
        Err(ValidationError::InvalidArgument)
    );
}

#[test]
fn collection_n_and_n_plus_one_boundaries_use_the_prepare_admission_path() {
    let entry = catalog_entry("model");
    let mut input = prepare_input();
    input.messages = vec![Message::User(UserMessage {
        blocks: vec![UserBlock::Text(text("x")); 4_096],
    })];
    assert!(validate_prepare_input(&input, selected(&entry)).is_ok());

    let Message::User(user) = &mut input.messages[0] else {
        panic!("user message")
    };
    user.blocks.push(UserBlock::Text(text("x")));
    assert_eq!(
        validate_prepare_input(&input, selected(&entry)),
        Err(ValidationError::Limit)
    );

    let mut maximum_system = prepare_input();
    maximum_system.system = vec![String::new(); 1_024];
    assert!(validate_prepare_input(&maximum_system, selected(&entry)).is_ok());
    maximum_system.system.push(String::new());
    assert_eq!(
        validate_prepare_input(&maximum_system, selected(&entry)),
        Err(ValidationError::Limit)
    );

    let mut maximum_tools = prepare_input();
    maximum_tools.tools = (0..1_024)
        .map(|index| tool(&format!("tool-{index:04}")))
        .collect();
    assert!(validate_prepare_input(&maximum_tools, selected(&entry)).is_ok());
    maximum_tools.tools.push(tool("tool-1024"));
    assert_eq!(
        validate_prepare_input(&maximum_tools, selected(&entry)),
        Err(ValidationError::Limit)
    );

    let mut maximum_messages = prepare_input();
    maximum_messages.messages = vec![
        Message::User(UserMessage {
            blocks: vec![UserBlock::Text(text("x"))],
        });
        4_096
    ];
    assert!(validate_prepare_input(&maximum_messages, selected(&entry)).is_ok());
    maximum_messages.messages.push(Message::User(UserMessage {
        blocks: vec![UserBlock::Text(text("x"))],
    }));
    assert_eq!(
        validate_prepare_input(&maximum_messages, selected(&entry)),
        Err(ValidationError::Limit)
    );

    let mut over_charge = prepare_input();
    over_charge.system = vec!["x".repeat(65_536); 128];
    assert_eq!(
        validate_prepare_input(&over_charge, selected(&entry)),
        Err(ValidationError::Limit)
    );
}

#[test]
fn tool_choice_reasoning_and_output_limit_use_independent_capabilities() {
    let mut entry = catalog_entry("model");
    let mut input = prepare_input();
    input.tools = vec![tool("alpha")];
    input.tool_choice = ToolChoice::Auto;
    input.reasoning = Reasoning::Enabled(EnabledReasoning {
        effort: Some(ReasoningEffort::High),
        budget_tokens: Some(1),
    });
    input.max_output_tokens = Some(1_024);
    assert!(validate_prepare_input(&input, selected(&entry)).is_ok());

    input.max_output_tokens = Some(1_025);
    assert!(validate_prepare_input(&input, selected(&entry)).is_err());
    input.max_output_tokens = Some(1_024);
    input.reasoning = Reasoning::Enabled(EnabledReasoning {
        effort: None,
        budget_tokens: Some(0),
    });
    assert!(validate_prepare_input(&input, selected(&entry)).is_err());

    input.reasoning = Reasoning::Unset;
    input.tool_choice = ToolChoice::Specific(SpecificToolChoice {
        name: "alpha".to_owned(),
    });
    assert!(validate_prepare_input(&input, selected(&entry)).is_ok());
    entry.tool_capability.specific_choice = CapabilitySupport::Unknown;
    assert!(validate_prepare_input(&input, selected(&entry)).is_err());
}

#[test]
fn image_and_proof_local_bounds_do_not_create_sidecar_authority() {
    let mut entry = catalog_entry("model");
    let image = ImageView {
        stamp: "img1-0123456789abcdef0123456789abcdef".to_owned(),
        media_type: ImageMediaType::Png,
        bytes: vec![1],
        metadata: ImageMetadata {
            width: 1,
            height: 16_384,
            frames: 64,
        },
    };
    let mut input = prepare_input();
    input.messages = vec![Message::User(UserMessage {
        blocks: vec![UserBlock::Image(image)],
    })];
    assert!(validate_prepare_input(&input, selected(&entry)).is_ok());

    let Message::User(user) = &mut input.messages[0] else {
        panic!("user message")
    };
    let UserBlock::Image(image) = &mut user.blocks[0] else {
        panic!("image block")
    };
    image.bytes = vec![0; 8 * 1_024 * 1_024 + 1];
    assert_eq!(
        validate_prepare_input(&input, selected(&entry)),
        Err(ValidationError::Limit)
    );

    let proof = ReasoningProofView {
        stamp: "prf1-0123456789abcdef0123456789abcdef".to_owned(),
        source_request_id: "request-0".to_owned(),
        source_turn_id: "turn-0".to_owned(),
        source_content_index: 63,
        reasoning_kind: ReasoningKind::Thinking,
        proof: vec![1; 65_536],
    };
    let mut proof_input = prepare_input();
    proof_input.messages = vec![Message::Assistant(AssistantMessage {
        blocks: vec![AssistantBlock::Reasoning(ReasoningBlock {
            kind: ReasoningKind::Thinking,
            text: "thinking".to_owned(),
            proof: Some(proof),
        })],
    })];
    assert!(validate_prepare_input(&proof_input, selected(&entry)).is_ok());

    {
        let Message::Assistant(assistant) = &mut proof_input.messages[0] else {
            panic!("assistant message")
        };
        let AssistantBlock::Reasoning(reasoning) = &mut assistant.blocks[0] else {
            panic!("reasoning block")
        };
        reasoning.proof.as_mut().expect("proof").proof.push(1);
    }
    assert_eq!(
        validate_prepare_input(&proof_input, selected(&entry)),
        Err(ValidationError::Limit)
    );

    let Message::Assistant(assistant) = &mut proof_input.messages[0] else {
        panic!("assistant message")
    };
    let AssistantBlock::Reasoning(reasoning) = &mut assistant.blocks[0] else {
        panic!("reasoning block")
    };
    reasoning.proof.as_mut().expect("proof").proof.pop();
    entry.reasoning_capability.reasoning = CapabilitySupport::Unsupported;
    assert!(validate_prepare_input(&proof_input, selected(&entry)).is_err());
}

#[test]
fn ordinary_headers_require_lowercase_sorted_unique_allowed_values() {
    let valid = vec![
        OrdinaryHeader {
            name: "accept".to_owned(),
            value: "application/json".to_owned(),
        },
        OrdinaryHeader {
            name: "x-mode".to_owned(),
            value: "one\ttwo".to_owned(),
        },
    ];
    assert!(validate_ordinary_headers(&valid, &["x-reserved"]).is_ok());
    assert!(
        validate_ordinary_headers(
            &[OrdinaryHeader {
                name: "x".repeat(64),
                value: "v".repeat(4_096),
            }],
            &[]
        )
        .is_ok()
    );
    assert_eq!(
        validate_ordinary_headers(
            &[OrdinaryHeader {
                name: "x".to_owned(),
                value: "v".repeat(4_097),
            }],
            &[]
        ),
        Err(ValidationError::Limit)
    );

    let mut reversed = valid.clone();
    reversed.reverse();
    assert!(validate_ordinary_headers(&reversed, &[]).is_err());

    let duplicate = vec![valid[0].clone(), valid[0].clone()];
    assert!(validate_ordinary_headers(&duplicate, &[]).is_err());

    for name in [
        "Authorization",
        "authorization",
        "x-forwarded-for",
        "x-amz-date",
    ] {
        assert!(
            validate_ordinary_headers(
                &[OrdinaryHeader {
                    name: name.to_owned(),
                    value: "value".to_owned(),
                }],
                &[]
            )
            .is_err(),
            "{name}"
        );
    }
    assert!(
        validate_ordinary_headers(
            &[OrdinaryHeader {
                name: "x-mode".to_owned(),
                value: " value".to_owned(),
            }],
            &[]
        )
        .is_err()
    );

    let maximum = (0..32)
        .map(|index| OrdinaryHeader {
            name: format!("x-{index:02}"),
            value: "v".to_owned(),
        })
        .collect::<Vec<_>>();
    assert!(validate_ordinary_headers(&maximum, &[]).is_ok());
    let mut too_many = maximum;
    too_many.push(OrdinaryHeader {
        name: "x-32".to_owned(),
        value: "v".to_owned(),
    });
    assert_eq!(
        validate_ordinary_headers(&too_many, &[]),
        Err(ValidationError::Limit)
    );
}

#[test]
fn prepared_request_validates_body_and_guest_headers_only() {
    let request = PreparedRequest {
        body: empty_object(),
        ordinary_headers: vec![OrdinaryHeader {
            name: "accept".to_owned(),
            value: "application/json".to_owned(),
        }],
    };
    assert!(validate_prepared_request(&request, &[]).is_ok());
    assert!(validate_prepared_request(&request, &["accept"]).is_err());
}
