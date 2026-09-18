//! Bounded transcript storage and viewport-budgeted materialization.
//!
//! Replacement keeps only the newest blocks. Materialization walks history
//! only until the requested window is filled, clamps oversized offsets, and
//! never walks blocks when the viewport has zero width or zero height.
use mcode_render::{MAX_PLAIN_WIDTH, RenderBlock};

use crate::state::Viewport;

/// Maximum retained transcript blocks.
///
/// A long interactive session should persist history in the session log, not
/// grow an unbounded TUI vector. 1024 blocks covers a busy turn without
/// retaining multi-megabyte tool dumps in the view.
pub const DEFAULT_SCROLLBACK_BLOCKS: usize = 1_024;

/// Width, height, and line offset used to materialize transcript lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MaterializeBudget {
    width: usize,
    height: usize,
    offset: usize,
}

impl MaterializeBudget {
    /// Creates a budget from column, row, and skip counts.
    #[must_use]
    pub const fn new(width: usize, height: usize, offset: usize) -> Self {
        Self {
            width,
            height,
            offset,
        }
    }

    /// Creates a budget from a terminal viewport and a line offset.
    #[must_use]
    pub fn from_viewport(viewport: Viewport, offset: usize) -> Self {
        Self::new(
            usize::from(viewport.width),
            usize::from(viewport.height),
            offset,
        )
    }

    /// Returns the column budget.
    #[must_use]
    pub const fn width(self) -> usize {
        self.width
    }

    /// Returns the row budget.
    #[must_use]
    pub const fn height(self) -> usize {
        self.height
    }

    /// Returns how many leading materialized lines to skip.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }
}

/// One plain line produced under a [`MaterializeBudget`].
#[derive(Debug, Clone, PartialEq)]
pub struct MaterializedLine<'a> {
    text: String,
    block: &'a RenderBlock,
}

impl<'a> MaterializedLine<'a> {
    /// Returns the width-bounded plain text for this row.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the source block used to style this row.
    #[must_use]
    pub const fn block(&self) -> &'a RenderBlock {
        self.block
    }
}

/// Result of budgeted transcript materialization.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterializedView<'a> {
    lines: Vec<MaterializedLine<'a>>,
    blocks_examined: usize,
    offset: usize,
}

impl<'a> MaterializedView<'a> {
    /// Returns the visible lines in display order.
    #[must_use]
    pub fn lines(&self) -> &[MaterializedLine<'a>] {
        &self.lines
    }

    /// Returns how many source blocks were visited.
    ///
    /// Zero-width and zero-height budgets report `0` without touching history.
    #[must_use]
    pub const fn blocks_examined(&self) -> usize {
        self.blocks_examined
    }

    /// Returns the applied offset after clamping to retained history.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }
}

/// Newest-first transcript with a fixed block capacity.
#[derive(Debug, Clone, PartialEq)]
pub struct Scrollback {
    blocks: Vec<RenderBlock>,
    capacity: usize,
    offset: usize,
}

impl Scrollback {
    /// Creates empty scrollback that retains at most `capacity` blocks.
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self {
            blocks: Vec::new(),
            capacity,
            offset: 0,
        }
    }

    /// Returns retained blocks in display order (oldest first).
    #[must_use]
    pub fn blocks(&self) -> &[RenderBlock] {
        &self.blocks
    }

    /// Returns the maximum number of retained blocks.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the number of leading lines skipped at materialize time.
    #[must_use]
    pub const fn offset(&self) -> usize {
        self.offset
    }

    /// Replaces retained blocks, dropping the oldest when over capacity.
    ///
    /// Returns whether the stored sequence or offset changed. Replacement
    /// jumps to the newest tail (`offset == 0`) so a host refresh does not
    /// leave the viewport parked in older history.
    pub fn replace(&mut self, mut blocks: Vec<RenderBlock>) -> bool {
        bound_newest(&mut blocks, self.capacity);
        let changed = self.blocks != blocks || self.offset != 0;
        if changed {
            self.blocks = blocks;
            self.offset = 0;
        }
        changed
    }

    /// Moves the materialized window toward older history when `older_lines` is
    /// positive.
    ///
    /// `budget` supplies the exact transcript width and height. Nonnegative
    /// movement materializes only through the requested candidate window and
    /// clamps to the oldest full window. A zero-sized viewport preserves the
    /// current offset until usable geometry is available. Returns whether the
    /// offset changed.
    pub fn scroll_by(&mut self, older_lines: i32, budget: MaterializeBudget) -> bool {
        if budget.width == 0 || budget.height == 0 {
            return false;
        }

        let candidate = if older_lines >= 0 {
            self.offset
                .saturating_add(usize::try_from(older_lines).unwrap_or(usize::MAX))
        } else {
            self.offset
                .saturating_sub(usize::try_from(older_lines.unsigned_abs()).unwrap_or(usize::MAX))
        };
        let next = if older_lines >= 0 {
            materialize(
                &self.blocks,
                MaterializeBudget::new(budget.width, budget.height, candidate),
            )
            .offset
        } else {
            candidate
        };
        if next == self.offset {
            return false;
        }
        self.offset = next;
        true
    }
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::new(DEFAULT_SCROLLBACK_BLOCKS)
    }
}

/// Materializes transcript lines without exceeding `budget`.
///
/// A zero width or zero height returns no lines and examines no blocks, even
/// when `blocks` is huge. Otherwise blocks are visited from the newest end
/// until `offset + height` newest lines are collected. `offset` skips that
/// many newest lines so `0` shows the tail of history. An oversized offset
/// clamps to the oldest full window.
#[must_use]
pub fn materialize<'a>(
    blocks: &'a [RenderBlock],
    budget: MaterializeBudget,
) -> MaterializedView<'a> {
    if budget.width == 0 || budget.height == 0 {
        return MaterializedView {
            lines: Vec::new(),
            blocks_examined: 0,
            offset: 0,
        };
    }

    let width = budget.width.min(MAX_PLAIN_WIDTH);
    let want = budget.offset.saturating_add(budget.height);
    let mut newest_first: Vec<MaterializedLine<'a>> = Vec::with_capacity(want.min(64));
    let mut examined = 0_usize;

    for (index, block) in blocks.iter().rev().enumerate() {
        if newest_first.len() >= want {
            break;
        }
        examined += 1;
        let plain = block.to_plain_text(width);
        let mut block_lines: Vec<&str> = plain.lines().collect();
        if block_lines.is_empty() {
            block_lines.push("");
        }
        for text in block_lines.into_iter().rev() {
            newest_first.push(MaterializedLine {
                text: text.to_owned(),
                block,
            });
            if newest_first.len() >= want {
                break;
            }
        }
        let older_exists = index + 1 < blocks.len();
        if older_exists && newest_first.len() < want {
            newest_first.push(MaterializedLine {
                text: String::new(),
                block,
            });
        }
    }

    let offset = budget
        .offset
        .min(newest_first.len().saturating_sub(budget.height));
    let visible = newest_first
        .into_iter()
        .skip(offset)
        .take(budget.height)
        .rev()
        .collect();

    MaterializedView {
        lines: visible,
        blocks_examined: examined,
        offset,
    }
}

fn bound_newest(blocks: &mut Vec<RenderBlock>, capacity: usize) {
    if capacity == 0 {
        blocks.clear();
        return;
    }
    if blocks.len() > capacity {
        let extra = blocks.len() - capacity;
        blocks.drain(..extra);
    }
}
