//! Parse-based contract tests for the sole-current Web FeaturePack artifacts.

mod support;

use support::{
    allowed_vocabulary, assert_denied_names, assert_denied_types, assert_invoke_and_pull,
    assert_lf, assert_owned_function, assert_pull, assert_rule_inventory, assert_semantic_sha256,
    assert_world_topology, package_interface, parse, semantic_rules, type_inventory,
};
use wit_parser::{PackageId, Resolve};

const SOURCE: &str = include_str!("../wit/feature-pack/web.wit");
const GOLDEN: &str = include_str!("../goldens/feature_web_current.wit");
const SEMANTICS: &str = include_str!("../goldens/feature_web_current.jsonl");
const SEMANTICS_SHA256: &str = "c4109673814dd320e79070e1fcd9e340510367cbd1c0b702a8369d551de81c4b";
const HOST_INVENTORY: &str = r#"search-range=enum(none,d7,w2,m3,y1)
typed-search=record(query:string,count:u8,range:search-range,chunks:u8,country:option<string>,language:option<string>,domains:list<string>)
content-format=enum(markdown,text,html)
typed-fetch=record(urls:list<string>,format:content-format,per-page-timeout:u8,metadata:bool)
web-media=enum(json,event-stream,text,html)
web-head=record(status:u16,media:web-media)
web-data=record(bytes:list<u8>)
web-frame=variant(head:web-head,data:web-data,end)
web-failure=enum(dns,tls,timeout,truncated,transport,cancelled)
web-exchange-pull=variant(pending,frame:web-frame,failed:web-failure)
web-host-error=enum(invalid-argument,authority-rejected,remote-unavailable,protocol,limit,cancelled)
web-exchange=resource
"#;
const PACK_INVENTORY: &str = r#"search-range=enum(none,d7,w2,m3,y1)
search-request=record(query:string,count:u8,range:search-range,chunks:u8,country:option<string>,language:option<string>,domains:list<string>)
content-format=enum(markdown,text,html)
fetch-request=record(urls:list<string>,format:content-format,per-page-timeout:u8,metadata:bool)
web-request=variant(search:search-request,fetch:fetch-request)
fetch-progress=record(completed:u8,total:u8)
web-progress=variant(searching,fetching:fetch-progress)
search-item=record(source-id:string,search-id:string,url:string,title:string,text:string,published:option<string>,truncated:bool)
search-results=record(items:list<search-item>,truncated:bool)
fetch-page=record(source-id:string,search-id:option<string>,url:string,title:option<string>,text:string,published:option<string>,truncated:bool,original-bytes:option<u64>,returned-bytes:u64,returned-lines:u32)
fetch-results=record(pages:list<fetch-page>,truncated:bool)
web-result=variant(search-results:search-results,fetch-results:fetch-results)
web-error=enum(invalid-argument,invalid-url,authority-rejected,remote-unavailable,protocol,limit,cancelled)
web-pull=variant(pending,progress:web-progress,complete:web-result,failed:web-error)
web-operation=resource
"#;
const RULES: &str = "authority-binding authority-digest backpressure deadlines exchange-limits exchange-reducer failure-mapping family-isolation fetch-input fetch-output logical-charge operation-authority progress-reducer redaction search-input search-output stage-boundary text-safety topology typed-requests url-canonicalization";

#[test]
fn web_artifacts_are_identical_lf_and_exactly_shaped() {
    assert_eq!(SOURCE.as_bytes(), GOLDEN.as_bytes());
    for (path, source) in [("web.wit", SOURCE), ("feature_web_current.wit", GOLDEN)] {
        assert_lf(path, source);
        let (resolve, package_id) = parse(path, source);
        assert_shape(&resolve, package_id);
    }
}

#[test]
fn web_semantics_have_the_closed_inventory_and_critical_values() {
    assert_lf("feature_web_current.jsonl", SEMANTICS);
    assert_semantic_sha256(
        "feature_web_current.jsonl",
        SEMANTICS,
        SEMANTICS_SHA256,
        (
            r#""inputBytes":{"min":1,"max":4096,"ascii":true}"#,
            r#""inputBytes":{"min":1,"max":4097,"ascii":true}"#,
        ),
    );
    let rules = semantic_rules(SEMANTICS);
    assert_rule_inventory(&rules, RULES.split_ascii_whitespace());
    assert_eq!(rules["authority-binding"]["version"], 1);
    assert_eq!(
        rules["authority-binding"]["fieldTypes"],
        serde_json::json!([
            "version:u16",
            "family:web-authority-family",
            "manager-id:string",
            "manager-generation:u64",
            "pack-id:string",
            "pack-version:string",
            "pack-hash:string",
            "pack-generation:u64",
            "operation:web-authority-operation",
            "method:web-method",
            "origin:string",
            "path:string",
            "query-policy:web-query-policy",
            "service-id:string",
            "account-id:string",
            "auth-slot-id:string",
            "adapter-policy-digest:string",
            "header-policy-digest:string",
            "redirect-policy:web-redirect-policy",
            "retry-policy:web-retry-policy",
            "deadline:web-deadline",
            "authority-digest:string"
        ])
    );
    assert_eq!(
        rules["authority-binding"]["closedTypes"],
        serde_json::json!([
            {"name":"web-authority-family","kind":"enum","cases":["web"]},
            {"name":"web-authority-operation","kind":"enum","cases":["search","fetch"]},
            {"name":"web-method","kind":"enum","cases":["get","post"]},
            {"name":"web-query-policy","kind":"enum","cases":["none","adapter-canonical"]},
            {"name":"web-redirect-policy","kind":"variant","cases":[{"name":"forbid"},{"name":"same-origin-bounded","type":"u8"}]},
            {"name":"web-retry-policy","kind":"record","fields":["max-attempts:u8","backoff-ms:u32","retry-transport:bool","statuses:list<u16>"]},
            {"name":"web-deadline","kind":"record","fields":["total-ms:u32","per-attempt-ms:u32"]}
        ])
    );
    assert_eq!(
        rules["authority-binding"]["retry"],
        serde_json::json!({
            "attempts":{"min":1,"max":4},
            "backoffMs":{"min":0,"max":60000},
            "statuses":{"min":0,"max":16,"valueMin":400,"valueMax":599,"order":"numeric ascending unique"},
            "singleAttempt":{"maxAttempts":1,"statusesCount":0,"retryTransport":false},
            "multipleAttempts":{"minAttempts":2,"maxAttempts":4,"requiredAny":["retry-transport=true","statuses nonempty"]}
        })
    );
    assert_eq!(
        rules["authority-binding"]["deadline"]["totalMs"]["max"],
        70_000
    );
    assert_eq!(rules["exchange-limits"]["search2xx"]["framesMax"], 32);
    assert_eq!(rules["exchange-limits"]["fetch2xx"]["bytesMax"], 10_485_760);
    assert_eq!(rules["backpressure"]["bufferedFramesMax"], 1);
    assert_eq!(
        rules["fetch-output"]["pages"],
        "exactly one per deduplicated request URL in request order"
    );
    assert_eq!(
        rules["stage-boundary"]["doesNotClaim"][0],
        "binary runtime PASS"
    );
}

#[test]
fn web_contract_guards_parameter_type_and_family_mutations() {
    let typed = SOURCE.replacen("query: string,", "query: list<u8>,", 1);
    let (resolve, package_id) = parse("web typed mutation", &typed);
    let host = package_interface(&resolve, package_id, "web-host");
    assert_ne!(type_inventory(&resolve, host), HOST_INVENTORY);

    let label = SOURCE.replacen(
        "start-search: func(request: typed-search)",
        "start-search: func(input: typed-search)",
        1,
    );
    let (resolve, package_id) = parse("web label mutation", &label);
    let host = package_interface(&resolve, package_id, "web-host");
    assert_eq!(host.functions["start-search"].params[0].name, "input");

    let crossed = SOURCE.replacen("interface web-host", "interface mcp-host", 1);
    let mut resolve = Resolve::default();
    assert!(resolve.push_str("web family mutation", &crossed).is_err());
}

fn assert_shape(resolve: &Resolve, package_id: PackageId) {
    let (pack_id, host_id) = assert_world_topology(
        resolve,
        package_id,
        "mcode:feature-pack@0.0.1",
        "web",
        &["web-host", "web-pack"],
        "web-host",
        "web-pack",
    );
    let host = &resolve.interfaces[host_id];
    let pack = &resolve.interfaces[pack_id];
    assert_eq!(type_inventory(resolve, host), HOST_INVENTORY);
    assert_eq!(type_inventory(resolve, pack), PACK_INVENTORY);
    assert_eq!(host.functions.len(), 3);
    assert_owned_function(
        resolve,
        host,
        "start-search",
        "request",
        "typed-search",
        "web-exchange",
        "web-host-error",
    );
    assert_owned_function(
        resolve,
        host,
        "start-fetch",
        "request",
        "typed-fetch",
        "web-exchange",
        "web-host-error",
    );
    assert_pull(resolve, host, "web-exchange", "web-exchange-pull");
    assert_eq!(pack.functions.len(), 2);
    assert_invoke_and_pull(
        resolve,
        pack,
        "web-operation",
        "web-request",
        "web-error",
        "web-pull",
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
            "mcp-",
            "usage-",
            "provider-",
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
