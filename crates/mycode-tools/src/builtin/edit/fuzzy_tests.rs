use super::*;
use crate::builtin::test_support::{ctx_at, run_dyn};
use serde_json::json;

#[tokio::test]
async fn fuzzy_unique_best_with_margin_accepts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello world\nunrelated\n").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "hello wrold",
                "replacement": "hello earth",
                "max_distance": 2
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "hello earth\nunrelated\n"
    );
}

#[tokio::test]
async fn fuzzy_matches_a_single_token_merge() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "abcd\n").unwrap();
    let ctx = ctx_at(dir.path());
    run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "ab cd",
                "replacement": "matched",
                "max_distance": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "matched\n"
    );
}

#[tokio::test]
async fn fuzzy_ambiguous_rejects_with_bounded_preview() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "hello world\nhello world\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "hello world",
                "replacement": "x",
                "max_distance": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unique"), "{msg}");
    assert!(msg.contains("near-misses"), "{msg}");
    assert!(msg.contains("hello world"), "{msg}");
    assert!(
        msg.len() < 8 * 1024,
        "preview must stay bounded, got {} bytes",
        msg.len()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "hello world\nhello world\n"
    );
}

#[tokio::test]
async fn fuzzy_distance_overflow_rejects_with_bounded_preview() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!("abxxxx{}\n", "z".repeat(200));
    std::fs::write(dir.path().join("f.txt"), &source).unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "abcdef",
                "replacement": "y",
                "max_distance": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("max_distance"), "{msg}");
    assert!(msg.contains("near-misses"), "{msg}");
    assert!(msg.contains("line 1 d>1"), "{msg}");
    assert!(msg.contains("abxxxx"), "{msg}");
    assert!(msg.contains('…'), "preview must be truncated: {msg}");
    assert!(
        msg.len() < 4 * 1024,
        "preview must stay bounded, got {} bytes",
        msg.len()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        source
    );
}

#[tokio::test]
async fn fuzzy_preview_anchors_after_long_unicode_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let source = format!("{} abcdefzz {}\n", "界".repeat(40), "z".repeat(200));
    std::fs::write(dir.path().join("f.txt"), &source).unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "abcdefgh",
                "replacement": "changed",
                "max_distance": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    let msg = err.to_string();

    assert!(msg.contains("line 1 d=2: …abcdefzz"), "{msg}");
    assert!(
        msg.matches('…').count() >= 2,
        "both omitted prefix and suffix must be marked: {msg}"
    );
    assert!(
        msg.len() < super::MAX_DIFF_SUMMARY_BYTES,
        "preview must stay bounded, got {} bytes",
        msg.len()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        source
    );
}

#[tokio::test]
async fn fuzzy_occurrence_field_is_rejected_without_editing() {
    let dir = tempfile::tempdir().unwrap();
    let source = "hello world\n";
    std::fs::write(dir.path().join("f.txt"), source).unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "hello worle",
                "replacement": "changed",
                "max_distance": 1,
                "occurrence": "all"
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();

    assert!(
        matches!(err, crate::tool::ToolError::InvalidArgs(_)),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        source
    );
}

#[tokio::test]
async fn fuzzy_does_not_silently_exact_fallback() {
    let dir = tempfile::tempdir().unwrap();
    // Exact unique would replace the first line; fuzzy must refuse because
    // the runner-up is within the length-scaled margin of the exact hit.
    std::fs::write(dir.path().join("f.txt"), "hello world\nhello worle\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "hello world",
                "replacement": "changed",
                "max_distance": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("unique") || err.to_string().contains("margin"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "hello world\nhello worle\n"
    );
}

#[tokio::test]
async fn fuzzy_margin_considers_runner_up_beyond_user_cap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "abcdefgi\nabcdefzz\n").unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "abcdefgh",
                "replacement": "changed",
                "max_distance": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("margin"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "abcdefgi\nabcdefzz\n"
    );
}

#[tokio::test]
async fn fuzzy_rejects_high_token_density_before_scoring() {
    let dir = tempfile::tempdir().unwrap();
    let source = ".".repeat(super::fuzzy::MAX_FUZZY_TOKENS + 1);
    std::fs::write(dir.path().join("f.txt"), &source).unwrap();
    let ctx = ctx_at(dir.path());
    let err = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "abcdefgh",
                "replacement": "changed",
                "max_distance": 1
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("tokens"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        source
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fuzzy_planning_keeps_current_thread_runtime_responsive() {
    let dir = tempfile::tempdir().unwrap();
    let source = "a ".repeat(400);
    std::fs::write(dir.path().join("f.txt"), &source).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let ctx = ctx_at(dir.path()).with_cancel(cancel.clone());
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        cancel.cancel();
    });

    let error = run_dyn(
        &EditTool,
        json!({
            "path": "f.txt",
            "operations": [{
                "type": "fuzzy",
                "pattern": "a ".repeat(128),
                "replacement": "changed",
                "max_distance": 3
            }]
        }),
        &ctx,
    )
    .await
    .unwrap_err();
    canceller.await.unwrap();

    assert!(error.to_string().contains("cancelled"), "{error}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        source
    );
}
