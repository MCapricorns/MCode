//! Static semantic regressions for audited contract paths.
use super::super::adapter::types::{
    AdapterCollection, AdapterContractV1, AdapterModelSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterVariantSource,
    AdapterWireId, ContractTree, PathSegment, TypedJsonConstant,
};
use super::super::adapter::validate_contract;
use super::exhaustive_fixture::exhaustive_fixture;
use super::fixtures::{ContractBuilder, decoder_for};

#[test]
fn second_system_messages_occurrence_is_statically_rejected() {
    let mut builder = ContractBuilder::default();
    let first = system_messages(&mut builder, "a");
    let second = system_messages(&mut builder, "b");
    let contract = contract(builder, vec![first, second]);
    assert!(validate_contract(&contract).is_err());
}

#[test]
fn system_messages_rejects_every_separate_system_projection() {
    for extra in [
        ExtraSystemProjection::System,
        ExtraSystemProjection::Messages,
        ExtraSystemProjection::Joined,
    ] {
        for sequence_first in [true, false] {
            let mut builder = ContractBuilder::default();
            let sequence = system_messages(&mut builder, if sequence_first { "a" } else { "b" });
            let extra_node = extra.build(&mut builder, if sequence_first { "b" } else { "a" });
            let children = if sequence_first {
                vec![sequence, extra_node]
            } else {
                vec![extra_node, sequence]
            };
            let contract = contract(builder, children);
            assert!(
                validate_contract(&contract).is_err(),
                "{extra:?} sequence_first={sequence_first}"
            );
        }
    }
}

#[test]
fn duplicate_nested_presence_source_is_rejected_on_one_active_path() {
    let mut builder = ContractBuilder::default();
    let inner = builder.constant(
        Some(key("inner")),
        AdapterPresence::OmitIfNone,
        Some(AdapterPresenceSource::MaxOutput),
        TypedJsonConstant::Null,
    );
    let outer = builder.object(
        Some(key("outer")),
        AdapterPresence::OmitIfNone,
        Some(AdapterPresenceSource::MaxOutput),
        vec![inner],
    );
    assert!(validate_contract(&contract(builder, vec![outer])).is_err());
}

#[test]
fn reasoning_value_wrappers_require_the_enabled_reasoning_case() {
    for source in [
        AdapterPresenceSource::ReasoningEffort,
        AdapterPresenceSource::ReasoningBudget,
    ] {
        for ordinal in [0, 1] {
            assert!(
                validate_contract(&reasoning_wrapper_contract(source, ordinal)).is_err(),
                "{source:?} ordinal={ordinal}"
            );
        }
        assert!(
            validate_contract(&reasoning_wrapper_contract(source, 2)).is_ok(),
            "{source:?} enabled"
        );
    }

    let valid = exhaustive_fixture(true);
    assert!(validate_contract(&valid.contract).is_ok());
}

fn reasoning_wrapper_contract(
    source: AdapterPresenceSource,
    wrapper_ordinal: usize,
) -> AdapterContractV1 {
    let mut builder = ContractBuilder::default();
    let cases = (0..3)
        .map(|ordinal| {
            if ordinal == wrapper_ordinal {
                builder.constant(
                    None,
                    AdapterPresence::OmitIfNone,
                    Some(source),
                    TypedJsonConstant::Null,
                )
            } else {
                constant(&mut builder, None)
            }
        })
        .collect();
    let reasoning = builder.switch(
        Some(key("reasoning")),
        AdapterPresence::OmitForUnset,
        Some(AdapterPresenceSource::Reasoning),
        AdapterVariantSource::Reasoning,
        cases,
    );
    contract(builder, vec![reasoning])
}

#[derive(Debug, Clone, Copy)]
enum ExtraSystemProjection {
    System,
    Messages,
    Joined,
}

impl ExtraSystemProjection {
    fn build(self, builder: &mut ContractBuilder, name: &str) -> u32 {
        match self {
            Self::System => {
                let item = builder.value(
                    Some(PathSegment::ArrayItem),
                    AdapterPresence::Required,
                    None,
                    AdapterScalarSource::SystemItem,
                    AdapterTransform::Identity,
                );
                builder.array(Some(key(name)), AdapterCollection::System, item, 0, 1_024)
            }
            Self::Messages => {
                let cases = (0..3).map(|_| constant(builder, None)).collect::<Vec<_>>();
                let item = builder.switch(
                    Some(PathSegment::ArrayItem),
                    AdapterPresence::Required,
                    None,
                    AdapterVariantSource::Message,
                    cases,
                );
                builder.array(Some(key(name)), AdapterCollection::Messages, item, 0, 4_096)
            }
            Self::Joined => builder.value(
                Some(key(name)),
                AdapterPresence::Required,
                None,
                AdapterScalarSource::SystemJoined,
                AdapterTransform::JoinLf,
            ),
        }
    }
}

fn system_messages(builder: &mut ContractBuilder, name: &str) -> u32 {
    let system = builder.value(
        None,
        AdapterPresence::Required,
        None,
        AdapterScalarSource::SystemItem,
        AdapterTransform::Identity,
    );
    let message_cases = (0..3).map(|_| constant(builder, None)).collect::<Vec<_>>();
    let message = builder.switch(
        None,
        AdapterPresence::Required,
        None,
        AdapterVariantSource::Message,
        message_cases,
    );
    let item = builder.switch(
        Some(PathSegment::ArrayItem),
        AdapterPresence::Required,
        None,
        AdapterVariantSource::SystemMessageEntry,
        vec![system, message],
    );
    builder.array(
        Some(key(name)),
        AdapterCollection::SystemMessages,
        item,
        0,
        5_120,
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

fn contract(mut builder: ContractBuilder, children: Vec<u32>) -> AdapterContractV1 {
    let root = builder.object(None, AdapterPresence::Required, None, children);
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

fn key(value: &str) -> PathSegment {
    PathSegment::Key(value.to_owned())
}
