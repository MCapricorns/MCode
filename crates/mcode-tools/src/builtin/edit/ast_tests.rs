use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn};
use crate::tool::ToolError;
use serde_json::json;

async fn apply_ast(
    filename: &str,
    source: &str,
    query: &str,
    capture: &str,
    replacement: &str,
) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(filename);
    std::fs::write(&path, source).unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": filename,
            "operations": [{
                "type": "ast",
                "query": query,
                "capture": capture,
                "replacement": replacement
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    std::fs::read_to_string(&path).unwrap()
}

#[tokio::test]
async fn ast_replaces_rust_fn_name() {
    let out = apply_ast(
        "lib.rs",
        "fn foo() {}\n",
        "(function_item name: (identifier) @name)",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "fn bar() {}\n");
}

#[tokio::test]
async fn ast_replaces_typescript_fn_name() {
    let out = apply_ast(
        "a.ts",
        "function foo() { return 1; }\n",
        "(function_declaration name: (identifier) @name)",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "function bar() { return 1; }\n");
}

#[tokio::test]
async fn ast_replaces_javascript_fn_name() {
    let out = apply_ast(
        "a.js",
        "function foo() { return 1; }\n",
        "(function_declaration name: (identifier) @name)",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "function bar() { return 1; }\n");
}

#[tokio::test]
async fn ast_replaces_python_fn_name() {
    let out = apply_ast(
        "a.py",
        "def foo():\n    pass\n",
        "(function_definition name: (identifier) @name)",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "def bar():\n    pass\n");
}

#[tokio::test]
async fn ast_replaces_go_fn_name() {
    let out = apply_ast(
        "a.go",
        "package p\nfunc foo() {}\n",
        "(function_declaration name: (identifier) @name)",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "package p\nfunc bar() {}\n");
}

#[tokio::test]
async fn ast_replaces_java_method_name() {
    let out = apply_ast(
        "A.java",
        "class C { void foo() {} }\n",
        "(method_declaration name: (identifier) @name)",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "class C { void bar() {} }\n");
}

#[tokio::test]
async fn ast_replaces_c_fn_name() {
    let out = apply_ast(
        "a.c",
        "void foo(void) {}\n",
        "(function_definition declarator: (function_declarator declarator: (identifier) @name))",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "void bar(void) {}\n");
}

#[tokio::test]
async fn ast_replaces_cpp_fn_name() {
    let out = apply_ast(
        "a.cpp",
        "int foo() { return 0; }\n",
        "(function_definition declarator: (function_declarator declarator: (identifier) @name))",
        "name",
        "bar",
    )
    .await;
    assert_eq!(out, "int bar() { return 0; }\n");
}

#[tokio::test]
async fn ast_replaces_csharp_method_name() {
    let out = apply_ast(
        "A.cs",
        "class C { void Foo() {} }\n",
        "(method_declaration name: (identifier) @name)",
        "name",
        "Bar",
    )
    .await;
    assert_eq!(out, "class C { void Bar() {} }\n");
}

#[tokio::test]
async fn ast_replaces_json_key() {
    let out = apply_ast(
        "a.json",
        "{\"foo\": 1}\n",
        "(pair key: (string) @key)",
        "key",
        "\"bar\"",
    )
    .await;
    assert_eq!(out, "{\"bar\": 1}\n");
}

#[tokio::test]
async fn ast_overlap_with_literal_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [
                {
                    "type": "ast",
                    "query": "(function_item name: (identifier) @name)",
                    "capture": "name",
                    "replacement": "bar"
                },
                {
                    "type": "literal",
                    "pattern": "foo",
                    "replacement": "baz"
                }
            ]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("overlap"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "fn foo() {}\n"
    );
}

#[tokio::test]
async fn ast_reparse_error_does_not_publish() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "(function_item name: (identifier) @name)",
                "capture": "name",
                "replacement": "foo("
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("syntax error"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "fn foo() {}\n"
    );
}

#[tokio::test]
async fn ast_noop_inside_preexisting_error_does_not_block_other_edit() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn foo() {}\n@\n";
    std::fs::write(dir.path().join("lib.rs"), source).unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [
                {
                    "type": "ast",
                    "query": "(function_item name: (identifier) @name)",
                    "capture": "name",
                    "replacement": "bar"
                },
                {
                    "type": "ast",
                    "query": "(ERROR) @broken",
                    "capture": "broken",
                    "replacement": "@broken"
                }
            ]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "fn bar() {}\n@\n"
    );
}

#[tokio::test]
async fn ast_preserves_preexisting_errors_with_utf8_bom() {
    let dir = tempfile::tempdir().unwrap();
    let source = "\u{feff}fn foo() {}\nfn broken(\n";
    std::fs::write(dir.path().join("lib.rs"), source).unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "(function_item name: (identifier) @name)",
                "capture": "name",
                "replacement": "renamed"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "\u{feff}fn renamed() {}\nfn broken(\n"
    );
}

#[tokio::test]
async fn ast_maps_preexisting_errors_when_edit_introduces_utf8_bom() {
    let dir = tempfile::tempdir().unwrap();
    let source = "foo();\nfn broken(\n";
    std::fs::write(dir.path().join("lib.rs"), source).unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "((identifier) @name (#eq? @name \"foo\"))",
                "capture": "name",
                "replacement": "\u{feff}foo"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "\u{feff}foo();\nfn broken(\n"
    );
}

#[tokio::test]
async fn ast_maps_preexisting_errors_across_multiple_length_changes() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn foo() {}\nstruct B;\nfn broken(\n";
    std::fs::write(dir.path().join("lib.rs"), source).unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [
                {
                    "type": "ast",
                    "query": "(function_item name: (identifier) @name)",
                    "capture": "name",
                    "replacement": "long_function_name"
                },
                {
                    "type": "ast",
                    "query": "(struct_item name: (type_identifier) @name)",
                    "capture": "name",
                    "replacement": "LongStructName"
                }
            ]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "fn long_function_name() {}\nstruct LongStructName;\nfn broken(\n"
    );
}

#[tokio::test]
async fn ast_misspelled_capture_field_is_rejected_without_editing() {
    let dir = tempfile::tempdir().unwrap();
    let source = "fn foo() { let local = 1; }\n";
    std::fs::write(dir.path().join("lib.rs"), source).unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "(identifier) @name",
                "caputre": "name",
                "replacement": "changed"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();

    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        source
    );
}

#[tokio::test]
async fn ast_unknown_language_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "language": "cobol",
                "query": "(function_item name: (identifier) @name)",
                "capture": "name",
                "replacement": "bar"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(err.to_string().contains("language"), "{err}");
}

#[tokio::test]
async fn ast_unsupported_extension_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.xyz"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.xyz",
            "operations": [{
                "type": "ast",
                "query": "(function_item name: (identifier) @name)",
                "capture": "name",
                "replacement": "bar"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(err.to_string().contains("extension"), "{err}");
}

#[tokio::test]
async fn ast_selected_capture_skips_other_query_patterns() {
    let out = apply_ast(
        "lib.rs",
        "fn foo() {}\nstruct Keep;\n",
        "(function_item name: (identifier) @name)\n(struct_item name: (type_identifier) @other)",
        "name",
        "renamed_@name",
    )
    .await;
    assert_eq!(out, "fn renamed_foo() {}\nstruct Keep;\n");
}

#[tokio::test]
async fn ast_repeated_target_capture_expands_per_node() {
    let out = apply_ast(
        "lib.rs",
        "fn f() { let _ = a + b; }\n",
        "(binary_expression left: (identifier) @id right: (identifier) @id)",
        "id",
        "pre_@id",
    )
    .await;
    assert_eq!(out, "fn f() { let _ = pre_a + pre_b; }\n");
}

#[tokio::test]
async fn ast_template_expands_dotted_capture_name() {
    let out = apply_ast(
        "lib.rs",
        "fn foo() {}\n",
        "(function_item name: (identifier) @function.method)",
        "function.method",
        "pre_@function.method",
    )
    .await;
    assert_eq!(out, "fn pre_foo() {}\n");
}

#[tokio::test]
async fn ast_template_expands_portable_capture_name_characters() {
    let out = apply_ast(
        "lib.rs",
        "fn foo() {}\n",
        "(function_item name: (identifier) @1-name.part?!)",
        "1-name.part?!",
        "pre_@1-name.part?!",
    )
    .await;
    assert_eq!(out, "fn pre_foo() {}\n");
}

#[tokio::test]
async fn ast_nonportable_selected_capture_name_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "(function_item name: (identifier) @name)",
                "capture": "名",
                "replacement": "bar"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(err.to_string().contains("not portable"), "{err}");
}

#[tokio::test]
async fn ast_template_double_at_emits_literal_at() {
    let out = apply_ast(
        "lib.rs",
        "const S: &str = \"old\";\n",
        "(string_literal) @value",
        "value",
        "\"mail@@host\"",
    )
    .await;
    assert_eq!(out, "const S: &str = \"mail@host\";\n");
}

#[tokio::test]
async fn ast_multiple_operations_share_the_snapshot_tree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\nstruct Bar;\n").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [
                {
                    "type": "ast",
                    "query": "(function_item name: (identifier) @name)",
                    "capture": "name",
                    "replacement": "baz"
                },
                {
                    "type": "ast",
                    "query": "(struct_item name: (type_identifier) @name)",
                    "capture": "name",
                    "replacement": "Qux"
                }
            ]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "fn baz() {}\nstruct Qux;\n"
    );
}

#[tokio::test]
async fn ast_excessive_completed_captures_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!(
        "fn f() {{ {} }}\n",
        "a;".repeat(super::ast::MAX_AST_SCANNED + 1)
    );
    std::fs::write(dir.path().join("lib.rs"), &source).unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "(identifier) @name",
                "capture": "name",
                "replacement": "x"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("scanned"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        source
    );
}

#[test]
fn ast_parse_and_query_observe_cancellation() {
    use super::engine::PreparedOp;
    use tokio_util::sync::CancellationToken;

    let body = "fn f() { let _ = a + b; }\n";
    let language = super::ast::resolve_language(Some("rust"), "lib.rs").unwrap();
    let parse_cancel = CancellationToken::new();
    parse_cancel.cancel();
    let parse_error = super::ast::parse_body(language, body, &parse_cancel).unwrap_err();
    assert!(
        parse_error.to_string().contains("cancelled"),
        "{parse_error}"
    );

    let cancel = CancellationToken::new();
    let tree = super::ast::parse_body(language, body, &cancel).unwrap();
    let prepared = super::ast::prepare(
        Some("rust"),
        "lib.rs",
        "(identifier) @name",
        "x",
        Some("name"),
    )
    .unwrap();
    let PreparedOp::Ast {
        query,
        replacement,
        capture,
        ..
    } = prepared
    else {
        panic!("ast prepare returned a non-ast operation");
    };
    cancel.cancel();
    let mut planned = Vec::new();
    let mut replacement_bytes = 0;
    let query_error = super::ast::plan_ast(
        body,
        &tree,
        super::ast::AstPlan {
            query: &query,
            replacement: &replacement,
            capture: capture.as_deref(),
        },
        0,
        &mut planned,
        &mut replacement_bytes,
        &cancel,
    )
    .unwrap_err();
    assert!(
        query_error.to_string().contains("cancelled"),
        "{query_error}"
    );
}

#[tokio::test]
async fn ast_query_too_large_is_invalid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let query = "(".repeat(super::ast::MAX_AST_QUERY_BYTES + 1);
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": query,
                "capture": "name",
                "replacement": "bar"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(err.to_string().contains("query exceeds"), "{err}");
}

#[test]
fn ast_invalid_query_diagnostic_does_not_echo_source() {
    let leaked = format!("LEAK{}", "x".repeat(super::ast::MAX_AST_QUERY_BYTES - 16));
    let query = format!("({leaked}) @name");
    assert!(query.len() <= super::ast::MAX_AST_QUERY_BYTES);

    let err = match super::ast::prepare(Some("rust"), "lib.rs", &query, "bar", Some("name")) {
        Ok(_) => panic!("expected invalid query to fail"),
        Err(error) => error,
    };
    let msg = err.to_string();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(msg.contains("invalid tree-sitter query"), "{msg}");
    assert!(msg.len() < 256, "diagnostic must stay bounded: {msg}");
    assert!(!msg.contains("LEAK"), "leaked query source: {msg}");
}

#[tokio::test]
async fn ast_unknown_template_capture_diagnostic_does_not_echo_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let leaked = format!(
        "LEAK{}",
        "x".repeat(crate::builtin::fs_search::MAX_PATTERN_BYTES - 5)
    );
    let replacement = format!("@{leaked}");
    assert_eq!(
        replacement.len(),
        crate::builtin::fs_search::MAX_PATTERN_BYTES
    );

    let err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "(function_item name: (identifier) @name)",
                "capture": "name",
                "replacement": replacement
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(msg.contains("unknown capture"), "{msg}");
    assert!(msg.len() < 256, "diagnostic must stay bounded: {msg}");
    assert!(!msg.contains("LEAK"), "leaked capture name: {msg}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("lib.rs")).unwrap(),
        "fn foo() {}\n"
    );
}

fn assert_bounded_field_error(err: &ToolError, field: &str, leaked: &str) {
    let msg = err.to_string();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(msg.contains(field), "{msg}");
    assert!(msg.contains("exceeds"), "{msg}");
    assert!(
        msg.len() < 256,
        "diagnostic must stay bounded, got {} bytes: {msg}",
        msg.len()
    );
    assert!(
        !msg.contains(leaked),
        "leaked attacker-controlled value: {msg}"
    );
}

#[test]
fn ast_capture_at_limit_is_accepted() {
    let name = "a".repeat(super::ast::MAX_AST_CAPTURE_BYTES);
    let query = format!("(function_item name: (identifier) @{name})");
    super::ast::prepare(Some("rust"), "lib.rs", &query, "bar", Some(&name)).unwrap();
}

#[test]
fn ast_capture_over_limit_is_rejected_without_leakage() {
    let name = format!(
        "LEAK{}",
        "a".repeat(super::ast::MAX_AST_CAPTURE_BYTES + 2048)
    );
    let err = match super::ast::prepare(
        Some("rust"),
        "lib.rs",
        "(identifier) @name",
        "bar",
        Some(&name),
    ) {
        Ok(_) => panic!("expected over-limit capture to fail"),
        Err(error) => error,
    };
    assert_bounded_field_error(&err, "capture", &name);
    assert!(!err.to_string().contains("LEAK"), "{err}");
}

#[test]
fn ast_language_at_limit_is_length_accepted() {
    let name = "x".repeat(super::ast::MAX_AST_LANGUAGE_BYTES);
    let err = match super::ast::prepare(
        Some(&name),
        "lib.rs",
        "(identifier) @name",
        "bar",
        Some("name"),
    ) {
        Ok(_) => panic!("expected at-limit unknown language to fail"),
        Err(error) => error,
    };
    let msg = err.to_string();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert!(msg.contains("language"), "{msg}");
    assert!(!msg.contains("exceeds"), "{msg}");
    assert!(
        msg.len() < 256,
        "diagnostic must stay bounded, got {}",
        msg.len()
    );
    assert!(!msg.contains(&name), "leaked at-limit language: {msg}");
}

#[test]
fn ast_language_over_limit_is_rejected_without_leakage() {
    let name = format!(
        "LEAK{}",
        "x".repeat(super::ast::MAX_AST_LANGUAGE_BYTES + 2048)
    );
    let err = match super::ast::prepare(
        Some(&name),
        "lib.rs",
        "(identifier) @name",
        "bar",
        Some("name"),
    ) {
        Ok(_) => panic!("expected over-limit language to fail"),
        Err(error) => error,
    };
    assert_bounded_field_error(&err, "language", &name);
    assert!(!err.to_string().contains("LEAK"), "{err}");
}

#[tokio::test]
async fn ast_capture_at_limit_replaces() {
    let name = "a".repeat(super::ast::MAX_AST_CAPTURE_BYTES);
    let query = format!("(function_item name: (identifier) @{name})");
    let out = apply_ast("lib.rs", "fn foo() {}\n", &query, &name, "bar").await;
    assert_eq!(out, "fn bar() {}\n");
}

#[tokio::test]
async fn ast_language_and_capture_over_limit_do_not_echo_through_tool() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lib.rs"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    let language = format!(
        "LEAK{}",
        "x".repeat(super::ast::MAX_AST_LANGUAGE_BYTES + 2048)
    );
    let language_err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "language": language,
                "query": "(function_item name: (identifier) @name)",
                "capture": "name",
                "replacement": "bar"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert_bounded_field_error(&language_err, "language", "LEAK");

    let capture = format!(
        "LEAK{}",
        "a".repeat(super::ast::MAX_AST_CAPTURE_BYTES + 2048)
    );
    let capture_err = run_dyn(
        &EditTool,
        json!({
            "path": "lib.rs",
            "operations": [{
                "type": "ast",
                "query": "(function_item name: (identifier) @name)",
                "capture": capture,
                "replacement": "bar"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert_bounded_field_error(&capture_err, "capture", "LEAK");
}
