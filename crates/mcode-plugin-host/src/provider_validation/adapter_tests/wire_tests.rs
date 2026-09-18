//! Ten-wire decoder, status, name, and composite behavior tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    AssistantBlock, AssistantMessage, Message, TextBlock, ToolCallBlock, ToolDefinition,
    ToolResultBlock, ToolResultMessage,
};

use super::super::adapter::evaluate::{StatusProjection, requires_result_name, status_projection};
use super::super::adapter::json::AdapterJson;
use super::super::adapter::types::{
    AdapterCollection, AdapterContractV1, AdapterEnumSource, AdapterModelSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterVariantSource,
    AdapterWireId, ContractTree, EnumTokenEntry, EnumTokenTable, PathSegment, TypedJsonConstant,
};
use super::super::adapter::validate_adapter;
use super::fixtures::{ContractBuilder, DummyFixture, decoder_for};
use super::test_json::wire_document;

const WIRES: [AdapterWireId; 10] = [
    AdapterWireId::AnthropicMessages,
    AdapterWireId::OpenAiCompletions,
    AdapterWireId::OpenAiResponses,
    AdapterWireId::OpenAiCodexResponses,
    AdapterWireId::AzureOpenAiResponses,
    AdapterWireId::GoogleGenerativeAi,
    AdapterWireId::GoogleVertex,
    AdapterWireId::MistralConversations,
    AdapterWireId::BedrockConverseStream,
    AdapterWireId::PiMessages,
];

#[test]
fn every_wire_pair_projects_success_and_error_with_exact_policy() {
    for wire in WIRES {
        for is_error in [false, true] {
            let fixture = wire_fixture(wire, is_error);
            let validated = validate_adapter(
                &fixture.contract,
                fixture.selected(),
                &fixture.original,
                &fixture.body,
                &fixture.headers,
            )
            .unwrap_or_else(|error| panic!("{wire:?} is_error={is_error}: {error:?}"));
            assert_eq!(validated.wire_id, wire);
            assert_eq!(validated.decoder_kind, decoder_for(wire));
        }
    }
}

#[test]
fn every_wire_has_one_exact_status_name_and_composite_classification() {
    for (wire, status, name) in [
        (
            AdapterWireId::AnthropicMessages,
            StatusProjection::Boolean,
            false,
        ),
        (
            AdapterWireId::OpenAiCompletions,
            StatusProjection::None,
            false,
        ),
        (
            AdapterWireId::OpenAiResponses,
            StatusProjection::None,
            false,
        ),
        (
            AdapterWireId::OpenAiCodexResponses,
            StatusProjection::None,
            false,
        ),
        (
            AdapterWireId::AzureOpenAiResponses,
            StatusProjection::None,
            false,
        ),
        (
            AdapterWireId::GoogleGenerativeAi,
            StatusProjection::Switch,
            true,
        ),
        (AdapterWireId::GoogleVertex, StatusProjection::Switch, true),
        (
            AdapterWireId::MistralConversations,
            StatusProjection::Composite,
            true,
        ),
        (
            AdapterWireId::BedrockConverseStream,
            StatusProjection::Scalar,
            false,
        ),
        (AdapterWireId::PiMessages, StatusProjection::Boolean, true),
    ] {
        assert_eq!(status_projection(wire), status, "{wire:?}");
        assert_eq!(requires_result_name(wire), name, "{wire:?}");
    }
}

fn wire_fixture(wire: AdapterWireId, is_error: bool) -> DummyFixture {
    let mut original = super::super::test_support::prepare_input();
    original.tools = vec![ToolDefinition {
        name: "tool".to_owned(),
        description: "Use tool".to_owned(),
        input_schema: super::super::test_support::empty_object(),
    }];
    original.messages = vec![
        Message::Assistant(AssistantMessage {
            blocks: vec![AssistantBlock::ToolCall(ToolCallBlock {
                call_id: "call-1".to_owned(),
                name: "tool".to_owned(),
                arguments: super::super::test_support::empty_object(),
            })],
        }),
        Message::ToolResult(ToolResultMessage {
            call_id: "call-1".to_owned(),
            blocks: vec![ToolResultBlock::Text(TextBlock {
                text: " result ".to_owned(),
            })],
            is_error,
        }),
    ];
    let expected = wire_expected(wire, is_error);
    DummyFixture {
        contract: wire_contract(wire),
        entry: super::super::test_support::catalog_entry("model"),
        original,
        body: wire_document(&expected),
        headers: vec![],
    }
}

pub(super) fn wire_contract(wire: AdapterWireId) -> AdapterContractV1 {
    let mut builder = ContractBuilder::default();
    let mut tables = Vec::new();

    let model_cases = (0..2)
        .map(|_| {
            builder.value(
                None,
                AdapterPresence::Required,
                None,
                AdapterScalarSource::SelectedModel,
                AdapterTransform::Identity,
            )
        })
        .collect();
    let model = builder.switch(
        Some(key("model")),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::ModelSelection,
        model_cases,
    );

    let system_item = builder.value(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterScalarSource::SystemItem,
        AdapterTransform::Identity,
    );
    let system = builder.array(
        Some(key("system")),
        AdapterCollection::System,
        system_item,
        0,
        1_024,
    );

    let call_arguments = value(
        &mut builder,
        "arguments",
        AdapterScalarSource::ToolCallArguments,
        AdapterTransform::JsonSubtree,
    );
    let call_id = value(
        &mut builder,
        "callId",
        AdapterScalarSource::ToolCallId,
        AdapterTransform::Identity,
    );
    let call_name = value(
        &mut builder,
        "name",
        AdapterScalarSource::ToolCallName,
        AdapterTransform::Identity,
    );
    let call = object(&mut builder, None, vec![call_arguments, call_id, call_name]);
    let assistant_text = constant(&mut builder, None, TypedJsonConstant::Null);
    let assistant_reasoning = constant(&mut builder, None, TypedJsonConstant::Null);
    let assistant_item = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::AssistantBlock,
        vec![assistant_text, assistant_reasoning, call],
    );
    let assistant_blocks = builder.array(
        Some(key("blocks")),
        AdapterCollection::Blocks,
        assistant_item,
        1,
        4_096,
    );
    let assistant = object(&mut builder, None, vec![assistant_blocks]);

    let result_call_id = value(
        &mut builder,
        "callId",
        AdapterScalarSource::ToolResultCallId,
        AdapterTransform::Identity,
    );
    let mut result_children = Vec::new();
    if wire != AdapterWireId::MistralConversations {
        let result_text = builder.value(
            None,
            AdapterPresence::Required,
            None,
            AdapterScalarSource::BlockText,
            AdapterTransform::Identity,
        );
        let result_image = constant(&mut builder, None, TypedJsonConstant::Null);
        let result_item = builder.switch(
            Some(PathSegment::ArrayItem),
            AdapterPresence::Required,
            None,
            AdapterVariantSource::ToolResultBlock,
            vec![result_text, result_image],
        );
        let result_blocks = builder.array(
            Some(key("blocks")),
            AdapterCollection::Blocks,
            result_item,
            1,
            4_096,
        );
        result_children.push(result_blocks);
    }
    result_children.push(result_call_id);
    if wire == AdapterWireId::MistralConversations {
        result_children.push(value(
            &mut builder,
            "content",
            AdapterScalarSource::MistralToolResultContent,
            AdapterTransform::MistralToolResultContent,
        ));
    }
    if matches!(status_projection(wire), StatusProjection::Boolean) {
        result_children.push(value(
            &mut builder,
            "isError",
            AdapterScalarSource::ToolResultIsError,
            AdapterTransform::Identity,
        ));
    }
    if requires_result_name(wire) {
        result_children.push(value(
            &mut builder,
            "name",
            AdapterScalarSource::ToolResultName,
            AdapterTransform::Identity,
        ));
    }
    if status_projection(wire) == StatusProjection::Switch {
        let success = constant(
            &mut builder,
            None,
            TypedJsonConstant::String("success-branch".to_owned()),
        );
        let error = constant(
            &mut builder,
            None,
            TypedJsonConstant::String("error-branch".to_owned()),
        );
        result_children.push(builder.switch(
            Some(key("outcome")),
            AdapterPresence::Required,
            None,
            AdapterVariantSource::ToolResultStatus,
            vec![success, error],
        ));
    }
    if status_projection(wire) == StatusProjection::Scalar {
        tables.push(EnumTokenTable {
            source: AdapterEnumSource::ToolResultStatus,
            entries: vec![
                EnumTokenEntry {
                    variant_ordinal: 0,
                    token: "success".to_owned(),
                },
                EnumTokenEntry {
                    variant_ordinal: 1,
                    token: "error".to_owned(),
                },
            ],
        });
        result_children.push(value(
            &mut builder,
            "status",
            AdapterScalarSource::ToolResultStatus,
            AdapterTransform::EnumToken(0),
        ));
    }
    let result = object(&mut builder, None, result_children);
    let user = constant(&mut builder, None, TypedJsonConstant::Null);
    let message_item = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::Message,
        vec![user, assistant, result],
    );
    let messages = builder.array(
        Some(key("messages")),
        AdapterCollection::Messages,
        message_item,
        0,
        4_096,
    );

    let description = value(
        &mut builder,
        "description",
        AdapterScalarSource::ToolDescription,
        AdapterTransform::Identity,
    );
    let name = value(
        &mut builder,
        "name",
        AdapterScalarSource::ToolName,
        AdapterTransform::Identity,
    );
    let schema = value(
        &mut builder,
        "schema",
        AdapterScalarSource::ToolSchema,
        AdapterTransform::JsonSubtree,
    );
    let tool = object(
        &mut builder,
        Some(PathSegment::ArrayItem),
        vec![description, name, schema],
    );
    let tools = builder.array(Some(key("tools")), AdapterCollection::Tools, tool, 0, 1_024);

    let cache = omitted(
        &mut builder,
        "cache",
        AdapterPresence::OmitForUnset,
        AdapterPresenceSource::CacheRetention,
    );
    let max_output = omitted(
        &mut builder,
        "maxOutput",
        AdapterPresence::OmitIfNone,
        AdapterPresenceSource::MaxOutput,
    );
    let reasoning = omitted(
        &mut builder,
        "reasoning",
        AdapterPresence::OmitForUnset,
        AdapterPresenceSource::Reasoning,
    );
    let tool_choice = omitted(
        &mut builder,
        "toolChoice",
        AdapterPresence::OmitForUnset,
        AdapterPresenceSource::ToolChoice,
    );
    let root = object(
        &mut builder,
        None,
        vec![
            cache,
            max_output,
            messages,
            model,
            reasoning,
            system,
            tool_choice,
            tools,
        ],
    );
    AdapterContractV1 {
        version: 1,
        wire_id: wire,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root,
            nodes: builder.nodes,
            tables,
        },
        ordinary_header_rules: vec![],
        decoder_kind: decoder_for(wire),
    }
}

fn wire_expected(wire: AdapterWireId, is_error: bool) -> AdapterJson {
    let assistant = object_json(vec![(
        "blocks",
        AdapterJson::Array(vec![object_json(vec![
            ("arguments", object_json(vec![])),
            ("callId", string("call-1")),
            ("name", string("tool")),
        ])]),
    )]);
    let mut result = Vec::new();
    if wire != AdapterWireId::MistralConversations {
        result.push(("blocks", AdapterJson::Array(vec![string(" result ")])));
    }
    result.push(("callId", string("call-1")));
    if wire == AdapterWireId::MistralConversations {
        let text = if is_error {
            "[tool error] result"
        } else {
            "result"
        };
        result.push((
            "content",
            AdapterJson::Array(vec![object_json(vec![
                ("text", string(text)),
                ("type", string("text")),
            ])]),
        ));
    }
    if matches!(status_projection(wire), StatusProjection::Boolean) {
        result.push(("isError", AdapterJson::Boolean(is_error)));
    }
    if requires_result_name(wire) {
        result.push(("name", string("tool")));
    }
    if status_projection(wire) == StatusProjection::Switch {
        result.push((
            "outcome",
            string(if is_error {
                "error-branch"
            } else {
                "success-branch"
            }),
        ));
    }
    if status_projection(wire) == StatusProjection::Scalar {
        result.push(("status", string(if is_error { "error" } else { "success" })));
    }
    object_json(vec![
        (
            "messages",
            AdapterJson::Array(vec![assistant, object_json(result)]),
        ),
        ("model", string("model")),
        ("system", AdapterJson::Array(vec![])),
        (
            "tools",
            AdapterJson::Array(vec![object_json(vec![
                ("description", string("Use tool")),
                ("name", string("tool")),
                ("schema", object_json(vec![])),
            ])]),
        ),
    ])
}

fn value(
    builder: &mut ContractBuilder,
    name: &str,
    source: AdapterScalarSource,
    transform: AdapterTransform,
) -> u32 {
    builder.value(
        Some(key(name)),
        AdapterPresence::Required,
        None,
        source,
        transform,
    )
}

fn constant(
    builder: &mut ContractBuilder,
    segment: Option<PathSegment>,
    value: TypedJsonConstant,
) -> u32 {
    builder.constant(segment, AdapterPresence::Required, None, value)
}

fn omitted(
    builder: &mut ContractBuilder,
    name: &str,
    presence: AdapterPresence,
    source: AdapterPresenceSource,
) -> u32 {
    builder.constant(
        Some(key(name)),
        presence,
        Some(source),
        TypedJsonConstant::Null,
    )
}

fn object(builder: &mut ContractBuilder, segment: Option<PathSegment>, children: Vec<u32>) -> u32 {
    builder.object(segment, AdapterPresence::Required, None, children)
}

fn key(value: &str) -> PathSegment {
    PathSegment::Key(value.to_owned())
}

fn string(value: &str) -> AdapterJson {
    AdapterJson::ordinary_string(value)
}

fn object_json(fields: Vec<(&str, AdapterJson)>) -> AdapterJson {
    AdapterJson::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}
