//! Serializable render block data types.
//!
//! These types describe presentation intent without selecting a terminal,
//! web, or headless renderer. Fields remain owned so blocks can cross task,
//! process, and persistence boundaries through serde.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A vendor-neutral unit of renderable content.
///
/// The enum uses an explicitly tagged serde representation so stored values
/// remain self-describing. Unknown custom presentation data belongs in
/// [`RenderBlock::Widget`]; consumers that do not understand it can use
/// [`RenderBlock::to_plain_text`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(tag = "type", content = "content", rename_all = "snake_case")]
pub enum RenderBlock {
    /// Unformatted text.
    Text(String),
    /// Markdown source for capable consumers.
    Markdown(String),
    /// A structured file diff.
    Diff(Diff),
    /// A table with optional headers.
    Table(Table),
    /// A hierarchical tree.
    Tree(Tree),
    /// Progress for a bounded or unbounded operation.
    Progress(Progress),
    /// A user-facing error description.
    Error(ErrorBlock),
    /// Custom widget data owned by an extension or another frontend.
    Widget(Value),
}

impl RenderBlock {
    /// Converts this block to width-bounded plain text.
    ///
    /// Every emitted line is at most `width` terminal columns, ANSI and OSC
    /// control sequences are removed, and output is capped by
    /// [`crate::MAX_PLAIN_LINES`]. A `width` of zero produces an empty string;
    /// larger values are capped by [`crate::MAX_PLAIN_WIDTH`].
    #[must_use]
    pub fn to_plain_text(&self, width: usize) -> String {
        crate::plain::render(self, width)
    }
}

/// A diff associated with one display path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    /// Display path of the changed resource.
    pub path: String,
    /// Ordered diff hunks.
    pub hunks: Vec<DiffHunk>,
}

impl Diff {
    /// Creates a diff for `path` with the supplied hunks.
    pub fn new(path: impl Into<String>, hunks: Vec<DiffHunk>) -> Self {
        Self {
            path: path.into(),
            hunks,
        }
    }
}

/// One contiguous hunk in a [`Diff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    /// Display header, commonly an `@@` range description.
    pub header: String,
    /// Ordered lines in the hunk.
    pub lines: Vec<DiffLine>,
}

impl DiffHunk {
    /// Creates a hunk with `header` and ordered lines.
    pub fn new(header: impl Into<String>, lines: Vec<DiffLine>) -> Self {
        Self {
            header: header.into(),
            lines,
        }
    }
}

/// A semantic line in a [`DiffHunk`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    /// Whether the line is context, added, or removed.
    pub kind: DiffLineKind,
    /// Optional one-based line number in the old resource.
    pub old_line: Option<u32>,
    /// Optional one-based line number in the new resource.
    pub new_line: Option<u32>,
    /// Line content without a diff marker.
    pub text: String,
}

impl DiffLine {
    /// Creates a diff line without source line numbers.
    pub fn new(kind: DiffLineKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            old_line: None,
            new_line: None,
            text: text.into(),
        }
    }

    /// Attaches optional old and new source line numbers.
    #[must_use]
    pub const fn with_line_numbers(mut self, old_line: Option<u32>, new_line: Option<u32>) -> Self {
        self.old_line = old_line;
        self.new_line = new_line;
        self
    }
}

/// The semantic role of a diff line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    /// An unchanged context line.
    Context,
    /// A newly added line.
    Added,
    /// A removed line.
    Removed,
}

/// Tabular data with rows kept in source order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    /// Optional caption shown before the table.
    pub caption: Option<String>,
    /// Column headings; an empty vector means no heading row.
    pub headers: Vec<String>,
    /// Cells grouped by row; ragged rows are valid.
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Creates an uncaptioned table.
    pub fn new(headers: Vec<String>, rows: Vec<Vec<String>>) -> Self {
        Self {
            caption: None,
            headers,
            rows,
        }
    }

    /// Attaches a caption.
    #[must_use]
    pub fn with_caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }
}

/// A rooted hierarchy of labeled nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    /// Root node rendered first.
    pub root: TreeNode,
}

impl Tree {
    /// Creates a tree from `root`.
    pub const fn new(root: TreeNode) -> Self {
        Self { root }
    }
}

/// One labeled node in a [`Tree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeNode {
    /// User-visible node label.
    pub label: String,
    /// Child nodes in display order.
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    /// Creates a leaf node.
    pub fn leaf(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            children: Vec::new(),
        }
    }

    /// Creates a node with ordered children.
    pub fn branch(label: impl Into<String>, children: Vec<Self>) -> Self {
        Self {
            label: label.into(),
            children,
        }
    }
}

/// Progress information for one operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// User-visible operation label.
    pub label: String,
    /// Completed units.
    pub current: u64,
    /// Total units, or `None` when the operation is unbounded.
    pub total: Option<u64>,
    /// Current lifecycle state.
    pub state: ProgressState,
}

impl Progress {
    /// Creates running progress.
    pub fn running(label: impl Into<String>, current: u64, total: Option<u64>) -> Self {
        Self {
            label: label.into(),
            current,
            total,
            state: ProgressState::Running,
        }
    }

    /// Changes the lifecycle state.
    #[must_use]
    pub const fn with_state(mut self, state: ProgressState) -> Self {
        self.state = state;
        self
    }
}

/// Lifecycle state of a [`Progress`] block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum ProgressState {
    /// Work has not started.
    Pending,
    /// Work is in progress.
    Running,
    /// Work completed successfully.
    Succeeded,
    /// Work completed with a failure.
    Failed,
}

/// Structured user-facing error content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBlock {
    /// Short error heading.
    pub title: String,
    /// Primary error message.
    pub message: String,
    /// Optional additional diagnostic text.
    pub details: Option<String>,
    /// Whether the originating operation may be retried.
    pub retryable: bool,
}

impl ErrorBlock {
    /// Creates a non-retryable error without additional details.
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            details: None,
            retryable: false,
        }
    }

    /// Attaches additional diagnostic text.
    #[must_use]
    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// Marks whether retrying the operation is appropriate.
    #[must_use]
    pub const fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}
