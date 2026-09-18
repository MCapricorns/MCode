//! End-to-end system-message sequence behavior tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::{
    Message, TextBlock, UserBlock, UserMessage, WireJsonNode,
};

use super::super::adapter::json::AdapterJson;
use super::super::adapter::types::{
    AdapterCollection, AdapterContractV1, AdapterModelSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterValidationError,
    AdapterVariantSource, AdapterWireId, ContractTree, PathSegment, TypedJsonConstant,
};
use super::super::adapter::validate_adapter;
use super::fixtures::{ContractBuilder, DummyFixture, decoder_for};
use super::test_json::wire_document;

#[test]
fn system_messages_materialize_system_before_messages_in_lexical_scope() {
    let fixture = sequence_fixture();
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

    let mut reversed = fixture.body.clone();
    let entries = match reversed.nodes.last() {
        Some(WireJsonNode::ObjectValue(root)) => root.fields[0].value as usize,
        _ => panic!("root object"),
    };
    let WireJsonNode::ArrayValue(entries) = &mut reversed.nodes[entries] else {
        panic!("entries array")
    };
    entries.items.swap(0, 1);
    assert_eq!(
        validate_adapter(
            &fixture.contract,
            fixture.selected(),
            &fixture.original,
            &reversed,
            &fixture.headers,
        ),
        Err(AdapterValidationError::BodyMismatch)
    );
}

fn sequence_fixture() -> DummyFixture {
    let mut original = super::super::test_support::prepare_input();
    original.system = vec!["system".to_owned()];
    original.messages = vec![Message::User(UserMessage {
        blocks: vec![UserBlock::Text(TextBlock {
            text: "user".to_owned(),
        })],
    })];
    let expected = json_object(vec![
        (
            "entries",
            AdapterJson::Array(vec![
                json_object(vec![
                    ("content", string("system")),
                    ("role", string("system")),
                ]),
                json_object(vec![("blocks", AdapterJson::Array(vec![string("user")]))]),
            ]),
        ),
        ("model", string("model")),
        ("tools", AdapterJson::Array(vec![])),
    ]);
    DummyFixture {
        contract: sequence_contract(),
        entry: super::super::test_support::catalog_entry("model"),
        original,
        body: wire_document(&expected),
        headers: vec![],
    }
}

fn sequence_contract() -> AdapterContractV1 {
    let mut builder = ContractBuilder::default();
    let content = value(
        &mut builder,
        Some(key("content")),
        AdapterScalarSource::SystemItem,
        AdapterTransform::Identity,
    );
    let role = builder.constant(
        Some(key("role")),
        AdapterPresence::Required,
        None,
        TypedJsonConstant::String("system".to_owned()),
    );
    let system = object(&mut builder, None, vec![content, role]);

    let text = value(
        &mut builder,
        None,
        AdapterScalarSource::BlockText,
        AdapterTransform::Identity,
    );
    let image = builder.constant(
        None,
        AdapterPresence::Required,
        None,
        TypedJsonConstant::Null,
    );
    let user_item = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::UserBlock,
        vec![text, image],
    );
    let user_blocks = builder.array(
        Some(key("blocks")),
        AdapterCollection::Blocks,
        user_item,
        1,
        4_096,
    );
    let user = object(&mut builder, None, vec![user_blocks]);
    let assistant = builder.constant(
        None,
        AdapterPresence::Required,
        None,
        TypedJsonConstant::Null,
    );
    let result = builder.constant(
        None,
        AdapterPresence::Required,
        None,
        TypedJsonConstant::Null,
    );
    let message = builder.switch(
        None,
        AdapterPresence::Required,
        None,
        AdapterVariantSource::Message,
        vec![user, assistant, result],
    );
    let entry = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::SystemMessageEntry,
        vec![system, message],
    );
    let entries = builder.array(
        Some(key("entries")),
        AdapterCollection::SystemMessages,
        entry,
        0,
        5_120,
    );

    let model_cases = (0..2)
        .map(|_| {
            value(
                &mut builder,
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

    let description = value(
        &mut builder,
        Some(key("description")),
        AdapterScalarSource::ToolDescription,
        AdapterTransform::Identity,
    );
    let name = value(
        &mut builder,
        Some(key("name")),
        AdapterScalarSource::ToolName,
        AdapterTransform::Identity,
    );
    let schema = value(
        &mut builder,
        Some(key("schema")),
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
            entries,
            max_output,
            model,
            reasoning,
            tool_choice,
            tools,
        ],
    );
    AdapterContractV1 {
        version: 1,
        wire_id: AdapterWireId::OpenAiCompletions,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root,
            nodes: builder.nodes,
            tables: vec![],
        },
        ordinary_header_rules: vec![],
        decoder_kind: decoder_for(AdapterWireId::OpenAiCompletions),
    }
}

fn value(
    builder: &mut ContractBuilder,
    segment: Option<PathSegment>,
    source: AdapterScalarSource,
    transform: AdapterTransform,
) -> u32 {
    builder.value(segment, AdapterPresence::Required, None, source, transform)
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

fn json_object(fields: Vec<(&str, AdapterJson)>) -> AdapterJson {
    AdapterJson::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}
