//! Parse-based contract tests for the sole-current Usage FeaturePack artifacts.

mod support;

use support::{
    allowed_vocabulary, assert_denied_names, assert_denied_types, assert_invoke_and_pull,
    assert_lf, assert_pull, assert_rule_inventory, assert_semantic_sha256, assert_world_topology,
    assert_zero_parameter_owned_function, package_interface, parse, semantic_rules, type_inventory,
    variant_cases,
};
use wit_parser::{PackageId, Resolve};

const SOURCE: &str = include_str!("../wit/feature-pack/usage.wit");
const GOLDEN: &str = include_str!("../goldens/feature_usage_current.wit");
const SEMANTICS: &str = include_str!("../goldens/feature_usage_current.jsonl");
const SEMANTICS_SHA256: &str = "36f8efa5997c181fb15aed8f484babe073d313a8e534d1ba015573a4cc4521b5";
const HOST_INVENTORY: &str = r#"usage-wire-array=record(children:list<u32>)
usage-wire-member=record(key:string,value:u32)
usage-wire-object=record(members:list<usage-wire-member>)
usage-wire-node=variant(null,boolean:bool,number:string,string:string,array:usage-wire-array,object:usage-wire-object)
usage-wire-document=record(root:u32,nodes:list<usage-wire-node>)
usage-host-error=enum(authority-rejected,protocol,source-unavailable,limit,cancelled)
usage-head=record(status:u16,source-contract-digest:string,pack-generation:u64)
usage-document-frame=record(value:usage-wire-document)
usage-failure=enum(transport,protocol,timeout,cancelled)
usage-frame=variant(head:usage-head,document:usage-document-frame,failed:usage-failure,end)
usage-exchange-pull=variant(pending,frame:usage-frame)
usage-exchange=resource
"#;
const PACK_INVENTORY: &str = r#"usage-wire-document=alias(usage-wire-document)
usage-progress=enum(normalizing,refreshing)
usage-tone=enum(neutral,info,success,warning,error)
usage-row=record(id:string,label:string,value:string,tone:usage-tone)
summary-result=record(rows:list<usage-row>)
usage-card=record(id:string,title:string,rows:list<usage-row>)
details-result=record(cards:list<usage-card>)
usage-result=variant(ingested,summary:summary-result,details:details-result,refreshed)
usage-error=enum(invalid-argument,stale-stamp,duplicate-sample,source-unavailable,limit,cancelled)
usage-pull=variant(pending,progress:usage-progress,complete:usage-result,failed:usage-error)
usage-source-view=record(source:string,source-stamp:string)
refresh-request=record(source-view:usage-source-view)
usage-counters=record(input:option<u64>,output:option<u64>,cache-read:option<u64>,cache-write:option<u64>)
usage-sample-view=record(source:string,sample-id:string,producer-provider:string,producer-route:string,producer-request:string,producer-turn:string,current-model:string,requested-model:string,requested-alias:option<string>,resolved-model:option<string>,counters:usage-counters,source-contract-digest:string,producer-pack-hash:string,producer-pack-generation:u64,producer-route-generation:u64,terminal:bool)
ingest-request=record(source-view:usage-source-view,sample:usage-sample-view)
usage-render-state-view=record(state-stamp:string,source:string,source-contract-digest:string,consumer-pack-generation:u64,accepted-samples:list<usage-sample-view>,latest-refresh:option<usage-wire-document>)
render-summary-request=record(source-view:usage-source-view,state:usage-render-state-view)
render-details-request=record(source-view:usage-source-view,state:usage-render-state-view)
usage-request=variant(ingest:ingest-request,render-summary:render-summary-request,render-details:render-details-request,refresh:refresh-request)
usage-operation=resource
"#;
const RULES: &str = "backpressure exchange-reducer failure-mapping family-isolation generation-separation ingest logical-charge operation-authority outputs progress-reducer refresh render-state sample-view source-contract source-digests source-key source-schema source-view stage-boundary text-safety topology wire-ast";

#[test]
fn usage_artifacts_are_identical_lf_and_exactly_shaped() {
    assert_eq!(SOURCE.as_bytes(), GOLDEN.as_bytes());
    for (path, source) in [("usage.wit", SOURCE), ("feature_usage_current.wit", GOLDEN)] {
        assert_lf(path, source);
        let (resolve, package_id) = parse(path, source);
        assert_shape(&resolve, package_id);
    }
}

#[test]
fn usage_semantics_have_the_closed_inventory_and_critical_values() {
    assert_lf("feature_usage_current.jsonl", SEMANTICS);
    assert_semantic_sha256(
        "feature_usage_current.jsonl",
        SEMANTICS,
        SEMANTICS_SHA256,
        (
            r#""readBeforeConsumerPull":false"#,
            r#""readBeforeConsumerPull":true"#,
        ),
    );
    let rules = semantic_rules(SEMANTICS);
    assert_rule_inventory(&rules, RULES.split_ascii_whitespace());
    assert_eq!(
        rules["source-contract"]["fields"],
        serde_json::json!([
            "version",
            "manager-id",
            "pack-id",
            "pack-version",
            "pack-hash",
            "publisher-source-id",
            "canonical-source-key",
            "operation-id",
            "authority-digest",
            "schema"
        ])
    );
    assert_eq!(
        rules["source-contract"]["fieldTypes"],
        serde_json::json!([
            "version:u16",
            "manager-id:string",
            "pack-id:string",
            "pack-version:string",
            "pack-hash:string",
            "publisher-source-id:string",
            "canonical-source-key:string",
            "operation-id:string",
            "authority-digest:string",
            "schema:usage-source-schema"
        ])
    );
    assert_eq!(rules["source-contract"]["operation-id"], "refresh");
    assert_eq!(
        rules["source-schema"]["closedTypes"],
        serde_json::json!([
            {"name":"usage-source-schema","kind":"record","fields":["root:u32","nodes:list<usage-schema-node>"]},
            {"name":"usage-schema-node","kind":"variant","cases":[{"name":"null"},{"name":"boolean"},{"name":"number","type":"usage-number-schema"},{"name":"string","type":"usage-string-schema"},{"name":"array","type":"usage-array-schema"},{"name":"object","type":"usage-object-schema"}]},
            {"name":"usage-number-schema","kind":"record","fields":["minimum:option<usage-number-text>","maximum:option<usage-number-text>"]},
            {"name":"usage-string-schema","kind":"record","fields":["min-bytes:u32","max-bytes:u32"]},
            {"name":"usage-array-schema","kind":"record","fields":["item:u32","min-items:u16","max-items:u16"]},
            {"name":"usage-object-schema","kind":"record","fields":["properties:list<usage-schema-property>","additional:usage-additional"]},
            {"name":"usage-schema-property","kind":"record","fields":["key:string","schema:u32","required:bool"]},
            {"name":"usage-additional","kind":"enum","cases":["forbid"]},
            {"name":"usage-number-text","kind":"alias","type":"string"}
        ])
    );
    assert_eq!(rules["exchange-reducer"]["status"]["min"], 200);
    assert_eq!(rules["exchange-reducer"]["status"]["max"], 599);
    assert_eq!(
        rules["generation-separation"]["exactHeadFieldName"],
        "pack-generation"
    );
    assert_eq!(rules["wire-ast"]["nodes"]["max"], 16_384);
    assert_eq!(rules["source-schema"]["nodes"]["max"], 4_096);
    assert_eq!(rules["backpressure"]["bufferedFramesMax"], 1);
    assert_eq!(
        rules["stage-boundary"]["doesNotClaim"][0],
        "binary runtime PASS"
    );
}

#[test]
fn usage_contract_guards_references_labels_escapes_and_cross_family_mutations() {
    let typed = SOURCE.replacen(
        "latest-refresh: option<usage-wire-document>",
        "latest-refresh: option<string>",
        1,
    );
    let (resolve, package_id) = parse("Usage typed mutation", &typed);
    assert_ne!(
        type_inventory(
            &resolve,
            package_interface(&resolve, package_id, "usage-pack"),
        ),
        PACK_INVENTORY
    );

    let generation = SOURCE.replacen("pack-generation: u64", "provider-generation: u64", 1);
    let (resolve, package_id) = parse("Usage generation mutation", &generation);
    assert_ne!(
        type_inventory(
            &resolve,
            package_interface(&resolve, package_id, "usage-host"),
        ),
        HOST_INVENTORY
    );

    let label = SOURCE.replacen(
        "invoke: func(request: usage-request)",
        "invoke: func(input: usage-request)",
        1,
    );
    let (resolve, package_id) = parse("Usage label mutation", &label);
    assert_eq!(
        package_interface(&resolve, package_id, "usage-pack").functions["invoke"].params[0].name,
        "input"
    );

    let unescaped = SOURCE.replacen("%string(string)", "string(string)", 1);
    assert!(
        Resolve::default()
            .push_str("Usage escape mutation", &unescaped)
            .is_err()
    );
    let crossed = SOURCE.replacen(
        "latest-refresh: option<usage-wire-document>",
        "latest-refresh: option<mcp-json-document>",
        1,
    );
    assert!(
        Resolve::default()
            .push_str("Usage family mutation", &crossed)
            .is_err()
    );
}

fn assert_shape(resolve: &Resolve, package_id: PackageId) {
    let (pack_id, host_id) = assert_world_topology(
        resolve,
        package_id,
        "mcode:feature-pack@0.0.1",
        "usage",
        &["usage-host", "usage-pack"],
        "usage-host",
        "usage-pack",
    );
    let host = &resolve.interfaces[host_id];
    let pack = &resolve.interfaces[pack_id];
    assert_eq!(type_inventory(resolve, host), HOST_INVENTORY);
    assert_eq!(type_inventory(resolve, pack), PACK_INVENTORY);
    assert_eq!(host.functions.len(), 2);
    assert_zero_parameter_owned_function(
        resolve,
        host,
        "start-refresh",
        "usage-exchange",
        "usage-host-error",
    );
    assert_pull(resolve, host, "usage-exchange", "usage-exchange-pull");
    assert_eq!(pack.functions.len(), 2);
    assert_invoke_and_pull(
        resolve,
        pack,
        "usage-operation",
        "usage-request",
        "usage-error",
        "usage-pull",
    );
    assert_eq!(
        variant_cases(resolve, host, "usage-wire-node"),
        ["null", "boolean", "number", "string", "array", "object"]
    );
    assert!(allowed_vocabulary(
        SOURCE,
        &[
            "wasi:",
            "map<",
            "future<",
            "stream<",
            "borrow<",
            "f32",
            "f64",
            "error-context",
            "pack-operation",
            "provider-pack",
            "mcp-",
            "socket",
            "endpoint",
            "credential",
        ]
    ));
    assert_denied_names(
        resolve,
        package_id,
        &["value", "map", "metadata", "pack-operation"],
        &[],
    );
    assert_denied_types(resolve);
}
