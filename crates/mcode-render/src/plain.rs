//! Bounded plain-text rendering and Unicode display-width helpers.
//!
//! The renderer deliberately emits no ANSI styling. It strips terminal
//! control sequences from data, truncates long logical lines by display
//! columns, and applies fixed width and line-count budgets.
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    DiffLineKind, ErrorBlock, Progress, ProgressState, RenderBlock, Table, Tree, TreeNode,
};

/// Maximum accepted display width for one plain-text line.
///
/// The cap prevents an accidental huge allocation when a caller forwards an
/// untrusted terminal width.
pub const MAX_PLAIN_WIDTH: usize = 4_096;

/// Maximum number of lines returned by one plain-text conversion.
///
/// This keeps fallback rendering finite even when a block contains very large
/// tables, trees, or multiline values.
pub const MAX_PLAIN_LINES: usize = 4_096;

/// Maximum tree depth visited by the fallback renderer.
///
/// Deep extension-provided trees are omitted after this level to avoid stack
/// or prefix growth from adversarial input.
const MAX_TREE_DEPTH: usize = 128;

// Allows combining marks while bounding pathological zero-width input.
const MAX_PLAIN_LINE_SCALARS: usize = 16_384;
const ZERO_WIDTH_SCALAR_ALLOWANCE: usize = 64;
const ELLIPSIS: &str = "…";
const OUTPUT_TRUNCATED: &str = "[output truncated]";
const NESTED_NODES_OMITTED: &str = "[nested nodes omitted]";

/// Returns the terminal display width of `text` in columns.
///
/// Width follows the non-CJK ambiguous-width policy from `unicode-width`;
/// full-width Latin, Hangul, and other wide code points still occupy two
/// columns.
#[must_use]
pub fn display_width(text: impl AsRef<str>) -> usize {
    UnicodeWidthStr::width(text.as_ref())
}

/// Returns the start of the grapheme cluster before `byte_index`.
///
/// `byte_index` is clamped to `text.len()`. The result is always a grapheme
/// boundary so backspace can delete one user-visible cluster.
#[must_use]
pub fn prev_grapheme_boundary(text: &str, byte_index: usize) -> usize {
    let byte_index = byte_index.min(text.len());
    if byte_index == 0 {
        return 0;
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .rfind(|&index| index < byte_index)
        .unwrap_or(0)
}

/// Returns the start of the grapheme cluster after `byte_index`.
///
/// `byte_index` is clamped to `text.len()`. An index inside a cluster snaps
/// to that cluster's end so a caret cannot rest inside a grapheme.
#[must_use]
pub fn next_grapheme_boundary(text: &str, byte_index: usize) -> usize {
    let byte_index = byte_index.min(text.len());
    if byte_index == text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .find_map(|(index, cluster)| {
            if index >= byte_index || index + cluster.len() > byte_index {
                Some(index + cluster.len())
            } else {
                None
            }
        })
        .unwrap_or(text.len())
}

/// Truncates one visible line to at most `width` display columns.
///
/// A truncated line ends in `…` when at least one column is available. The
/// function preserves extended grapheme clusters within its scalar budget and
/// measures both input and output with [`display_width`]. Control characters
/// count according to `unicode-width`; call [`sanitize_terminal_text`] first
/// for untrusted terminal-facing data.
#[must_use]
pub fn truncate_display_width(text: impl AsRef<str>, width: usize) -> String {
    let text = text.as_ref();
    let width = width.min(MAX_PLAIN_WIDTH);
    if width == 0 {
        return String::new();
    }
    let scalar_limit = width
        .saturating_mul(4)
        .saturating_add(ZERO_WIDTH_SCALAR_ALLOWANCE)
        .min(MAX_PLAIN_LINE_SCALARS);
    let (bounded, exceeds_scalar_limit) = match text.char_indices().nth(scalar_limit) {
        Some((end, _)) => (&text[..end], true),
        None => (text, false),
    };
    if !exceeds_scalar_limit && display_width(text) <= width {
        return text.to_owned();
    }

    let ellipsis_width = display_width(ELLIPSIS);
    if ellipsis_width > width {
        return String::new();
    }
    let content_width = width - ellipsis_width;
    let mut rendered = String::new();
    let mut grapheme_ends = Vec::new();
    let mut used = 0_usize;

    for grapheme in bounded.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        if used.saturating_add(grapheme_width) > content_width {
            break;
        }
        rendered.push_str(grapheme);
        grapheme_ends.push(rendered.len());
        used = used.saturating_add(grapheme_width);
    }

    rendered.push_str(ELLIPSIS);
    while display_width(&rendered) > width {
        rendered.truncate(rendered.len() - ELLIPSIS.len());
        if grapheme_ends.pop().is_none() {
            return ELLIPSIS.to_owned();
        }
        rendered.truncate(grapheme_ends.last().copied().unwrap_or(0));
        rendered.push_str(ELLIPSIS);
    }
    rendered
}

/// Removes terminal control sequences while preserving readable text.
///
/// CSI styling, OSC strings, and related escaped strings are discarded.
/// Newlines are retained, carriage returns become safe line breaks, tabs
/// become four spaces, and other control characters are removed. This
/// function never interprets or executes a sequence.
#[must_use]
pub fn sanitize_terminal_text(text: impl AsRef<str>) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Text,
        Escape,
        Csi,
        String,
        StringEscape,
    }

    let mut state = State::Text;
    let mut output = String::with_capacity(text.as_ref().len());
    let mut characters = text.as_ref().chars().peekable();

    while let Some(character) = characters.next() {
        match state {
            State::Text => match character {
                '\u{1b}' => state = State::Escape,
                '\u{009b}' => state = State::Csi,
                '\u{009d}' | '\u{0090}' | '\u{009e}' | '\u{009f}' => {
                    state = State::String;
                }
                '\r' => {
                    if characters.peek() != Some(&'\n') {
                        output.push('\n');
                    }
                }
                '\n' => output.push('\n'),
                '\t' => output.push_str("    "),
                _ if !character.is_control() => output.push(character),
                _ => {}
            },
            State::Escape => match character {
                '[' => state = State::Csi,
                ']' | 'P' | '^' | '_' => state = State::String,
                _ => state = State::Text,
            },
            State::Csi => {
                if ('@'..='~').contains(&character) {
                    state = State::Text;
                }
            }
            State::String => match character {
                '\u{0007}' | '\u{009c}' => state = State::Text,
                '\u{1b}' => state = State::StringEscape,
                _ => {}
            },
            State::StringEscape => {
                state = if character == '\\' {
                    State::Text
                } else {
                    State::String
                };
            }
        }
    }

    output
}

pub(crate) fn render(block: &RenderBlock, width: usize) -> String {
    let mut writer = PlainWriter::new(width);
    if writer.width == 0 {
        return String::new();
    }

    match block {
        RenderBlock::Text(text) | RenderBlock::Markdown(text) => writer.push_text(text),
        RenderBlock::Diff(diff) => {
            writer.push_line(format!("--- {}", diff.path));
            writer.push_line(format!("+++ {}", diff.path));
            for hunk in &diff.hunks {
                if !writer.has_capacity() {
                    writer.mark_truncated();
                    break;
                }
                writer.push_line(&hunk.header);
                for line in &hunk.lines {
                    if !writer.has_capacity() {
                        writer.mark_truncated();
                        break;
                    }
                    let marker = match line.kind {
                        DiffLineKind::Context => ' ',
                        DiffLineKind::Added => '+',
                        DiffLineKind::Removed => '-',
                    };
                    writer.push_line(format!("{marker}{}", line.text));
                }
            }
        }
        RenderBlock::Table(table) => render_table(&mut writer, table),
        RenderBlock::Tree(tree) => render_tree(&mut writer, tree),
        RenderBlock::Progress(progress) => render_progress(&mut writer, progress),
        RenderBlock::Error(error) => render_error(&mut writer, error),
        RenderBlock::Widget(value) => {
            writer.push_line("Widget:");
            match serde_json::to_string_pretty(value) {
                Ok(serialized) => writer.push_text(serialized),
                Err(_) => writer.push_line("[widget data unavailable]"),
            }
        }
    }

    writer.finish()
}

fn render_table(writer: &mut PlainWriter, table: &Table) {
    if let Some(caption) = &table.caption {
        writer.push_line(caption);
    }
    if !table.headers.is_empty() {
        let header = join_cells(&table.headers, writer.width);
        let separator_width = display_width(&header).max(1).min(writer.width);
        writer.push_line(header);
        writer.push_line("-".repeat(separator_width));
    }
    for row in &table.rows {
        if !writer.has_capacity() {
            writer.mark_truncated();
            break;
        }
        writer.push_line(join_cells(row, writer.width));
    }
    if table.headers.is_empty() && table.rows.is_empty() {
        writer.push_line("(empty table)");
    }
}

fn join_cells(cells: &[String], width: usize) -> String {
    let mut joined = String::new();
    for (index, cell) in cells.iter().enumerate() {
        let separator = if index == 0 { "" } else { " | " };
        let remaining = width.saturating_sub(display_width(&joined));
        if remaining == 0 {
            break;
        }
        let separator = truncate_display_width(separator, remaining);
        joined.push_str(&separator);

        let remaining = width.saturating_sub(display_width(&joined));
        if remaining == 0 {
            break;
        }
        let clean = sanitize_terminal_text(cell).replace('\n', " ");
        joined.push_str(&truncate_display_width(clean, remaining));
    }
    joined
}

fn render_tree(writer: &mut PlainWriter, tree: &Tree) {
    struct Pending<'a> {
        node: &'a TreeNode,
        prefix: String,
        is_last: bool,
        is_root: bool,
        depth: usize,
    }

    let mut pending = vec![Pending {
        node: &tree.root,
        prefix: String::new(),
        is_last: true,
        is_root: true,
        depth: 0,
    }];

    while let Some(current) = pending.pop() {
        if !writer.has_capacity() {
            writer.mark_truncated();
            break;
        }

        if current.is_root {
            writer.push_line(&current.node.label);
        } else {
            let connector = if current.is_last { "`- " } else { "|- " };
            writer.push_line(format!(
                "{}{}{}",
                current.prefix, connector, current.node.label
            ));
        }

        if current.depth >= MAX_TREE_DEPTH {
            if !current.node.children.is_empty() {
                writer.push_line(format!("{}  {NESTED_NODES_OMITTED}", current.prefix));
            }
            continue;
        }

        let child_prefix = if current.is_root {
            String::new()
        } else {
            format!(
                "{}{}",
                current.prefix,
                if current.is_last { "   " } else { "|  " }
            )
        };
        let child_count = current.node.children.len();
        let available_slots = writer.remaining_capacity().saturating_sub(pending.len());
        let visible_count = child_count.min(available_slots);
        if visible_count < child_count {
            writer.mark_truncated();
        }
        for (index, child) in current.node.children[..visible_count]
            .iter()
            .enumerate()
            .rev()
        {
            pending.push(Pending {
                node: child,
                prefix: child_prefix.clone(),
                is_last: index + 1 == visible_count,
                is_root: false,
                depth: current.depth + 1,
            });
        }
    }
}

fn render_progress(writer: &mut PlainWriter, progress: &Progress) {
    let state = match progress.state {
        ProgressState::Pending => "Pending",
        ProgressState::Running => "Running",
        ProgressState::Succeeded => "Complete",
        ProgressState::Failed => "Failed",
    };
    match progress.total {
        Some(total) if total > 0 => {
            let completed = progress.current.min(total);
            let percent = (u128::from(completed) * 100) / u128::from(total);
            writer.push_line(format!(
                "{state}: {} [{}/{}, {percent}%]",
                progress.label, progress.current, total
            ));
        }
        Some(total) => writer.push_line(format!(
            "{state}: {} [{}/{}]",
            progress.label, progress.current, total
        )),
        None => writer.push_line(format!(
            "{state}: {} [{}]",
            progress.label, progress.current
        )),
    }
}

fn render_error(writer: &mut PlainWriter, error: &ErrorBlock) {
    writer.push_line(format!("Error: {}", error.title));
    writer.push_text(&error.message);
    if let Some(details) = &error.details {
        writer.push_line("Details:");
        writer.push_text(details);
    }
    if error.retryable {
        writer.push_line("Retry available.");
    }
}

struct PlainWriter {
    width: usize,
    lines: Vec<String>,
    truncated: bool,
}

impl PlainWriter {
    fn new(width: usize) -> Self {
        Self {
            width: width.min(MAX_PLAIN_WIDTH),
            lines: Vec::new(),
            truncated: false,
        }
    }

    fn has_capacity(&self) -> bool {
        self.lines.len() < MAX_PLAIN_LINES
    }

    fn remaining_capacity(&self) -> usize {
        MAX_PLAIN_LINES.saturating_sub(self.lines.len())
    }

    fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    fn push_text(&mut self, text: impl AsRef<str>) {
        let clean = sanitize_terminal_text(text);
        for line in clean.split('\n') {
            if !self.has_capacity() {
                self.mark_truncated();
                break;
            }
            self.push_clean_line(line);
        }
    }

    fn push_line(&mut self, line: impl AsRef<str>) {
        if !self.has_capacity() {
            self.mark_truncated();
            return;
        }
        let clean = sanitize_terminal_text(line);
        let mut lines = clean.split('\n');
        if let Some(first) = lines.next() {
            self.push_clean_line(first);
        }
        for rest in lines {
            if !self.has_capacity() {
                self.mark_truncated();
                break;
            }
            self.push_clean_line(rest);
        }
    }

    fn push_clean_line(&mut self, line: &str) {
        self.lines.push(truncate_display_width(line, self.width));
    }

    fn finish(mut self) -> String {
        if self.truncated {
            let marker = truncate_display_width(OUTPUT_TRUNCATED, self.width);
            if let Some(last) = self.lines.last_mut() {
                *last = marker;
            } else if self.width > 0 {
                self.lines.push(marker);
            }
        }
        self.lines.join("\n")
    }
}
