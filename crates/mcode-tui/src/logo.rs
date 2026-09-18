//! Responsive, theme-token-only MCode terminal mark.
//!
//! The mark is original to MCode and stores semantic tokens rather than ANSI
//! escapes or concrete colors. Wide terminals receive a horizontal lockup,
//! compact terminals receive a small terminal badge, and no-color or
//! non-Unicode terminals receive an ASCII badge.
use mcode_render::{display_width, truncate_display_width};

use crate::labels::{LOGO_TAGLINE, LOGO_WORKFLOW};
use crate::terminal::TerminalCapabilities;
use crate::theme::SemanticToken;

/// Minimum terminal width that selects the wide colored logo.
pub const WIDE_LOGO_MIN_WIDTH: u16 = 70;

/// Rows occupied by every responsive logo variant.
pub(crate) const LOGO_ROWS: u16 = 4;

/// Responsive logo layout selected for a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LogoVariant {
    /// Horizontal Unicode lockup for wide color terminals.
    Wide,
    /// Compact Unicode terminal badge.
    Compact,
    /// ASCII badge for no-color or non-Unicode terminals.
    Ascii,
}

/// One semantically styled span in a logo line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoSpan {
    text: String,
    token: SemanticToken,
    bold: bool,
}

impl LogoSpan {
    /// Returns this span's visible text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the semantic theme role for this span.
    #[must_use]
    pub const fn token(&self) -> SemanticToken {
        self.token
    }

    /// Returns whether this span requests bold emphasis.
    #[must_use]
    pub const fn is_bold(&self) -> bool {
        self.bold
    }
}

/// One bounded line in a [`TerminalLogo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoLine {
    spans: Vec<LogoSpan>,
}

impl LogoLine {
    /// Returns this line's semantic spans.
    #[must_use]
    pub fn spans(&self) -> &[LogoSpan] {
        &self.spans
    }

    /// Returns this line without styling.
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    /// Returns this line's terminal display width.
    #[must_use]
    pub fn display_width(&self) -> usize {
        display_width(self.to_plain_text())
    }
}

/// A responsive MCode logo ready for backend-specific styling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalLogo {
    variant: LogoVariant,
    lines: Vec<LogoLine>,
}

impl TerminalLogo {
    /// Returns the selected responsive layout.
    #[must_use]
    pub const fn variant(&self) -> LogoVariant {
        self.variant
    }

    /// Returns the bounded lines in display order.
    #[must_use]
    pub fn lines(&self) -> &[LogoLine] {
        &self.lines
    }
}

/// Builds a responsive terminal logo no wider than `width` columns.
#[must_use]
pub fn terminal_logo(width: u16, capabilities: TerminalCapabilities) -> TerminalLogo {
    let variant = if !capabilities.supports_color() || !capabilities.supports_unicode() {
        LogoVariant::Ascii
    } else if width >= WIDE_LOGO_MIN_WIDTH {
        LogoVariant::Wide
    } else {
        LogoVariant::Compact
    };

    let source = match variant {
        LogoVariant::Wide => wide_lines(),
        LogoVariant::Compact => compact_lines(),
        LogoVariant::Ascii => ascii_lines(),
    };
    let lines = source
        .into_iter()
        .map(|line| bound_line(line, usize::from(width), variant == LogoVariant::Ascii))
        .collect();

    TerminalLogo { variant, lines }
}

fn span(text: impl Into<String>, token: SemanticToken, bold: bool) -> LogoSpan {
    LogoSpan {
        text: text.into(),
        token,
        bold,
    }
}

fn line(spans: Vec<LogoSpan>) -> LogoLine {
    LogoLine { spans }
}

fn wide_lines() -> Vec<LogoLine> {
    vec![
        line(vec![
            span("╭────────────────╮   ", SemanticToken::Accent, false),
            span("M C O D E", SemanticToken::TextPrimary, true),
        ]),
        line(vec![
            span("│  M>_           │   ", SemanticToken::Accent, true),
            span(LOGO_WORKFLOW, SemanticToken::ToolTitle, false),
        ]),
        line(vec![
            span("│  [◆◆◆······]   │   ", SemanticToken::Accent, false),
            span(LOGO_TAGLINE, SemanticToken::TextMuted, true),
        ]),
        line(vec![span(
            "╰────────────────╯",
            SemanticToken::Accent,
            false,
        )]),
    ]
}

fn compact_lines() -> Vec<LogoLine> {
    vec![
        line(vec![span(
            "╭─ MCODE ────────╮",
            SemanticToken::Accent,
            true,
        )]),
        line(vec![
            span("│ M>_  ", SemanticToken::Accent, true),
            span("BUILD      │", SemanticToken::ToolTitle, false),
        ]),
        line(vec![span(
            "╰─────────────────╯",
            SemanticToken::Accent,
            false,
        )]),
        line(vec![span(LOGO_TAGLINE, SemanticToken::TextMuted, true)]),
    ]
}

fn ascii_lines() -> Vec<LogoLine> {
    vec![
        line(vec![span(
            "+-- MCODE --------+",
            SemanticToken::Accent,
            true,
        )]),
        line(vec![
            span("| M>_  ", SemanticToken::Accent, true),
            span("BUILD      |", SemanticToken::ToolTitle, false),
        ]),
        line(vec![span(
            "+-----------------+",
            SemanticToken::Accent,
            false,
        )]),
        line(vec![span(LOGO_TAGLINE, SemanticToken::TextMuted, true)]),
    ]
}

fn bound_line(line: LogoLine, width: usize, ascii: bool) -> LogoLine {
    let mut remaining = width;
    let mut spans = Vec::new();

    for source in line.spans {
        if remaining == 0 {
            break;
        }
        let mut text = truncate_display_width(&source.text, remaining);
        if ascii && text.ends_with('…') {
            text.pop();
            text.push('.');
        }
        let used = display_width(&text);
        let was_truncated = text != source.text;
        spans.push(LogoSpan { text, ..source });
        remaining = remaining.saturating_sub(used);
        if was_truncated {
            break;
        }
    }

    LogoLine { spans }
}
