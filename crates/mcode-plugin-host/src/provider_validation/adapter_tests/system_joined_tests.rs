//! End-to-end system collection and join-composite alternatives.
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
fn system_joined_materializes_exact_declaration_order_lf_bytes() {
    let fixture = joined_fixture(SystemProjection::Joined);
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
fn system_collection_and_joined_are_exclusive_and_complete() {
    for (projection, expected) in [
        (
            SystemProjection::Both,
            AdapterValidationError::InvalidContract,
        ),
        (
            SystemProjection::Neither,
            AdapterValidationError::SourceMismatch,
        ),
    ] {
        let fixture = joined_fixture(projection);
        assert_eq!(
            validate_adapter(
                &fixture.contract,
                fixture.selected(),
                &fixture.original,
                &fixture.body,
                &fixture.headers,
            ),
            Err(expected),
            "{projection:?}"
        );
    }
}

#[derive(Debug, Clone, Copy)]
enum SystemProjection {
    Joined,
    Both,
    Neither,
}

fn joined_fixture(projection: SystemProjection) -> DummyFixture {
    let mut original = super::super::test_support::prepare_input();
    original.system = vec!["alpha".to_owned(), String::new(), "omega".to_owned()];
    let expected = object(vec![
        ("messages", AdapterJson::Array(vec![])),
        ("model", string("model")),
        ("system", string("alpha\n\nomega")),
        ("tools", AdapterJson::Array(vec![])),
    ]);
    DummyFixture {
        contract: joined_contract(projection),
        entry: super::super::test_support::catalog_entry("model"),
        original,
        body: wire_document(&expected),
        headers: vec![],
    }
}

fn joined_contract(projection: SystemProjection) -> AdapterContractV1 {
    let mut builder = ContractBuilder::default();
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
    let message_cases = (0..3).map(|_| constant(&mut builder, None)).collect();
    let message_item = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::Message,
        message_cases,
    );
    let messages = builder.array(
        Some(key("messages")),
        AdapterCollection::Messages,
        message_item,
        0,
        4_096,
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
    let reasoning = omitted(
        &mut builder,
        "reasoning",
        AdapterPresence::OmitForUnset,
        AdapterPresenceSource::Reasoning,
    );

    let mut system = Vec::new();
    if matches!(projection, SystemProjection::Both) {
        let item = value(
            &mut builder,
            Some(PathSegment::ArrayItem),
            AdapterScalarSource::SystemItem,
            AdapterTransform::Identity,
        );
        system.push(builder.array(
            Some(key("systemArray")),
            AdapterCollection::System,
            item,
            0,
            1_024,
        ));
    }
    if matches!(
        projection,
        SystemProjection::Joined | SystemProjection::Both
    ) {
        let name = if matches!(projection, SystemProjection::Both) {
            "systemJoined"
        } else {
            "system"
        };
        system.push(value(
            &mut builder,
            Some(key(name)),
            AdapterScalarSource::SystemJoined,
            AdapterTransform::JoinLf,
        ));
    }

    let tool_choice = omitted(
        &mut builder,
        "toolChoice",
        AdapterPresence::OmitForUnset,
        AdapterPresenceSource::ToolChoice,
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
    let tool = builder.object(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        vec![description, name, schema],
    );
    let tools = builder.array(Some(key("tools")), AdapterCollection::Tools, tool, 0, 1_024);

    let mut children = vec![cache, max_output, messages, model, reasoning];
    children.extend(system);
    children.extend([tool_choice, tools]);
    let root = builder.object(None, AdapterPresence::Required, None, children);
    AdapterContractV1 {
        version: 1,
        wire_id: AdapterWireId::OpenAiResponses,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root,
            nodes: builder.nodes,
            tables: vec![],
        },
        ordinary_header_rules: vec![],
        decoder_kind: decoder_for(AdapterWireId::OpenAiResponses),
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

fn constant(builder: &mut ContractBuilder, segment: Option<PathSegment>) -> u32 {
    builder.constant(
        segment,
        AdapterPresence::Required,
        None,
        TypedJsonConstant::Null,
    )
}

fn key(value: &str) -> PathSegment {
    PathSegment::Key(value.to_owned())
}

fn string(value: &str) -> AdapterJson {
    AdapterJson::ordinary_string(value)
}

fn object(fields: Vec<(&str, AdapterJson)>) -> AdapterJson {
    AdapterJson::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}
