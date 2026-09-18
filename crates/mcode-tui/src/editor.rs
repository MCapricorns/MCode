//! Grapheme-aware multiline input buffer.
//!
//! The editor stores UTF-8 text and keeps the caret on an extended grapheme
//! boundary. Display width is measured with [`mcode_render::display_width`]
//! (unicode-width). The type performs no I/O.
use mcode_render::{display_width, prev_grapheme_boundary, sanitize_terminal_text};

/// Multiline input buffer with a grapheme-cluster caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineEditor {
    buffer: String,
    /// Byte offset of the caret; always a grapheme boundary.
    cursor: usize,
}

impl LineEditor {
    /// Creates an empty editor with the caret at the start.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    /// Returns the full input buffer, including newlines.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.buffer
    }

    /// Returns whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Returns the caret byte offset.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the caret `(line, display_column)` measured with unicode-width.
    #[must_use]
    pub fn caret_line_column(&self) -> (usize, usize) {
        let mut remaining = self.cursor;
        for (index, line) in self.buffer.split('\n').enumerate() {
            if remaining <= line.len() {
                return (index, display_width(&line[..remaining]));
            }
            remaining = remaining.saturating_sub(line.len().saturating_add(1));
        }
        let lines = self.buffer.split('\n').count().saturating_sub(1);
        (lines, 0)
    }

    /// Returns the unicode-width display width of the whole buffer.
    #[must_use]
    pub fn display_width(&self) -> usize {
        display_width(&self.buffer)
    }

    /// Inserts `character` at the caret.
    ///
    /// Control characters other than newline are ignored so a host cannot
    /// inject terminal sequences through typed input.
    pub fn insert(&mut self, character: char) -> bool {
        if character.is_control() && character != '\n' {
            return false;
        }
        self.buffer.insert(self.cursor, character);
        self.cursor += character.len_utf8();
        true
    }

    /// Inserts a newline at the caret.
    pub fn insert_newline(&mut self) -> bool {
        self.insert('\n')
    }

    /// Inserts sanitized paste text at the caret.
    ///
    /// Terminal control sequences are stripped. Newlines are kept so a
    /// bracketed paste can populate the multiline buffer in one step.
    pub fn paste(&mut self, text: impl AsRef<str>) -> bool {
        let clean = sanitize_terminal_text(text);
        if clean.is_empty() {
            return false;
        }
        self.buffer.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
        true
    }

    /// Deletes the grapheme cluster before the caret.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let start = prev_grapheme_boundary(&self.buffer, self.cursor);
        self.buffer.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }

    /// Replaces the buffer and places the caret at the end.
    pub fn replace(&mut self, text: String) -> bool {
        if self.buffer == text && self.cursor == text.len() {
            return false;
        }
        self.buffer = text;
        self.cursor = self.buffer.len();
        true
    }

    /// Clears the buffer and returns the previous contents.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.buffer)
    }
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl From<String> for LineEditor {
    fn from(text: String) -> Self {
        let cursor = text.len();
        Self {
            buffer: text,
            cursor,
        }
    }
}
