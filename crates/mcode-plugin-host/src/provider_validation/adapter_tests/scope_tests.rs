//! System-message sequence and lexical-scope behavior tests.
use super::super::adapter::types::{
    AdapterCollection, AdapterContractV1, AdapterModelSource, AdapterPresence, AdapterScalarSource,
    AdapterTransform, AdapterVariantSource, AdapterWireId, ContractTree, PathSegment,
    TypedJsonConstant,
};
use super::super::adapter::validate_contract;
use super::fixtures::{ContractBuilder, decoder_for};

#[test]
fn system_messages_are_enabled_only_for_the_two_frozen_wires() {
    for wire in [
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
    ] {
        assert_eq!(
            validate_contract(&system_messages_contract(wire, false)).is_ok(),
            matches!(
                wire,
                AdapterWireId::OpenAiCompletions | AdapterWireId::MistralConversations
            ),
            "{wire:?}"
        );
    }
}

#[test]
fn system_message_entry_cases_have_disjoint_lexical_scopes() {
    assert!(
        validate_contract(&system_messages_contract(
            AdapterWireId::OpenAiCompletions,
            false
        ))
        .is_ok()
    );
    assert!(
        validate_contract(&system_messages_contract(
            AdapterWireId::OpenAiCompletions,
            true
        ))
        .is_err()
    );
}

fn system_messages_contract(wire: AdapterWireId, cross_cases: bool) -> AdapterContractV1 {
    let mut builder = ContractBuilder::default();
    let system = builder.value(
        None,
        AdapterPresence::Required,
        None,
        AdapterScalarSource::SystemItem,
        AdapterTransform::Identity,
    );
    let user = builder.constant(
        None,
        AdapterPresence::Required,
        None,
        TypedJsonConstant::Null,
    );
    let assistant = builder.constant(
        None,
        AdapterPresence::Required,
        None,
        TypedJsonConstant::Null,
    );
    let result = if wire == AdapterWireId::MistralConversations {
        let content = builder.value(
            Some(PathSegment::Key("content".to_owned())),
            AdapterPresence::Required,
            None,
            AdapterScalarSource::MistralToolResultContent,
            AdapterTransform::MistralToolResultContent,
        );
        builder.object(None, AdapterPresence::Required, None, vec![content])
    } else {
        builder.constant(
            None,
            AdapterPresence::Required,
            None,
            TypedJsonConstant::Null,
        )
    };
    let message = builder.switch(
        None,
        AdapterPresence::Required,
        None,
        AdapterVariantSource::Message,
        vec![user, assistant, result],
    );
    let cases = if cross_cases {
        vec![message, system]
    } else {
        vec![system, message]
    };
    let item = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::SystemMessageEntry,
        cases,
    );
    let sequence = builder.array(
        Some(PathSegment::Key("entries".to_owned())),
        AdapterCollection::SystemMessages,
        item,
        0,
        5_120,
    );
    let root = builder.object(None, AdapterPresence::Required, None, vec![sequence]);
    AdapterContractV1 {
        version: 1,
        wire_id: wire,
        model_source: AdapterModelSource::CurrentModel,
        tree: ContractTree {
            root,
            nodes: builder.nodes,
            tables: vec![],
        },
        ordinary_header_rules: vec![],
        decoder_kind: decoder_for(wire),
    }
}
