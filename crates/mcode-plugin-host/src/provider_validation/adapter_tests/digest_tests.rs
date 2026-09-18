//! Typed adapter digest ordering mutation tests.
use crate::provider_wit::exports::mcode::provider_pack::provider_api::OrdinaryHeader;

use super::super::adapter::digest::{body_digest, contract_digest, ordinary_header_digest};
use super::super::adapter::types::{AdapterModelSource, ContractNodeBody, OrdinaryHeaderRule};
use super::fixtures::minimal_fixture;

#[test]
fn every_top_level_and_nested_contract_region_changes_the_digest() {
    let fixture = minimal_fixture();
    let original = contract_digest(&fixture.contract).expect("contract digest");

    let mut version = fixture.contract.clone();
    version.version = 2;
    assert_ne!(contract_digest(&version).expect("version digest"), original);

    let mut source = fixture.contract.clone();
    source.model_source = AdapterModelSource::RequestedSelection;
    assert_ne!(contract_digest(&source).expect("source digest"), original);

    let mut tree = fixture.contract.clone();
    let ContractNodeBody::Constant(constant) = &mut tree
        .tree
        .nodes
        .iter_mut()
        .find(|node| matches!(node.body, ContractNodeBody::Constant(_)))
        .expect("constant")
        .body
    else {
        panic!("constant")
    };
    constant.value = super::super::adapter::types::TypedJsonConstant::Boolean(true);
    assert_ne!(contract_digest(&tree).expect("tree digest"), original);

    let mut tables = fixture.contract.clone();
    tables.tree.tables[0].entries[0].token = "direct".to_owned();
    assert_ne!(contract_digest(&tables).expect("table digest"), original);

    let mut fixed = fixture.contract.clone();
    let OrdinaryHeaderRule::Fixed(rule) = &mut fixed.ordinary_header_rules[0] else {
        panic!("fixed")
    };
    rule.value = "text/event-stream".to_owned();
    assert_ne!(contract_digest(&fixed).expect("fixed digest"), original);

    let mut values = fixture.contract.clone();
    let OrdinaryHeaderRule::OneOf(rule) = &mut values.ordinary_header_rules[1] else {
        panic!("one-of")
    };
    rule.values.swap(0, 1);
    assert_ne!(
        contract_digest(&values).expect("value order digest"),
        original
    );
}

#[test]
fn body_and_header_digest_preimages_bind_length_count_and_order() {
    let first = body_digest(b"{}").expect("empty object digest");
    let second = body_digest(b"{\"x\":0}").expect("member object digest");
    assert_eq!(
        first,
        "sha256:9bf48f1700bc188ea7faf2c1749289fdef9e748917d486505fdbd0a331fb83cb"
    );
    assert_ne!(first, second);

    let headers = vec![
        OrdinaryHeader {
            name: "a".to_owned(),
            value: "1".to_owned(),
        },
        OrdinaryHeader {
            name: "b".to_owned(),
            value: "2".to_owned(),
        },
    ];
    let ordered = ordinary_header_digest(&headers).expect("ordered headers");
    let mut reversed_headers = headers;
    reversed_headers.reverse();
    let reversed = ordinary_header_digest(&reversed_headers).expect("reversed headers");
    assert_ne!(ordered, reversed);
}
