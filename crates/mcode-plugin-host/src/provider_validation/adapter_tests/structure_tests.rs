//! Structural contract invariant mutation tests.
use super::super::adapter::types::{
    AdapterCollection, AdapterDecoderKind, AdapterEnumSource, AdapterPresence,
    AdapterPresenceSource, AdapterScalarSource, AdapterTransform, AdapterVariantSource,
    AdapterWireId, ContractArray, ContractCase, ContractNode, ContractNodeBody, ContractObject,
    ContractSwitch, ContractValue, OrdinaryHeaderRule, PathSegment, TypedJsonConstant,
};
use super::super::adapter::validate_contract;
use super::fixtures::minimal_fixture;

#[test]
fn node_parent_reference_segment_and_object_order_are_closed() {
    let fixture = minimal_fixture();

    let mut wrong_parent = fixture.contract.clone();
    wrong_parent.tree.nodes[0].parent = Some(wrong_parent.tree.root);
    assert!(validate_contract(&wrong_parent).is_err());

    let mut duplicate_reference = fixture.contract.clone();
    let ContractNodeBody::Object(root) = &mut duplicate_reference
        .tree
        .nodes
        .last_mut()
        .expect("root")
        .body
    else {
        panic!("root object")
    };
    root.children.push(root.children[0]);
    assert!(validate_contract(&duplicate_reference).is_err());

    let mut wrong_segment = fixture.contract.clone();
    wrong_segment.tree.nodes[0].segment = Some(PathSegment::ArrayItem);
    assert!(validate_contract(&wrong_segment).is_err());

    let mut reversed = fixture.contract.clone();
    let ContractNodeBody::Object(root) = &mut reversed.tree.nodes.last_mut().expect("root").body
    else {
        panic!("root object")
    };
    root.children.swap(0, 1);
    assert!(validate_contract(&reversed).is_err());
}

#[test]
fn arrays_switches_tables_presence_and_constants_reject_mutations() {
    let fixture = minimal_fixture();

    let mut array = fixture.contract.clone();
    let ContractNodeBody::Array(system) = array
        .tree
        .nodes
        .iter_mut()
        .find_map(|node| match &mut node.body {
            ContractNodeBody::Array(value) if value.collection == AdapterCollection::System => {
                Some(&mut node.body)
            }
            _ => None,
        })
        .expect("system array")
    else {
        panic!("array")
    };
    system.max = 1_025;
    assert!(validate_contract(&array).is_err());

    let mut crossed_collection = fixture.contract.clone();
    let ContractNodeBody::Array(system) = crossed_collection
        .tree
        .nodes
        .iter_mut()
        .find_map(|node| match &mut node.body {
            ContractNodeBody::Array(value) if value.collection == AdapterCollection::System => {
                Some(&mut node.body)
            }
            _ => None,
        })
        .expect("system array")
    else {
        panic!("array")
    };
    system.collection = AdapterCollection::Messages;
    assert!(validate_contract(&crossed_collection).is_err());

    let mut switch = fixture.contract.clone();
    let ContractNodeBody::Switch(model) = switch
        .tree
        .nodes
        .iter_mut()
        .find_map(|node| matches!(node.body, ContractNodeBody::Switch(_)).then_some(&mut node.body))
        .expect("model switch")
    else {
        panic!("switch")
    };
    model.cases.pop();
    assert!(validate_contract(&switch).is_err());

    let mut table = fixture.contract.clone();
    table.tree.tables[0].entries[1].token = "exact".to_owned();
    assert!(validate_contract(&table).is_err());

    let mut presence = fixture.contract.clone();
    let node = presence
        .tree
        .nodes
        .iter_mut()
        .find(|node| node.presence == AdapterPresence::OmitForUnset)
        .expect("optional node");
    node.presence_source = Some(AdapterPresenceSource::MaxOutput);
    assert!(validate_contract(&presence).is_err());

    let mut constant = fixture.contract.clone();
    let ContractNodeBody::Constant(value) = &mut constant
        .tree
        .nodes
        .iter_mut()
        .find(|node| matches!(node.body, ContractNodeBody::Constant(_)))
        .expect("constant")
        .body
    else {
        panic!("constant")
    };
    value.value = TypedJsonConstant::Number("01".to_owned());
    assert!(validate_contract(&constant).is_err());
}

#[test]
fn node_count_root_wrapper_and_table_reference_bounds_are_exact() {
    let fixture = minimal_fixture();

    let mut empty = fixture.contract.clone();
    empty.tree.nodes.clear();
    assert!(validate_contract(&empty).is_err());

    let mut too_many = fixture.contract.clone();
    too_many.tree.nodes = vec![
        ContractNode {
            parent: None,
            segment: None,
            presence: AdapterPresence::Required,
            presence_source: None,
            body: ContractNodeBody::Constant(super::super::adapter::types::ContractConstant {
                value: TypedJsonConstant::Null,
            }),
        };
        4_097
    ];
    too_many.tree.root = 4_096;
    assert!(validate_contract(&too_many).is_err());

    let mut wrong_root = fixture.contract.clone();
    wrong_root.tree.nodes.last_mut().expect("root").presence = AdapterPresence::OmitIfNone;
    wrong_root
        .tree
        .nodes
        .last_mut()
        .expect("root")
        .presence_source = Some(AdapterPresenceSource::MaxOutput);
    assert!(validate_contract(&wrong_root).is_err());

    let mut unused_table = fixture.contract.clone();
    unused_table
        .tree
        .tables
        .push(unused_table.tree.tables[0].clone());
    assert!(validate_contract(&unused_table).is_err());
}

#[test]
fn header_rules_deny_reserved_names_and_enforce_value_sets() {
    let fixture = minimal_fixture();

    let mut denied = fixture.contract.clone();
    let OrdinaryHeaderRule::Fixed(rule) = &mut denied.ordinary_header_rules[0] else {
        panic!("fixed")
    };
    rule.name = "authorization".to_owned();
    assert!(validate_contract(&denied).is_err());

    let mut empty_values = fixture.contract.clone();
    let OrdinaryHeaderRule::OneOf(rule) = &mut empty_values.ordinary_header_rules[1] else {
        panic!("one-of")
    };
    rule.values.clear();
    assert!(validate_contract(&empty_values).is_err());

    let mut duplicate_values = fixture.contract.clone();
    let OrdinaryHeaderRule::OneOf(rule) = &mut duplicate_values.ordinary_header_rules[1] else {
        panic!("one-of")
    };
    rule.values[1] = rule.values[0].clone();
    assert!(validate_contract(&duplicate_values).is_err());
}

#[test]
fn lexical_scalars_require_their_exact_switch_and_option_wrapper() {
    let mut fixture = minimal_fixture();
    let selection_value = fixture
        .contract
        .tree
        .nodes
        .iter_mut()
        .find(|node| {
            matches!(
                node.body,
                ContractNodeBody::Value(ContractValue {
                    source: AdapterScalarSource::SelectionKind,
                    ..
                })
            )
        })
        .expect("selection-kind value");
    selection_value.body = ContractNodeBody::Value(ContractValue {
        source: AdapterScalarSource::MaxOutput,
        transform: AdapterTransform::CheckedU64,
    });
    assert!(validate_contract(&fixture.contract).is_err());

    for (source, transform) in [
        (
            AdapterScalarSource::Proof,
            AdapterTransform::Base64StandardPadded,
        ),
        (
            AdapterScalarSource::ReasoningEffort,
            AdapterTransform::EnumToken(0),
        ),
        (
            AdapterScalarSource::ReasoningBudget,
            AdapterTransform::CheckedU64,
        ),
    ] {
        let table = (source == AdapterScalarSource::ReasoningEffort)
            .then_some(AdapterEnumSource::ReasoningEffort);
        let contract = super::fixtures::single_value_contract(
            AdapterWireId::AnthropicMessages,
            source,
            transform,
            table,
        );
        assert!(validate_contract(&contract).is_err(), "{source:?}");
    }

    for (source, table_source) in [
        (
            AdapterScalarSource::MessageRole,
            AdapterEnumSource::MessageKind,
        ),
        (
            AdapterScalarSource::ToolChoiceKind,
            AdapterEnumSource::ToolChoice,
        ),
        (
            AdapterScalarSource::ReasoningMode,
            AdapterEnumSource::ReasoningMode,
        ),
        (
            AdapterScalarSource::CacheRetention,
            AdapterEnumSource::CacheRetention,
        ),
    ] {
        let contract = super::fixtures::single_value_contract(
            AdapterWireId::AnthropicMessages,
            source,
            AdapterTransform::EnumToken(0),
            Some(table_source),
        );
        assert!(validate_contract(&contract).is_err(), "{source:?}");
    }

    let mut non_root_wrapper = minimal_fixture().contract;
    let system_item = non_root_wrapper
        .tree
        .nodes
        .iter_mut()
        .find(|node| {
            matches!(
                node.body,
                ContractNodeBody::Value(ContractValue {
                    source: AdapterScalarSource::SystemItem,
                    ..
                })
            )
        })
        .expect("system item");
    system_item.presence = AdapterPresence::OmitIfNone;
    system_item.presence_source = Some(AdapterPresenceSource::MaxOutput);
    assert!(validate_contract(&non_root_wrapper).is_err());
}

#[test]
fn unset_control_switches_require_matching_root_omission_wrappers() {
    for source in [
        AdapterVariantSource::ToolChoice,
        AdapterVariantSource::Reasoning,
        AdapterVariantSource::CacheRetention,
    ] {
        let mut nodes = Vec::new();
        let cases: Vec<_> = (0..source.variant_count())
            .map(|_| push_constant(&mut nodes, None))
            .collect();
        let switch = push_node(
            &mut nodes,
            Some(PathSegment::Key("control".to_owned())),
            ContractNodeBody::Switch(ContractSwitch {
                source,
                cases: cases
                    .iter()
                    .enumerate()
                    .map(|(ordinal, node)| ContractCase {
                        variant_ordinal: ordinal as u8,
                        node: *node,
                    })
                    .collect(),
            }),
        );
        attach(&mut nodes, switch, &cases);
        let root = push_node(
            &mut nodes,
            None,
            ContractNodeBody::Object(ContractObject {
                children: vec![switch],
            }),
        );
        attach(&mut nodes, root, &[switch]);
        let contract = super::super::adapter::types::AdapterContractV1 {
            version: 1,
            wire_id: AdapterWireId::AnthropicMessages,
            model_source: super::super::adapter::types::AdapterModelSource::CurrentModel,
            tree: super::super::adapter::types::ContractTree {
                root,
                nodes,
                tables: vec![],
            },
            ordinary_header_rules: vec![],
            decoder_kind: AdapterDecoderKind::AnthropicMessages,
        };
        assert!(validate_contract(&contract).is_err(), "{source:?}");
    }
}

#[test]
fn mistral_composite_accounting_rejects_both_neither_and_crossed_occurrences() {
    let composite = mistral_contract(false, true, false);
    assert!(validate_contract(&composite).is_ok());

    for contract in [
        mistral_contract(true, true, false),
        mistral_contract(false, false, false),
        mistral_contract(true, false, false),
        mistral_contract(false, true, true),
    ] {
        assert_eq!(
            validate_contract(&contract),
            Err(super::super::adapter::types::AdapterValidationError::InvalidContract)
        );
    }
}

#[test]
fn forbidden_wire_sources_are_rejected_even_in_inactive_branches() {
    for (wire, source, transform) in [
        (
            AdapterWireId::AnthropicMessages,
            AdapterScalarSource::ToolResultName,
            AdapterTransform::Identity,
        ),
        (
            AdapterWireId::OpenAiResponses,
            AdapterScalarSource::ToolResultIsError,
            AdapterTransform::Identity,
        ),
        (
            AdapterWireId::PiMessages,
            AdapterScalarSource::ToolResultStatus,
            AdapterTransform::EnumToken(0),
        ),
    ] {
        let table = (source == AdapterScalarSource::ToolResultStatus)
            .then_some(AdapterEnumSource::ToolResultStatus);
        let contract = super::fixtures::single_value_contract(wire, source, transform, table);
        assert!(validate_contract(&contract).is_err(), "{wire:?} {source:?}");
    }
}

fn mistral_contract(
    normal: bool,
    composite: bool,
    crossed: bool,
) -> super::super::adapter::types::AdapterContractV1 {
    let mut nodes = Vec::new();
    let user_case = push_constant(&mut nodes, None);
    let assistant_case = push_constant(&mut nodes, None);

    let mut result_children = Vec::new();
    if normal {
        let block_text_case = push_constant(&mut nodes, None);
        let block_image_case = push_constant(&mut nodes, None);
        let block_switch = push_node(
            &mut nodes,
            Some(PathSegment::ArrayItem),
            ContractNodeBody::Switch(ContractSwitch {
                source: AdapterVariantSource::ToolResultBlock,
                cases: vec![
                    ContractCase {
                        variant_ordinal: 0,
                        node: block_text_case,
                    },
                    ContractCase {
                        variant_ordinal: 1,
                        node: block_image_case,
                    },
                ],
            }),
        );
        attach(
            &mut nodes,
            block_switch,
            &[block_text_case, block_image_case],
        );
        let blocks = push_node(
            &mut nodes,
            Some(PathSegment::Key("blocks".to_owned())),
            ContractNodeBody::Array(ContractArray {
                collection: AdapterCollection::Blocks,
                item: block_switch,
                min: 1,
                max: 4_096,
            }),
        );
        attach(&mut nodes, blocks, &[block_switch]);
        result_children.push(blocks);
    }
    if composite {
        let content = push_node(
            &mut nodes,
            Some(PathSegment::Key("content".to_owned())),
            ContractNodeBody::Value(ContractValue {
                source: AdapterScalarSource::MistralToolResultContent,
                transform: AdapterTransform::MistralToolResultContent,
            }),
        );
        result_children.push(content);
    }
    let result_case = push_node(
        &mut nodes,
        None,
        ContractNodeBody::Object(ContractObject {
            children: result_children.clone(),
        }),
    );
    attach(&mut nodes, result_case, &result_children);

    let message_cases = if crossed {
        vec![result_case, assistant_case, user_case]
    } else {
        vec![user_case, assistant_case, result_case]
    };
    let message_switch = push_node(
        &mut nodes,
        Some(PathSegment::ArrayItem),
        ContractNodeBody::Switch(ContractSwitch {
            source: AdapterVariantSource::Message,
            cases: message_cases
                .iter()
                .enumerate()
                .map(|(ordinal, node)| ContractCase {
                    variant_ordinal: ordinal as u8,
                    node: *node,
                })
                .collect(),
        }),
    );
    attach(&mut nodes, message_switch, &message_cases);
    let messages = push_node(
        &mut nodes,
        Some(PathSegment::Key("messages".to_owned())),
        ContractNodeBody::Array(ContractArray {
            collection: AdapterCollection::Messages,
            item: message_switch,
            min: 0,
            max: 4_096,
        }),
    );
    attach(&mut nodes, messages, &[message_switch]);
    let root = push_node(
        &mut nodes,
        None,
        ContractNodeBody::Object(ContractObject {
            children: vec![messages],
        }),
    );
    attach(&mut nodes, root, &[messages]);

    super::super::adapter::types::AdapterContractV1 {
        version: 1,
        wire_id: AdapterWireId::MistralConversations,
        model_source: super::super::adapter::types::AdapterModelSource::CurrentModel,
        tree: super::super::adapter::types::ContractTree {
            root,
            nodes,
            tables: vec![],
        },
        ordinary_header_rules: vec![],
        decoder_kind: AdapterDecoderKind::MistralConversations,
    }
}

fn push_constant(nodes: &mut Vec<ContractNode>, segment: Option<PathSegment>) -> u32 {
    push_node(
        nodes,
        segment,
        ContractNodeBody::Constant(super::super::adapter::types::ContractConstant {
            value: TypedJsonConstant::Null,
        }),
    )
}

fn push_node(
    nodes: &mut Vec<ContractNode>,
    segment: Option<PathSegment>,
    body: ContractNodeBody,
) -> u32 {
    let index = nodes.len() as u32;
    nodes.push(ContractNode {
        parent: None,
        segment,
        presence: AdapterPresence::Required,
        presence_source: None,
        body,
    });
    index
}

fn attach(nodes: &mut [ContractNode], parent: u32, children: &[u32]) {
    for child in children {
        nodes[*child as usize].parent = Some(parent);
    }
}
