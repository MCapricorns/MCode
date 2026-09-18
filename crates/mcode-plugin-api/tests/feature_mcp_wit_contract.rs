//! Parse-based contract tests for the sole-current MCP FeaturePack artifacts.

mod support;

use support::{
    allowed_vocabulary, assert_denied_names, assert_denied_types, assert_invoke_and_pull,
    assert_lf, assert_owned_function, assert_pull, assert_rule_inventory, assert_semantic_sha256,
    assert_world_topology, package_interface, parse, semantic_rules, type_inventory, variant_cases,
};
use wit_parser::{PackageId, Resolve};

const SOURCE: &str = include_str!("../wit/feature-pack/mcp.wit");
const GOLDEN: &str = include_str!("../goldens/feature_mcp_current.wit");
const SEMANTICS: &str = include_str!("../goldens/feature_mcp_current.jsonl");
const SEMANTICS_SHA256: &str = "41e7c30d43f791303a3e48d93bc1a68cc8357d1c380f32fc4b35057278e6f83c";
const HOST_INVENTORY: &str = r#"mcp-json-array=record(children:list<u32>)
mcp-json-member=record(key:string,value:u32)
mcp-json-object=record(members:list<mcp-json-member>)
mcp-json-node=variant(null,boolean:bool,number:string,string:string,array:mcp-json-array,object:mcp-json-object)
mcp-json-document=record(root:u32,nodes:list<mcp-json-node>)
typed-invocation=record(snapshot-digest:string,schema-digest:string,server-id:string,tool-id:string,arguments:mcp-json-document)
mcp-head=record(invocation-id:string,snapshot-digest:string,schema-digest:string)
mcp-exchange-output=variant(text:string,json:mcp-json-document)
mcp-failure=enum(transport,protocol,timeout,cancelled)
mcp-frame=variant(head:mcp-head,output:mcp-exchange-output,failed:mcp-failure,end)
mcp-exchange-pull=variant(pending,frame:mcp-frame)
mcp-host-error=enum(invalid-argument,snapshot-mismatch,schema-mismatch,protocol,limit,transport-unavailable,cancelled)
mcp-exchange=resource
"#;
const PACK_INVENTORY: &str = r#"mcp-json-document=alias(mcp-json-document)
servers-request=record(snapshot-digest:string,after:option<string>,limit:u16)
tools-request=record(snapshot-digest:string,server-id:string,after:option<string>,limit:u16)
invoke-request=record(snapshot-digest:string,schema-digest:string,server-id:string,tool-id:string,arguments:mcp-json-document)
mcp-request=variant(servers:servers-request,tools:tools-request,invoke:invoke-request)
mcp-progress=enum(discovering,invoking)
server-info=record(server-id:string,title:string)
server-page=record(items:list<server-info>,next:option<string>)
mcp-output=variant(text:string,json:mcp-json-document)
mcp-error=enum(invalid-argument,snapshot-mismatch,schema-mismatch,server-not-found,tool-not-found,protocol,limit,transport-unavailable,cancelled)
mcp-string-schema=record(min-bytes:u32,max-bytes:u32)
mcp-array-schema=record(item:u32,min-items:u16,max-items:u16)
mcp-schema-property=record(key:string,schema:u32,required:bool)
mcp-additional=variant(forbid,allow-any,schema:u32)
mcp-object-schema=record(properties:list<mcp-schema-property>,additional:mcp-additional)
mcp-number-text=alias(string)
mcp-number-schema=record(integer:bool,minimum:option<mcp-number-text>,maximum:option<mcp-number-text>)
mcp-schema-node=variant(any,null,boolean,number:mcp-number-schema,string:mcp-string-schema,array:mcp-array-schema,object:mcp-object-schema)
mcp-schema-document=record(root:u32,nodes:list<mcp-schema-node>)
tool-info=record(tool-id:string,title:string,description:option<string>,schema-digest:string,schema:mcp-schema-document)
tool-page=record(server-id:string,items:list<tool-info>,next:option<string>)
mcp-result=variant(servers:server-page,tools:tool-page,invoked:mcp-output)
mcp-pull=variant(pending,progress:mcp-progress,complete:mcp-result,failed:mcp-error)
mcp-operation=resource
"#;
const RULES: &str = "backpressure canonical-number digests dto-bounds exchange-reducer failure-mapping family-isolation invocation-output json-ast logical-charge operation-authority pagination progress-reducer schema-ast schema-evaluator stage-boundary text-safety topology typed-invocation";

#[test]
fn mcp_artifacts_are_identical_lf_and_exactly_shaped() {
    assert_eq!(SOURCE.as_bytes(), GOLDEN.as_bytes());
    for (path, source) in [("mcp.wit", SOURCE), ("feature_mcp_current.wit", GOLDEN)] {
        assert_lf(path, source);
        let (resolve, package_id) = parse(path, source);
        assert_shape(&resolve, package_id);
    }
}

#[test]
fn mcp_semantics_have_the_closed_inventory_and_critical_values() {
    assert_lf("feature_mcp_current.jsonl", SEMANTICS);
    assert_semantic_sha256(
        "feature_mcp_current.jsonl",
        SEMANTICS,
        SEMANTICS_SHA256,
        (r#""arrayChildrenMax":1024"#, r#""arrayChildrenMax":1025"#),
    );
    let rules = semantic_rules(SEMANTICS);
    assert_rule_inventory(&rules, RULES.split_ascii_whitespace());
    assert_eq!(rules["json-ast"]["nodes"]["max"], 16_384);
    assert_eq!(rules["schema-ast"]["nodes"]["max"], 4_096);
    assert_eq!(
        rules["schema-ast"]["additionalCases"],
        serde_json::json!(["forbid", "allow-any", "schema"])
    );
    assert_eq!(rules["exchange-reducer"]["nonPendingMax"], 3);
    assert_eq!(rules["backpressure"]["bufferedFramesMax"], 1);
    assert_eq!(
        rules["digests"]["snapshot"]["serverFields"],
        serde_json::json!(["server-id", "server-title", "tools"])
    );
    assert_eq!(
        rules["stage-boundary"]["doesNotClaim"][0],
        "binary runtime PASS"
    );
}

#[test]
fn mcp_contract_guards_references_labels_escapes_and_cross_family_mutations() {
    let typed = SOURCE.replacen("arguments: mcp-json-document,", "arguments: string,", 1);
    let (resolve, package_id) = parse("MCP typed mutation", &typed);
    assert_ne!(
        type_inventory(
            &resolve,
            package_interface(&resolve, package_id, "mcp-host"),
        ),
        HOST_INVENTORY
    );

    let label = SOURCE.replacen(
        "start-invoke: func(request: typed-invocation)",
        "start-invoke: func(input: typed-invocation)",
        1,
    );
    let (resolve, package_id) = parse("MCP label mutation", &label);
    assert_eq!(
        package_interface(&resolve, package_id, "mcp-host").functions["start-invoke"].params[0]
            .name,
        "input"
    );

    let unescaped = SOURCE.replacen("%string(string)", "string(string)", 1);
    assert!(
        Resolve::default()
            .push_str("MCP escape mutation", &unescaped)
            .is_err()
    );
    let crossed = SOURCE.replacen(
        "arguments: mcp-json-document,",
        "arguments: usage-wire-document,",
        1,
    );
    assert!(
        Resolve::default()
            .push_str("MCP family mutation", &crossed)
            .is_err()
    );
}

fn assert_shape(resolve: &Resolve, package_id: PackageId) {
    let (pack_id, host_id) = assert_world_topology(
        resolve,
        package_id,
        "mcode:feature-pack@0.0.1",
        "mcp",
        &["mcp-host", "mcp-pack"],
        "mcp-host",
        "mcp-pack",
    );
    let host = &resolve.interfaces[host_id];
    let pack = &resolve.interfaces[pack_id];
    assert_eq!(type_inventory(resolve, host), HOST_INVENTORY);
    assert_eq!(type_inventory(resolve, pack), PACK_INVENTORY);
    assert_eq!(host.functions.len(), 2);
    assert_owned_function(
        resolve,
        host,
        "start-invoke",
        "request",
        "typed-invocation",
        "mcp-exchange",
        "mcp-host-error",
    );
    assert_pull(resolve, host, "mcp-exchange", "mcp-exchange-pull");
    assert_eq!(pack.functions.len(), 2);
    assert_invoke_and_pull(
        resolve,
        pack,
        "mcp-operation",
        "mcp-request",
        "mcp-error",
        "mcp-pull",
    );
    assert_eq!(
        variant_cases(resolve, host, "mcp-json-node"),
        ["null", "boolean", "number", "string", "array", "object"]
    );
    assert_eq!(
        variant_cases(resolve, pack, "mcp-schema-node"),
        [
            "any", "null", "boolean", "number", "string", "array", "object"
        ]
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
            "provider-",
            "usage-",
            "socket",
            "stdio",
            "json-rpc",
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
