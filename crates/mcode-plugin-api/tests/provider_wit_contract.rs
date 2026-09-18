//! Parse-based sole-current ProviderPack WIT contract tests.
use std::collections::{BTreeMap, BTreeSet};

use mcode_plugin_api::{PROVIDER_INTERFACE, PROVIDER_WIT, PROVIDER_WIT_PACKAGE, PROVIDER_WORLD};
use serde_json::Value;
use wit_parser::{
    Function, FunctionKind, Handle, Interface, PackageId, Resolve, Type, TypeDefKind, WorldItem,
    WorldKey,
};

const PROVIDER_GOLDEN_WIT: &str = include_str!("../goldens/provider_current.wit");
const PROVIDER_SEMANTICS: &str = include_str!("../goldens/provider_current.jsonl");
const CHANGED_SEMANTIC_RULES: [&str; 8] = [
    "catalog-metadata",
    "catalog-fetch",
    "catalog-refresh",
    "dto-bounds",
    "adapter-vocabulary",
    "message-reducer",
    "images",
    "decoder-tool-calls",
];
const EXPECTED_SEMANTIC_VALUES: [(&str, &str, &str); 2] = [
    ("catalog-paging", "/requestLimit", r#"{"min":1,"max":256}"#),
    ("logical-charge", "/prepareInputMax", "8388608"),
];
const EXPECTED_PROVIDER_TYPE_INVENTORY: &str = r#"provider-id=alias(string)
provider-route-id=alias(string)
provider-operation-id=alias(string)
catalog-digest=alias(string)
catalog-content-digest=alias(string)
catalog-revision=record(last-modified:u64,canonical-content-digest:catalog-content-digest)
model-id=alias(string)
model-alias=alias(string)
request-id=alias(string)
turn-id=alias(string)
image-stamp=alias(string)
proof-stamp=alias(string)
model-selection=variant(exact:model-id,alias:model-alias)
capability-support=enum(unknown,unsupported,supported)
input-modality=enum(unknown,text,image)
tool-capability=record(tools:capability-support,auto-choice:capability-support,none-choice:capability-support,specific-choice:capability-support)
reasoning-capability=record(reasoning:capability-support,effort:capability-support,budget:capability-support,proof:capability-support)
catalog-metadata-entry=record(selection:model-selection,display-name:option<string>,input-modalities:list<input-modality>,tool-capability:tool-capability,reasoning-capability:reasoning-capability,context-tokens:option<u64>,max-output-tokens:option<u64>)
catalog-metadata-view=record(revision:catalog-revision,entries:list<catalog-metadata-entry>)
catalog-source-view=variant(embedded,verified:catalog-metadata-view)
descriptor-request=record(provider-id:provider-id,route-id:provider-route-id,catalog-source:catalog-source-view)
provider-descriptor=record(provider-id:provider-id,route-id:provider-route-id,source-revision:option<catalog-revision>,catalog-digest:catalog-digest,model-count:u32)
catalog-entry=record(selection:model-selection,current-model:model-id,display-name:option<string>,input-modalities:list<input-modality>,tool-capability:tool-capability,reasoning-capability:reasoning-capability,context-tokens:option<u64>,max-output-tokens:option<u64>,completion-operation:provider-operation-id)
catalog-request=record(provider-id:provider-id,route-id:provider-route-id,catalog-source:catalog-source-view,catalog-digest:catalog-digest,offset:u32,limit:u16)
catalog-page=record(provider-id:provider-id,route-id:provider-route-id,source-revision:option<catalog-revision>,catalog-digest:catalog-digest,declared-count:u32,offset:u32,entries:list<catalog-entry>,next-offset:option<u32>)
auth-interaction-request=record(provider-id:provider-id,route-id:provider-route-id)
auth-instructions=record(title:string,steps:list<string>)
auth-interaction-response=variant(not-required,instructions:auth-instructions)
wire-json-field=record(key:string,value:u32)
wire-json-array=record(items:list<u32>)
wire-json-object=record(fields:list<wire-json-field>)
wire-json-node=variant(null-value,boolean-value:bool,number-value:string,string-value:string,array-value:wire-json-array,object-value:wire-json-object)
wire-json-document=record(root:u32,nodes:list<wire-json-node>)
text-block=record(text:string)
image-media-type=enum(png,jpeg,gif,webp,tiff)
image-metadata=record(width:u32,height:u32,frames:u32)
image-view=record(stamp:image-stamp,media-type:image-media-type,bytes:list<u8>,metadata:image-metadata)
reasoning-kind=enum(thinking,summary)
reasoning-proof-view=record(stamp:proof-stamp,source-request-id:request-id,source-turn-id:turn-id,source-content-index:u8,reasoning-kind:reasoning-kind,proof:list<u8>)
reasoning-block=record(kind:reasoning-kind,text:string,proof:option<reasoning-proof-view>)
tool-call-block=record(call-id:string,name:string,arguments:wire-json-document)
user-block=variant(text:text-block,image:image-view)
assistant-block=variant(text:text-block,reasoning:reasoning-block,tool-call:tool-call-block)
tool-result-block=variant(text:text-block,image:image-view)
user-message=record(blocks:list<user-block>)
assistant-message=record(blocks:list<assistant-block>)
tool-result-message=record(call-id:string,blocks:list<tool-result-block>,is-error:bool)
message=variant(user:user-message,assistant:assistant-message,tool-result:tool-result-message)
tool-definition=record(name:string,description:string,input-schema:wire-json-document)
specific-tool-choice=record(name:string)
tool-choice=variant(unset,auto,none,specific:specific-tool-choice)
reasoning-effort=enum(minimal,low,medium,high)
enabled-reasoning=record(effort:option<reasoning-effort>,budget-tokens:option<u64>)
reasoning=variant(unset,disabled,enabled:enabled-reasoning)
cache-retention=variant(unset,none,request,session)
prepare-input=record(provider-id:provider-id,route-id:provider-route-id,catalog-digest:catalog-digest,selection:model-selection,current-model:model-id,operation-id:provider-operation-id,request-id:request-id,turn-id:turn-id,system:list<string>,messages:list<message>,tools:list<tool-definition>,tool-choice:tool-choice,reasoning:reasoning,cache-retention:cache-retention,max-output-tokens:option<u64>)
ordinary-header=record(name:string,value:string)
prepared-request=record(body:wire-json-document,ordinary-headers:list<ordinary-header>)
response-media=enum(json,event-stream)
response-head=record(status:u16,media:response-media)
response-frame=variant(head:response-head,data:list<u8>,end)
text-delta=record(content-index:u8,text:string)
reasoning-delta=record(content-index:u8,kind:reasoning-kind,text:string)
reasoning-proof=record(content-index:u8,kind:reasoning-kind,proof:list<u8>)
tool-call-start=record(content-index:u8,call-id:string,name:string)
tool-arguments-delta=record(content-index:u8,call-id:string,delta:string)
tool-call-end=record(content-index:u8,call-id:string)
completion-reason=enum(stop,tool-use,length)
usage=record(input-tokens:option<u64>,output-tokens:option<u64>,cache-read-tokens:option<u64>,cache-write-tokens:option<u64>)
completion-terminal=record(reason:completion-reason,reported-model:option<model-id>,usage:usage)
unsupported-flow=enum(catalog-source,authentication,model,tools,tool-choice,reasoning,cache,image,proof,response-media)
provider-error=variant(invalid-argument,limit,unsupported-flow:unsupported-flow,unavailable,cancelled,failed)
normalized-event=variant(text-delta:text-delta,reasoning-delta:reasoning-delta,reasoning-proof:reasoning-proof,tool-call-start:tool-call-start,tool-arguments-delta:tool-arguments-delta,tool-call-end:tool-call-end,completed:completion-terminal,failed:provider-error)
decoder-pull=variant(events:list<normalized-event>,need-frame)
frame-acceptance=enum(accepted)
response-decoder=resource
prepared-completion=record(request:prepared-request,decoder:own<response-decoder>)
"#;
const EXPECTED_CHANGED_SEMANTICS: &str = r#"{"rule":"catalog-metadata","verifiedView":{"revision":{"type":"catalog-revision","fields":[{"name":"last-modified","type":"u64","min":1,"max":9223372036854775807},{"name":"canonical-content-digest","type":"catalog-content-digest","grammar":"sha256:[0-9a-f]{64}"}]},"entries":{"min":0,"max":4096,"order":"model-selection byte order","unique":true}},"entryFields":["selection","display-name","input-modalities","tool-capability","reasoning-capability","context-tokens","max-output-tokens"],"replacement":"complete for every listed metadata field","missing":"remains option-none or capability-unknown","selectionMustExistInPackSnapshot":true,"countHashSchemaMustMatch":true,"rejectFields":["provider","current-model","completion-operation","endpoint","authentication","wire","header"],"genericMetadataMap":false,"revisionIdentity":{"embeddedSourceRevision":"none","verifiedSourceRevision":"some exact sealed catalog-metadata-view.revision record","echoes":["provider-descriptor.source-revision","catalog-page.source-revision"],"comparison":"field-equal","networkTimeSource":"sole normalized Last-Modified header","cacheTimeSource":"closed cache lastModified","contentDigestSource":"Host recomputation over provider-id and canonical entries"}}
{"rule":"catalog-fetch","owner":"host","method":"GET","target":"https://pi.dev/api/models/providers/{provider-id}","query":false,"credential":false,"compression":false,"redirect":false,"attemptsPerActivationOrRefresh":1,"response":{"finalStatus":"2xx","contentEncoding":"reject any","chunkBytesMax":65536,"rawAggregateBytesMax":2097152,"streamLimitBeforeAggregateAllocation":true,"utf8":true,"jsonDepthMax":32,"jsonNodesMax":32768,"duplicateKeys":"reject","trailingData":"reject","unknownFields":"reject"},"cache":{"bytesMax":2097152,"schema":"closed cache envelope distinct from network wire schema","requiredFields":["formatVersion","kind","providerId","lastModified","canonicalContentDigest","modelCount","entries"],"formatVersion":1,"kind":"mcode-provider-catalog-cache","compatRead":false},"packFetches":false,"lastModified":{"sourceCount":1,"grammar":"strict RFC 9110 IMF-fixdate","normalizedUnixSeconds":{"min":1,"max":9223372036854775807}},"contentLength":{"grammar":"0|[1-9][0-9]*","bytesMax":2097152,"declaredActualMatch":true,"missingAllowsBoundedStreaming":true},"rawSchema":{"id":"pi-provider-response-v3","topLevel":"model-id keyed object","closedGeneratedSchema":true,"signedArtifactDigestRequired":true}}
{"rule":"catalog-refresh","comparison":["last-modified","canonical-content-digest"],"candidateRelations":{"greaterTime":"persist complete candidate as effective cache","equalTimeEqualDigest":"byte-preserving existing cache is effective","lowerTime":"reject candidate and preserve existing cache","equalTimeDifferentDigest":"reject candidate and preserve existing cache","networkUnavailableOrInvalid":"use only attempt-preselected valid cache"},"generationReconciliation":{"matchingRevisionAndBinding":"no pack call and no generation change","noActiveGeneration":"publish initial generation from effective cache","revisionOrBindingMismatch":"validate and atomically publish replacement then drain old generation","publicationFailure":"preserve durable cache and old active generation"},"partialOrAlternateSource":false,"descriptorAndPagination":"reuse one sealed snapshot without refetch"}
{"rule":"dto-bounds","safeText":{"utf8":true,"reject":["CR","DEL","C0 except TAB and LF","C1","U+061C","U+200E","U+200F","U+202A..U+202E","U+2066..U+2069"]},"label":"nonempty safe text without TAB or LF","scalars":{"provider-id":"1..64 lowercase ASCII bytes; starts with a letter, ends alphanumeric, internal bytes are lowercase letters, digits, or nonadjacent hyphens","route-id":"1..256 visible ASCII bytes","operation-id":"1..128 canonical operation bytes","catalog-digest":"sha256:[0-9a-f]{64}","catalog-revision":{"kind":"closed-record","fields":["last-modified:u64","canonical-content-digest:catalog-content-digest"]},"model-id":"1..256 visible ASCII bytes","model-alias":"1..256 visible ASCII bytes","request-id":"1..128 canonical tracking bytes","turn-id":"1..128 canonical tracking bytes","image-stamp":"img1-[0-9a-f]{32}","proof-stamp":"prf1-[0-9a-f]{32}","call-id":"1..128 canonical tracking bytes","tool-name":"1..128 label bytes","catalog-content-digest":"sha256:[0-9a-f]{64}"},"catalog":{"modelCountMax":4096,"displayNameBytesMax":256,"inputModalities":{"max":3,"order":"input-modality declaration order","unique":true,"unknownMustBeSoleElement":true},"contextTokensMinWhenPresent":1,"maxOutputTokensMinWhenPresent":1,"revisionLastModified":{"min":1,"max":9223372036854775807}},"authInstructions":{"titleBytesMax":256,"steps":{"min":1,"max":32,"itemBytesMax":4096}},"prepare":{"systemPartsMax":1024,"messagesMax":4096,"blocksPerMessage":{"min":1,"max":4096},"toolsMax":1024,"systemOrTextOrDescriptionBytesMax":65536,"imageBytesEachMax":8388608,"imageDimensionsAndFramesMin":1,"maxOutputTokensMinWhenPresent":1,"reasoningBudgetTokensMinWhenPresent":1,"imageMetadata":{"width":{"min":1,"max":16384},"height":{"min":1,"max":16384},"frames":{"min":1,"max":64}}},"prepared":{"ordinaryHeadersMax":32},"allListsAndStringsAlsoFitContainingLogicalCharge":true}
{"rule":"adapter-vocabulary","collections":["system","messages","system-messages","blocks","tools"],"variantSources":["model-selection","system-message-entry","message","user-block","assistant-block","tool-result-block","tool-result-status","tool-choice","reasoning","cache-retention"],"scalarSources":["selected-model","selection-kind","system-item","system-joined","message-role","block-kind","block-text","tool-result-call-id","tool-result-is-error","tool-result-status","tool-result-name","mistral-tool-result-content","tool-call-id","tool-call-name","tool-call-arguments","tool-name","tool-description","tool-schema","reasoning-kind","proof","image-bytes","image-media-type","image-width","image-height","image-frames","image-data-uri","tool-choice-kind","tool-choice-name","reasoning-mode","reasoning-effort","reasoning-budget","cache-retention","max-output"],"transforms":["identity","checked-u32","checked-u64","json-subtree","canonical-json-string","mistral-tool-result-content","join-lf","base64-standard-padded","base64-standard-unpadded","data-uri","enum-token"],"presence":["required","omit-if-none","omit-for-unset"],"enumTokenTables":{"order":"source variant order","keysUnique":true,"sourceExhaustive":true},"unknownPathSourceTransformPresence":"reject","collectionBounds":{"system":1024,"messages":4096,"system-messages":5120,"blocks":4096,"tools":1024},"enumSources":["selection-kind","message-kind","user-block-kind","assistant-block-kind","tool-result-block-kind","tool-result-status","reasoning-kind","image-media-type","tool-choice","reasoning-mode","reasoning-effort","cache-retention"]}
{"rule":"message-reducer","roleLegalBlocks":{"user":["text","image"],"assistant":["text","reasoning","tool-call"],"tool-result":["text","image"]},"messageBlocksNonempty":true,"states":["idle","pending(call-id,name,sealed-definition queue)"],"assistantCalls":"record globally unique call ids in declaration order","pendingAllows":"exactly one nonempty tool-result message for the next queued call","pendingRejects":["user message","assistant message","missing result","extra result","duplicate result","out-of-order result","request end"],"queueMustEndEmpty":true,"toolNamesGloballyUnique":true,"callIdsGloballyUnique":true,"idleToolResult":"reject","someZeroTokenCounter":"retain","semanticMinimumOneZero":"reject","toolResultDerivedProjections":{"status":{"false":"success","true":"error"},"name":"exact matched queued call and sealed definition name"}}
{"rule":"images","mediaTypes":["png","jpeg","gif","webp","tiff"],"inputForms":["image-stamp","exact bytes","typed media","verified width","verified height","verified frames"],"forbiddenInputForms":["URL","path","base64 text"],"everyRawImageConsumedExactlyOnce":true,"declaredMetadataMatchesHostVerification":true,"contractOutputAccountsEveryImage":true,"packCreatedBase64":"must decode exactly to bound bytes under the trusted V1 contract","metadata":{"width":{"min":1,"max":16384},"height":{"min":1,"max":16384},"frames":{"min":1,"max":64}},"metadataOutputDebt":"each scalar zero or one; duplicate use rejected"}
{"rule":"decoder-tool-calls","callIdsUnique":true,"sequence":["start","one or more nonempty argument deltas","end"],"perCallArgumentBytesMax":1048576,"allCallArgumentBytesMax":2097152,"onEnd":{"strictJson":true,"root":"object","duplicateKeys":"reject","depthMax":64,"nodesMax":16384,"decodedSafeRules":true,"logicalChargeMax":1048576},"reject":["delta before start","end before delta","duplicate start","crossed call id","argument byte limit plus one"],"deltaCount":{"min":1,"max":16384}}
"#;

#[test]
fn provider_wit_artifacts_are_identical_lf_and_parse_to_the_exact_shape() {
    assert_eq!(PROVIDER_WIT.as_bytes(), PROVIDER_GOLDEN_WIT.as_bytes());
    for (path, source) in [
        ("provider.wit", PROVIDER_WIT),
        ("provider_current.wit", PROVIDER_GOLDEN_WIT),
    ] {
        assert!(
            !source.as_bytes().contains(&b'\r'),
            "{path} must contain only LF"
        );
        assert_eq!(
            source.as_bytes().last(),
            Some(&b'\n'),
            "{path} must end in LF"
        );
        let (resolve, package_id) = parse_provider(path, source);
        assert_provider_shape(&resolve, package_id);
    }
}

#[test]
fn provider_semantics_match_the_closed_authority() {
    let rules = semantic_rules();
    assert_semantic_rule_names(&rules);
    assert_changed_semantic_rules(&rules);
}

#[test]
fn exact_type_inventory_detects_named_type_erasure() {
    for (label, mutated) in [
        (
            "model-selection payload",
            PROVIDER_WIT.replacen("exact(model-id)", "exact(string)", 1),
        ),
        (
            "catalog-entry current-model",
            PROVIDER_WIT.replacen("current-model: model-id", "current-model: string", 1),
        ),
    ] {
        let (resolve, package_id) = parse_provider(label, &mutated);
        let package = &resolve.packages[package_id];
        let interface_id = *package
            .interfaces
            .get(PROVIDER_INTERFACE)
            .expect("provider interface");
        assert_ne!(
            provider_type_inventory(&resolve, &resolve.interfaces[interface_id]),
            EXPECTED_PROVIDER_TYPE_INVENTORY,
            "{label} mutation must change the exact inventory"
        );
    }
}

fn parse_provider(path: &str, source: &str) -> (Resolve, PackageId) {
    let mut resolve = Resolve::default();
    let package_id = resolve
        .push_str(path, source)
        .unwrap_or_else(|error| panic!("{path} must parse with wit-parser 0.254.0: {error:#}"));
    (resolve, package_id)
}

fn assert_provider_shape(resolve: &Resolve, package_id: PackageId) {
    let package = &resolve.packages[package_id];
    assert_eq!(package.name.to_string(), PROVIDER_WIT_PACKAGE);
    assert_eq!(package.interfaces.len(), 1);
    assert_eq!(package.worlds.len(), 1);

    let interface_id = *package
        .interfaces
        .get(PROVIDER_INTERFACE)
        .expect("provider interface must exist");
    let world_id = *package
        .worlds
        .get(PROVIDER_WORLD)
        .expect("provider world must exist");
    let world = &resolve.worlds[world_id];
    assert!(
        world.imports.is_empty(),
        "provider world must have zero imports"
    );
    assert_eq!(world.exports.len(), 1, "provider has one interface export");
    let (export_key, export) = world.exports.first().expect("provider export");
    assert_eq!(export_key, &WorldKey::Interface(interface_id));
    let WorldItem::Interface { id, .. } = export else {
        panic!("provider world export must be an interface");
    };
    assert_eq!(*id, interface_id);

    let interface = &resolve.interfaces[interface_id];
    assert_eq!(interface.name.as_deref(), Some(PROVIDER_INTERFACE));
    assert_exact_provider_type_inventory(resolve, interface);
    assert_interface_functions(resolve, interface);
    assert_response_decoder(resolve, interface);
    assert_provider_type_vocabulary(resolve);
}

fn assert_exact_provider_type_inventory(resolve: &Resolve, interface: &Interface) {
    assert_eq!(
        provider_type_inventory(resolve, interface),
        EXPECTED_PROVIDER_TYPE_INVENTORY
    );
}

fn provider_type_inventory(resolve: &Resolve, interface: &Interface) -> String {
    interface
        .types
        .iter()
        .map(|(name, type_id)| {
            format!(
                "{name}={}",
                type_definition_shape(resolve, &resolve.types[*type_id].kind)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn assert_interface_functions(resolve: &Resolve, interface: &Interface) {
    let freestanding = interface
        .functions
        .iter()
        .filter_map(|(name, function)| {
            (function.kind == FunctionKind::Freestanding).then_some(name.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        freestanding,
        [
            "descriptor",
            "catalog",
            "auth-interaction",
            "prepare-request"
        ]
    );
    assert_eq!(
        interface.functions.len(),
        freestanding.len() + 2,
        "only the four freestanding exports and two resource methods are public"
    );

    assert_freestanding_signature(
        resolve,
        interface,
        "descriptor",
        "request",
        "descriptor-request",
        "provider-descriptor",
    );
    assert_freestanding_signature(
        resolve,
        interface,
        "catalog",
        "request",
        "catalog-request",
        "catalog-page",
    );
    assert_freestanding_signature(
        resolve,
        interface,
        "auth-interaction",
        "request",
        "auth-interaction-request",
        "auth-interaction-response",
    );
    assert_freestanding_signature(
        resolve,
        interface,
        "prepare-request",
        "input",
        "prepare-input",
        "prepared-completion",
    );
}

fn assert_freestanding_signature(
    resolve: &Resolve,
    interface: &Interface,
    function_name: &str,
    parameter_name: &str,
    parameter_type: &str,
    result_type: &str,
) {
    let function = interface
        .functions
        .get(function_name)
        .unwrap_or_else(|| panic!("missing {function_name}"));
    assert_eq!(function.kind, FunctionKind::Freestanding);
    assert_single_parameter(resolve, function, parameter_name, parameter_type);
    assert_result(resolve, function, result_type, "provider-error");
}

fn assert_response_decoder(resolve: &Resolve, interface: &Interface) {
    let resources = interface
        .types
        .iter()
        .filter(|(_, type_id)| matches!(resolve.types[**type_id].kind, TypeDefKind::Resource))
        .map(|(name, type_id)| (name.as_str(), *type_id))
        .collect::<Vec<_>>();
    assert_eq!(resources.len(), 1, "response-decoder is the sole resource");
    let (resource_name, resource_id) = resources[0];
    assert_eq!(resource_name, "response-decoder");

    let pull = interface
        .functions
        .get("[method]response-decoder.pull")
        .expect("decoder pull method");
    assert_eq!(pull.kind, FunctionKind::Method(resource_id));
    assert_method_parameter(resolve, pull, resource_id, "limit", "u8");
    assert_result(resolve, pull, "decoder-pull", "provider-error");

    let push = interface
        .functions
        .get("[method]response-decoder.push")
        .expect("decoder push method");
    assert_eq!(push.kind, FunctionKind::Method(resource_id));
    assert_method_parameter(resolve, push, resource_id, "frame", "response-frame");
    assert_result(resolve, push, "frame-acceptance", "provider-error");

    let decoder_pull_id = *interface
        .types
        .get("decoder-pull")
        .expect("decoder-pull type");
    let TypeDefKind::Variant(decoder_pull) = &resolve.types[decoder_pull_id].kind else {
        panic!("decoder-pull must be a variant");
    };
    let cases = decoder_pull
        .cases
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(cases, ["events", "need-frame"]);
    assert!(decoder_pull.cases[0].ty.is_some());
    assert!(decoder_pull.cases[1].ty.is_none());

    let prepared_id = *interface
        .types
        .get("prepared-completion")
        .expect("prepared-completion type");
    let TypeDefKind::Record(prepared) = &resolve.types[prepared_id].kind else {
        panic!("prepared-completion must be a record");
    };
    let fields = prepared
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(fields, ["request", "decoder"]);
    assert_eq!(
        named_type(resolve, &prepared.fields[0].ty),
        Some("prepared-request")
    );
    let Type::Id(handle_id) = prepared.fields[1].ty else {
        panic!("prepared decoder must be an owned handle");
    };
    let TypeDefKind::Handle(Handle::Own(owned_resource)) = resolve.types[handle_id].kind else {
        panic!("prepared decoder must transfer guest ownership");
    };
    assert_eq!(owned_resource, resource_id);
}

fn assert_single_parameter(
    resolve: &Resolve,
    function: &Function,
    parameter_name: &str,
    parameter_type: &str,
) {
    assert_eq!(
        function.params.len(),
        1,
        "{} parameter count",
        function.name
    );
    assert_parameter(resolve, function, 0, parameter_name, parameter_type);
}

fn assert_method_parameter(
    resolve: &Resolve,
    function: &Function,
    resource_id: wit_parser::TypeId,
    parameter_name: &str,
    parameter_type: &str,
) {
    assert_eq!(
        function.params.len(),
        2,
        "{} parameter count",
        function.name
    );
    assert_eq!(function.params[0].name, "self");
    let Type::Id(self_id) = function.params[0].ty else {
        panic!("{} self parameter must borrow the resource", function.name);
    };
    assert_eq!(
        resolve.types[self_id].kind,
        TypeDefKind::Handle(Handle::Borrow(resource_id))
    );
    assert_parameter(resolve, function, 1, parameter_name, parameter_type);
}

fn assert_parameter(
    resolve: &Resolve,
    function: &Function,
    index: usize,
    parameter_name: &str,
    parameter_type: &str,
) {
    assert_eq!(function.params[index].name, parameter_name);
    if parameter_type == "u8" {
        assert_eq!(function.params[index].ty, Type::U8);
    } else {
        assert_eq!(
            named_type(resolve, &function.params[index].ty),
            Some(parameter_type)
        );
    }
}

fn assert_result(resolve: &Resolve, function: &Function, ok: &str, error: &str) {
    let Type::Id(result_id) = function.result.expect("function result") else {
        panic!("{} must return a typed result", function.name);
    };
    let TypeDefKind::Result(result) = &resolve.types[result_id].kind else {
        panic!("{} must return result", function.name);
    };
    assert_eq!(
        named_type(resolve, result.ok.as_ref().expect("explicit ok type")),
        Some(ok)
    );
    assert_eq!(
        named_type(resolve, result.err.as_ref().expect("explicit error type")),
        Some(error)
    );
}

fn named_type<'a>(resolve: &'a Resolve, value_type: &Type) -> Option<&'a str> {
    let Type::Id(type_id) = value_type else {
        return None;
    };
    resolve.types[*type_id].name.as_deref()
}

fn type_definition_shape(resolve: &Resolve, kind: &TypeDefKind) -> String {
    match kind {
        TypeDefKind::Type(value_type) => format!("alias({})", type_shape(resolve, value_type)),
        TypeDefKind::Record(record) => format!(
            "record({})",
            record
                .fields
                .iter()
                .map(|field| format!("{}:{}", field.name, type_shape(resolve, &field.ty)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeDefKind::Variant(variant) => format!(
            "variant({})",
            variant
                .cases
                .iter()
                .map(|case| match &case.ty {
                    Some(value_type) => {
                        format!("{}:{}", case.name, type_shape(resolve, value_type))
                    }
                    None => case.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeDefKind::Enum(value) => format!(
            "enum({})",
            value
                .cases
                .iter()
                .map(|case| case.name.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        TypeDefKind::Resource => "resource".to_owned(),
        other => panic!("unexpected named ProviderPack type {other:?}"),
    }
}

fn type_shape(resolve: &Resolve, value_type: &Type) -> String {
    match value_type {
        Type::Bool => "bool".to_owned(),
        Type::U8 => "u8".to_owned(),
        Type::U16 => "u16".to_owned(),
        Type::U32 => "u32".to_owned(),
        Type::U64 => "u64".to_owned(),
        Type::String => "string".to_owned(),
        Type::Id(type_id) => {
            let definition = &resolve.types[*type_id];
            if let Some(name) = &definition.name {
                return name.clone();
            }
            match &definition.kind {
                TypeDefKind::List(inner) => format!("list<{}>", type_shape(resolve, inner)),
                TypeDefKind::Option(inner) => format!("option<{}>", type_shape(resolve, inner)),
                TypeDefKind::Type(inner) => type_shape(resolve, inner),
                TypeDefKind::Handle(Handle::Own(resource)) => format!(
                    "own<{}>",
                    resolve.types[*resource]
                        .name
                        .as_deref()
                        .expect("owned resource must be named")
                ),
                other => panic!("unexpected anonymous field type {other:?}"),
            }
        }
        other => panic!("unexpected field type {other:?}"),
    }
}

fn assert_provider_type_vocabulary(resolve: &Resolve) {
    for (_, definition) in resolve.types.iter() {
        match &definition.kind {
            TypeDefKind::Handle(Handle::Borrow(resource_id)) => {
                assert!(
                    definition.name.is_none(),
                    "borrowed handles must be implicit method self"
                );
                assert_eq!(
                    resolve.types[*resource_id].name.as_deref(),
                    Some("response-decoder")
                );
            }
            TypeDefKind::Handle(Handle::Own(resource_id)) => {
                assert_eq!(
                    resolve.types[*resource_id].name.as_deref(),
                    Some("response-decoder")
                );
            }
            TypeDefKind::Map(_, _)
            | TypeDefKind::FixedLengthList(_, _)
            | TypeDefKind::Future(_)
            | TypeDefKind::Stream(_) => panic!("ProviderPack contains a denied WIT type"),
            TypeDefKind::Record(record) => {
                for field in &record.fields {
                    assert_allowed_value_type(&field.ty);
                }
            }
            TypeDefKind::Tuple(tuple) => {
                for value_type in &tuple.types {
                    assert_allowed_value_type(value_type);
                }
            }
            TypeDefKind::Variant(variant) => {
                for value_type in variant.cases.iter().filter_map(|case| case.ty.as_ref()) {
                    assert_allowed_value_type(value_type);
                }
            }
            TypeDefKind::Option(value_type) | TypeDefKind::List(value_type) => {
                assert_allowed_value_type(value_type);
            }
            TypeDefKind::Result(result) => {
                assert_allowed_value_type(result.ok.as_ref().expect("explicit result ok type"));
                assert_allowed_value_type(result.err.as_ref().expect("explicit result error type"));
            }
            TypeDefKind::Type(value_type) => assert_allowed_value_type(value_type),
            TypeDefKind::Unknown => panic!("resolved WIT cannot contain unknown types"),
            TypeDefKind::Resource | TypeDefKind::Flags(_) | TypeDefKind::Enum(_) => {}
        }
    }
}

fn assert_allowed_value_type(value_type: &Type) {
    assert!(
        !matches!(value_type, Type::F32 | Type::F64 | Type::ErrorContext),
        "ProviderPack contains a denied scalar type"
    );
}

fn semantic_rules() -> BTreeMap<String, Value> {
    parse_semantic_rules("provider semantics", PROVIDER_SEMANTICS)
}

fn parse_semantic_rules(label: &str, source: &str) -> BTreeMap<String, Value> {
    let mut rules = BTreeMap::new();
    for (index, line) in source.lines().enumerate() {
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("{label} line {}: {error}", index + 1));
        let rule = value["rule"]
            .as_str()
            .unwrap_or_else(|| panic!("{label} line {} has no rule", index + 1))
            .to_owned();
        assert!(
            rules.insert(rule.clone(), value).is_none(),
            "duplicate {label} rule {rule}"
        );
    }
    rules
}

fn assert_semantic_rule_names(rules: &BTreeMap<String, Value>) {
    let expected = "adapter-contract-v1 adapter-tree adapter-validation adapter-vocabulary alias-binding catalog-digest catalog-fetch catalog-metadata catalog-ordering catalog-paging catalog-refresh catalog-snapshot catalog-source decoder-backpressure decoder-closure decoder-completion decoder-events decoder-frames decoder-non-2xx decoder-pull decoder-terminal decoder-tool-calls dto-bounds final-outbound-headers header-deny header-values images logical-charge message-reducer operation-authority proofs reasoning-cache stable-errors tool-choice topology wire-json-number wire-json-text wire-json-tree"
        .split_ascii_whitespace()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        rules.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        expected
    );
}

fn assert_changed_semantic_rules(rules: &BTreeMap<String, Value>) {
    let expected = parse_semantic_rules(
        "expected changed provider semantics",
        EXPECTED_CHANGED_SEMANTICS,
    );
    assert_eq!(expected.len(), CHANGED_SEMANTIC_RULES.len());

    for rule in CHANGED_SEMANTIC_RULES {
        let actual = rules
            .get(rule)
            .unwrap_or_else(|| panic!("missing semantic rule {rule}"));
        let expected = expected
            .get(rule)
            .unwrap_or_else(|| panic!("missing expected changed semantic rule {rule}"));
        assert_eq!(actual, expected, "changed semantic rule {rule}");
    }

    for (rule, pointer, expected_json) in EXPECTED_SEMANTIC_VALUES {
        let actual = rules
            .get(rule)
            .unwrap_or_else(|| panic!("missing semantic rule {rule}"))
            .pointer(pointer);
        let expected: Value = serde_json::from_str(expected_json)
            .unwrap_or_else(|error| panic!("invalid expected value {rule}{pointer}: {error}"));
        assert_eq!(actual, Some(&expected), "semantic value {rule}{pointer}");
    }
}
