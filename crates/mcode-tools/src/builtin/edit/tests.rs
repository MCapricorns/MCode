use super::*;
use crate::builtin::fs_io::FileAccess;
use crate::builtin::test_support::{ctx_at, run_dyn, text_of};
use crate::tool::{Tool, ToolError};
use serde_json::json;

#[tokio::test]
async fn replaces_unique_string() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("code.rs");
    std::fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &EditTool,
        json!({
            "path": "code.rs",
            "old_string": "println!(\"hello\");",
            "new_string": "println!(\"goodbye\");",
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    assert!(text_of(&result).starts_with("Edited"));

    let on_disk = std::fs::read_to_string(&file).unwrap();
    assert_eq!(on_disk, "fn main() {\n    println!(\"goodbye\");\n}\n");
    let details = result.details.as_ref().unwrap();
    assert_eq!(details["replacements"], 1);
    assert!(
        details["revision"]
            .as_str()
            .unwrap()
            .starts_with("mcode-rev1-")
    );
    assert!(!text_of(&result).contains("println!"));
}

#[tokio::test]
async fn missing_string_errors_with_guidance() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "aaa\nbbb\n").unwrap();
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &EditTool,
        json!({"path": "f.txt", "old_string": "zzz", "new_string": "y"}),
        &ctx,
    )
    .await
    .unwrap_err();
    match err {
        ToolError::Execution(msg) => {
            assert!(msg.contains("not found"), "{msg}");
        }
        other => panic!("expected Execution, got {other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "aaa\nbbb\n"
    );
}

#[tokio::test]
async fn ambiguous_string_errors_asking_for_context() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x\n  item\ny\n  item\n").unwrap();
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &EditTool,
        json!({"path": "f.txt", "old_string": "item", "new_string": "thing"}),
        &ctx,
    )
    .await
    .unwrap_err();
    match err {
        ToolError::Execution(msg) => {
            assert!(msg.contains("2 times"), "{msg}");
            assert!(msg.contains("unique"), "{msg}");
        }
        other => panic!("expected Execution, got {other:?}"),
    }
}

#[tokio::test]
async fn context_disambiguates() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x\n  item\ny\n  item\n").unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "old_string": "x\n  item",
            "new_string": "x\n  thing",
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "x\n  thing\ny\n  item\n"
    );
}

#[tokio::test]
async fn missing_file_is_an_execution_error() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &EditTool,
        json!({"path": "absent.txt", "old_string": "a", "new_string": "b"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)));
}

#[tokio::test]
async fn empty_old_string_is_invalid_args() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "content").unwrap();
    let ctx = ctx_at(dir.path());

    let err = run_dyn(
        &EditTool,
        json!({"path": "f.txt", "old_string": "", "new_string": "b"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)));
}

#[tokio::test]
async fn multi_byte_strings_count_bytes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("utf8.txt"), "héllo wörld").unwrap();
    let ctx = ctx_at(dir.path());

    let result = run_dyn(
        &EditTool,
        json!({
            "path": "utf8.txt",
            "old_string": "héllo",
            "new_string": "hola",
        }),
        &ctx,
    )
    .await
    .unwrap();
    let details = result.details.unwrap();
    assert_eq!(details["bytes_before"], "héllo wörld".len());
    assert_eq!(details["bytes_after"], "hola wörld".len());
}

#[tokio::test]
async fn expected_revision_mismatch_does_not_publish() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "alpha").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "expected_revision": "mcode-rev1-deadbeef",
            "old_string": "alpha",
            "new_string": "beta",
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "alpha"
    );
}

#[tokio::test]
async fn matching_expected_revision_publishes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "alpha").unwrap();
    let ctx = ctx_at(dir.path());
    let read = crate::builtin::test_support::run_dyn(
        &crate::builtin::ReadTool,
        json!({"path": "f.txt"}),
        &ctx,
    )
    .await
    .unwrap();
    let revision = read.details.unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "expected_revision": revision,
            "old_string": "alpha",
            "new_string": "beta",
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "beta"
    );
}

#[tokio::test]
async fn literal_all_and_nth() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a-a-a").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "literal",
                "pattern": "a",
                "replacement": "b",
                "occurrence": "nth",
                "n": 2
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "a-b-a"
    );
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "literal",
                "pattern": "a",
                "replacement": "c",
                "occurrence": "all"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "c-b-c"
    );
}

#[tokio::test]
async fn multi_pattern_aho_corasick() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "fox and cat").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "literal",
                "patterns": ["fox", "cat"],
                "replacements": ["dog", "owl"],
                "occurrence": "all"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "dog and owl"
    );
}

#[tokio::test]
async fn regex_capture_replacement() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "fn foo() {}\n").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "regex",
                "pattern": "fn ([a-z]+)",
                "replacement": "fn new_$1",
                "occurrence": "unique"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "fn new_foo() {}\n"
    );
}

#[tokio::test]
async fn regex_lookaround_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "ab").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "regex",
                "pattern": "a(?=b)",
                "replacement": "x"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "ab"
    );
}

#[tokio::test]
async fn line_range_expected_text_and_hash() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\n").unwrap();
    let ctx = ctx_at(dir.path());
    let range = "two\n";
    let hash = blake3::hash(range.as_bytes()).to_hex();
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "line_range",
                "start_line": 2,
                "end_line": 2,
                "expected_text": range,
                "expected_hash": hash.as_str(),
                "replacement": "TWO\n"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "one\nTWO\nthree\n"
    );
}

#[tokio::test]
async fn overlap_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "foobar").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [
                {"type": "literal", "pattern": "foo", "replacement": "x"},
                {"type": "literal", "pattern": "foobar", "replacement": "y"}
            ]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("overlap"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "foobar"
    );
}

#[tokio::test]
async fn preserves_bom_crlf_and_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let original = format!("{UTF8_BOM}alpha\r\nbeta\r\n");
    std::fs::write(dir.path().join("f.txt"), &original).unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "old_string": "beta",
            "new_string": "gamma",
        }),
        &ctx,
    )
    .await
    .unwrap();
    let on_disk = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
    assert_eq!(on_disk, format!("{UTF8_BOM}alpha\r\ngamma\r\n"));
}

#[tokio::test]
async fn pre_cancel_does_not_publish() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "keep").unwrap();
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let ctx = ctx_at(dir.path()).with_cancel(token);
    let err = run_dyn(
        &EditTool,
        json!({"path": "f.txt", "old_string": "keep", "new_string": "gone"}),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::Execution(_)), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn prefix_overlap_in_one_literal_op_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "foobar").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "literal",
                "patterns": ["foo", "foobar"],
                "replacements": ["x", "y"],
                "occurrence": "all"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("overlap"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "foobar"
    );
}

#[tokio::test]
async fn exact_replace_can_drop_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "beta\n").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({"path": "f.txt", "old_string": "beta\n", "new_string": "beta"}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "beta"
    );
}

#[tokio::test]
async fn capture_expansion_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let body = "x".repeat(64 * 1024);
    std::fs::write(dir.path().join("f.txt"), &body).unwrap();
    let ctx = ctx_at(dir.path());
    let template = "$0".repeat(32);
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "regex",
                "pattern": ".+",
                "replacement": template,
                "occurrence": "unique"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        body
    );
}

#[tokio::test]
async fn nth_pick_rejects_overlapping_candidates() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "foobar").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "literal",
                "patterns": ["foo", "foobar"],
                "replacements": ["x", "y"],
                "occurrence": "nth",
                "n": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("overlap"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "foobar"
    );
}

#[tokio::test]
async fn self_overlapping_pattern_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "aaa").unwrap();
    let ctx = ctx_at(dir.path());
    for patterns in [
        vec!["aa".to_owned()],
        vec!["aa".to_owned(), "zz".to_owned()],
    ] {
        let replacements = vec!["b".to_owned(); patterns.len()];
        let err = run_dyn(
            &EditTool,
            json!({
                "path": "f.txt",
                "operations": [{
                    "type": "literal",
                    "patterns": patterns,
                    "replacements": replacements,
                    "occurrence": "all"
                }]
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("overlap"), "{err}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "aaa"
    );
}

#[tokio::test]
async fn literal_all_aggregate_budget_rejects() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a".repeat(513)).unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "literal",
                "pattern": "a",
                "replacement": "b".repeat(16 * 1024),
                "occurrence": "all"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("planned replacements exceed"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "a".repeat(513)
    );
}

#[tokio::test]
async fn regex_replacement_uses_longest_capture_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "alpha beta").unwrap();
    let ctx = ctx_at(dir.path());
    // `$1a` references the nonexistent group named `1a` — it does not
    // expand to group 1 followed by a literal `a`.
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "regex",
                "pattern": "(alpha) (beta)",
                "replacement": "[$1a]",
                "occurrence": "unique"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "[]"
    );
}

#[tokio::test]
async fn exact_write_boundary_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let limit = crate::builtin::fs_io::MAX_WRITE_BYTES;
    let plain = format!("x{}", "a".repeat(limit - 1));
    std::fs::write(dir.path().join("plain.txt"), &plain).unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({"path": "plain.txt", "old_string": "x", "new_string": "y"}),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("plain.txt")).unwrap().len(),
        limit
    );

    // A preserved BOM must not consume budget headroom either: re-insertion
    // only restores bytes that were already part of the snapshot.
    let bom_body = format!("y{}", "b".repeat(limit - 4));
    std::fs::write(dir.path().join("bom.txt"), format!("\u{feff}{bom_body}")).unwrap();
    run_dyn(
        &EditTool,
        json!({"path": "bom.txt", "old_string": "y", "new_string": "z"}),
        &ctx,
    )
    .await
    .unwrap();
    let published = std::fs::read(dir.path().join("bom.txt")).unwrap();
    assert_eq!(published.len(), limit);
    assert!(published.starts_with("\u{feff}".as_bytes()));
}

#[tokio::test]
async fn empty_file_line_range_1_1_replaces_the_single_empty_line() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "line_range",
                "start_line": 1,
                "end_line": 1,
                "expected_text": "",
                "replacement": "seed"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "seed"
    );

    // The empty file still has exactly one line; 2-2 stays out of range.
    std::fs::write(dir.path().join("g.txt"), "").unwrap();
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "g.txt",
            "operations": [{
                "type": "line_range",
                "start_line": 2,
                "end_line": 2,
                "expected_text": "",
                "replacement": "x"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("outside the file (1 lines)"),
        "{err}"
    );
}

#[tokio::test]
async fn file_access_is_existing_content() {
    assert_eq!(EditTool.file_access(), Some(FileAccess::ExistingContent));
    assert!(<EditTool as Tool>::requires_file_preflight(&EditTool));
}

#[test]
fn debug_redacts_payloads() {
    let args = EditArgs {
        path: "secret.rs".into(),
        expected_revision: None,
        old_string: Some("password".into()),
        new_string: Some("hunter2".into()),
        operations: None,
    };
    let rendered = format!("{args:?}");
    assert!(rendered.contains("secret.rs"));
    assert!(!rendered.contains("password"));
    assert!(!rendered.contains("hunter2"));
}
