//! Vendor-neutral render descriptions for MCode.
//!
//! [`RenderBlock`] is plain serializable data. It contains no callbacks,
//! executable behavior, terminal handles, or backend-specific styling. Every
//! block also has a bounded, control-sequence-free plain-text representation
//! for headless and limited-capability consumers.
mod block;
mod plain;

#[doc(inline)]
pub use block::{
    Diff, DiffHunk, DiffLine, DiffLineKind, ErrorBlock, Progress, ProgressState, RenderBlock,
    Table, Tree, TreeNode,
};
#[doc(inline)]
pub use plain::{
    MAX_PLAIN_LINES, MAX_PLAIN_WIDTH, display_width, next_grapheme_boundary,
    prev_grapheme_boundary, sanitize_terminal_text, truncate_display_width,
};
