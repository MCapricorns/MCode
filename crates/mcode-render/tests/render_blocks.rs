use mcode_render::{
    Diff, DiffHunk, DiffLine, DiffLineKind, ErrorBlock, MAX_PLAIN_LINES, MAX_PLAIN_WIDTH, Progress,
    ProgressState, RenderBlock, Table, Tree, TreeNode, display_width, next_grapheme_boundary,
    prev_grapheme_boundary, sanitize_terminal_text, truncate_display_width,
};
use serde_json::json;

fn fixture_blocks() -> Vec<RenderBlock> {
    vec![
        RenderBlock::Text("Plain text".into()),
        RenderBlock::Markdown("# Heading\nA **strong** statement.".into()),
        RenderBlock::Diff(Diff::new(
            "src/main.rs",
            vec![DiffHunk::new(
                "@@ -1,2 +1,2 @@",
                vec![
                    DiffLine::new(DiffLineKind::Removed, "let old = true;")
                        .with_line_numbers(Some(1), None),
                    DiffLine::new(DiffLineKind::Added, "let current = true;")
                        .with_line_numbers(None, Some(1)),
                    DiffLine::new(DiffLineKind::Context, "run(current);")
                        .with_line_numbers(Some(2), Some(2)),
                ],
            )],
        )),
        RenderBlock::Table(
            Table::new(
                vec!["File".into(), "State".into()],
                vec![vec!["Cargo.toml".into(), "Ready".into()]],
            )
            .with_caption("Workspace"),
        ),
        RenderBlock::Tree(Tree::new(TreeNode::branch(
            "workspace",
            vec![
                TreeNode::leaf("Cargo.toml"),
                TreeNode::branch("src", vec![TreeNode::leaf("main.rs")]),
            ],
        ))),
        RenderBlock::Progress(
            Progress::running("Indexing files", 3, Some(8)).with_state(ProgressState::Running),
        ),
        RenderBlock::Error(
            ErrorBlock::new("Build failed", "The compiler reported an error.")
                .with_details("Review the first diagnostic.")
                .retryable(true),
        ),
        RenderBlock::Widget(json!({"kind": "sparkline", "points": [1, 3, 2]})),
    ]
}

#[test]
fn every_variant_round_trips_through_json() {
    for block in fixture_blocks() {
        let encoded = serde_json::to_string(&block).expect("render block must serialize");
        let decoded: RenderBlock =
            serde_json::from_str(&encoded).expect("render block must deserialize");
        assert_eq!(decoded, block);
    }
}

#[test]
fn every_variant_has_bounded_plain_text() {
    for block in fixture_blocks() {
        let rendered = block.to_plain_text(24);
        assert!(!rendered.is_empty());
        assert!(
            rendered.lines().all(|line| display_width(line) <= 24),
            "plain fallback exceeded its width: {rendered:?}"
        );
        assert!(!rendered.contains('\u{1b}'));
    }
}

#[test]
fn unicode_width_and_long_lines_are_bounded() {
    let text = format!("Status: {} {}", "ＡＢＣ", "x".repeat(100_000));
    let rendered = RenderBlock::Text(text).to_plain_text(12);

    assert_eq!(display_width("ＡＢＣ"), 6);
    assert!(display_width(&rendered) <= 12);
    assert!(rendered.ends_with('…'));
    assert!(rendered.len() < 64);
}

#[test]
fn truncation_handles_narrow_and_oversized_widths() {
    assert_eq!(truncate_display_width("abcdef", 0), "");
    assert_eq!(truncate_display_width("abcdef", 1), "…");
    assert_eq!(truncate_display_width("abcdef", 4), "abc…");

    let huge = truncate_display_width("short", usize::MAX);
    assert_eq!(huge, "short");
    let capped = RenderBlock::Text("z".repeat(MAX_PLAIN_WIDTH + 10)).to_plain_text(usize::MAX);
    assert_eq!(display_width(&capped), MAX_PLAIN_WIDTH);
}

#[test]
fn grapheme_boundaries_follow_extended_clusters() {
    // Woman scientist is one extended cluster: U+1F469 ZWJ U+1F52C.
    let scientist = "\u{1F469}\u{200D}\u{1F52C}";
    let text = format!("a{scientist}b");
    let last = prev_grapheme_boundary(&text, text.len());
    assert_eq!(&text[last..], "b");
    let cluster = prev_grapheme_boundary(&text, last);
    assert_eq!(&text[cluster..last], scientist);
    assert_eq!(prev_grapheme_boundary(&text, cluster), 0);
    assert_eq!(next_grapheme_boundary(&text, 0), "a".len());
    assert_eq!(next_grapheme_boundary(&text, cluster), last);
    assert_eq!(next_grapheme_boundary(&text, last), text.len());
}

#[test]
fn emoji_sequences_respect_the_display_width_contract() {
    for text in [
        "\u{2764}\u{fe0f}\u{2764}\u{fe0f}",
        "\u{1F469}\u{200D}\u{1F52C}\u{1F469}\u{200D}\u{1F52C}",
        "\u{1F44B}\u{1F3FD}\u{1F44B}\u{1F3FD}",
        "\u{1F1FA}\u{1F1F8}\u{1F1E8}\u{1F1E6}",
    ] {
        for width in 0..=6 {
            let rendered = truncate_display_width(text, width);
            assert!(
                display_width(&rendered) <= width,
                "{text:?} rendered as {rendered:?} at width {width}"
            );
        }
    }

    assert_eq!(
        truncate_display_width("\u{2764}\u{fe0f}\u{2764}\u{fe0f}", 3),
        "\u{2764}\u{fe0f}\u{2026}"
    );
    assert_eq!(
        truncate_display_width("\u{1F469}\u{200D}\u{1F52C}\u{1F469}\u{200D}\u{1F52C}", 3),
        "\u{1F469}\u{200D}\u{1F52C}\u{2026}"
    );
}

#[test]
fn zero_width_runs_cannot_bypass_the_line_bound() {
    let combining_marks = "\u{0301}".repeat(100_000);
    let rendered = RenderBlock::Text(combining_marks).to_plain_text(4);

    assert!(display_width(&rendered) <= 4);
    assert!(rendered.ends_with('\u{2026}'));
    assert!(rendered.chars().count() < 100);
}

#[test]
fn terminal_sequences_are_removed_in_plain_fallback() {
    let styled =
        "\u{1b}[31mred\u{1b}[0m \u{1b}]8;;https://example.invalid\u{1b}\\link\u{1b}]8;;\u{1b}\\";
    assert_eq!(sanitize_terminal_text(styled), "red link");
    assert_eq!(
        RenderBlock::Text(styled.into()).to_plain_text(80),
        "red link"
    );
}

#[test]
fn multiline_output_has_a_fixed_line_budget() {
    let oversized = std::iter::repeat_n("line", MAX_PLAIN_LINES + 5)
        .collect::<Vec<_>>()
        .join("\n");
    let rendered = RenderBlock::Text(oversized).to_plain_text(80);
    let lines = rendered.lines().collect::<Vec<_>>();

    assert_eq!(lines.len(), MAX_PLAIN_LINES);
    assert_eq!(lines.last(), Some(&"[output truncated]"));
}

#[test]
fn fallback_preserves_each_block_semantics() {
    let blocks = fixture_blocks();
    let combined = blocks
        .iter()
        .map(|block| block.to_plain_text(100))
        .collect::<Vec<_>>()
        .join("\n");

    for expected in [
        "Plain text",
        "# Heading",
        "+let current = true;",
        "File | State",
        "`- src",
        "Running: Indexing files [3/8, 37%]",
        "Error: Build failed",
        "Widget:",
    ] {
        assert!(combined.contains(expected), "missing {expected:?}");
    }
}
