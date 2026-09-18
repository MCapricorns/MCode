//! Semantic themes and deterministic theme selection.
//!
//! Themes expose intent-oriented tokens rather than widget-specific colors.
//! Built-in dark and light palettes are designed independently. Selection is
//! pure: callers provide any detected background and named themes, so this
//! module never reads environment variables or terminal state.
use std::fmt;

/// An eight-bit sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
}

impl Rgb {
    /// Creates an sRGB color from channel values.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "#{:02X}{:02X}{:02X}",
            self.red, self.green, self.blue
        )
    }
}

/// Visual appearance a theme is designed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ThemeAppearance {
    /// A palette designed for a dark terminal background.
    Dark,
    /// A palette designed for a light terminal background.
    Light,
}

/// Semantic color roles available to every theme.
///
/// [`SemanticToken::ALL`] and [`SemanticToken::COUNT`] provide a stable way to
/// validate custom themes and adapters without relying on widget internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
#[non_exhaustive]
pub enum SemanticToken {
    /// Primary application background.
    Background,
    /// Default panel surface.
    Surface,
    /// Raised or emphasized panel surface.
    SurfaceRaised,
    /// Primary readable text.
    TextPrimary,
    /// Secondary readable text.
    TextMuted,
    /// De-emphasized text.
    TextDim,
    /// Standard border.
    Border,
    /// Focused border.
    BorderFocus,
    /// Primary accent.
    Accent,
    /// Subtle accent surface.
    AccentMuted,
    /// Success state.
    Success,
    /// Warning state.
    Warning,
    /// Error state.
    Error,
    /// Informational state.
    Info,
    /// Selected-item background.
    SelectionBackground,
    /// Selected-item foreground.
    SelectionText,
    /// Input field background.
    InputBackground,
    /// Input field foreground.
    InputText,
    /// Status bar background.
    StatusBackground,
    /// Status bar foreground.
    StatusText,
    /// Tool heading and activity.
    ToolTitle,
    /// Tool output.
    ToolOutput,
    /// Markdown heading.
    MarkdownHeading,
    /// Markdown link.
    MarkdownLink,
    /// Inline and fenced Markdown code.
    MarkdownCode,
    /// Markdown quote.
    MarkdownQuote,
    /// Added diff line.
    DiffAdded,
    /// Removed diff line.
    DiffRemoved,
    /// Context diff line.
    DiffContext,
    /// Syntax comment.
    SyntaxComment,
    /// Syntax keyword.
    SyntaxKeyword,
    /// Syntax function.
    SyntaxFunction,
    /// Syntax variable.
    SyntaxVariable,
    /// Syntax string.
    SyntaxString,
    /// Syntax number.
    SyntaxNumber,
    /// Syntax type.
    SyntaxType,
    /// Syntax operator.
    SyntaxOperator,
    /// Syntax punctuation.
    SyntaxPunctuation,
    /// Inactive progress track.
    ProgressTrack,
    /// Active progress fill.
    ProgressFill,
}

impl SemanticToken {
    /// Number of semantic roles in a complete theme.
    pub const COUNT: usize = 40;

    /// Every semantic role in declaration order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Background,
        Self::Surface,
        Self::SurfaceRaised,
        Self::TextPrimary,
        Self::TextMuted,
        Self::TextDim,
        Self::Border,
        Self::BorderFocus,
        Self::Accent,
        Self::AccentMuted,
        Self::Success,
        Self::Warning,
        Self::Error,
        Self::Info,
        Self::SelectionBackground,
        Self::SelectionText,
        Self::InputBackground,
        Self::InputText,
        Self::StatusBackground,
        Self::StatusText,
        Self::ToolTitle,
        Self::ToolOutput,
        Self::MarkdownHeading,
        Self::MarkdownLink,
        Self::MarkdownCode,
        Self::MarkdownQuote,
        Self::DiffAdded,
        Self::DiffRemoved,
        Self::DiffContext,
        Self::SyntaxComment,
        Self::SyntaxKeyword,
        Self::SyntaxFunction,
        Self::SyntaxVariable,
        Self::SyntaxString,
        Self::SyntaxNumber,
        Self::SyntaxType,
        Self::SyntaxOperator,
        Self::SyntaxPunctuation,
        Self::ProgressTrack,
        Self::ProgressFill,
    ];

    /// Returns a stable ASCII name for this role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Surface => "surface",
            Self::SurfaceRaised => "surface_raised",
            Self::TextPrimary => "text_primary",
            Self::TextMuted => "text_muted",
            Self::TextDim => "text_dim",
            Self::Border => "border",
            Self::BorderFocus => "border_focus",
            Self::Accent => "accent",
            Self::AccentMuted => "accent_muted",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Info => "info",
            Self::SelectionBackground => "selection_background",
            Self::SelectionText => "selection_text",
            Self::InputBackground => "input_background",
            Self::InputText => "input_text",
            Self::StatusBackground => "status_background",
            Self::StatusText => "status_text",
            Self::ToolTitle => "tool_title",
            Self::ToolOutput => "tool_output",
            Self::MarkdownHeading => "markdown_heading",
            Self::MarkdownLink => "markdown_link",
            Self::MarkdownCode => "markdown_code",
            Self::MarkdownQuote => "markdown_quote",
            Self::DiffAdded => "diff_added",
            Self::DiffRemoved => "diff_removed",
            Self::DiffContext => "diff_context",
            Self::SyntaxComment => "syntax_comment",
            Self::SyntaxKeyword => "syntax_keyword",
            Self::SyntaxFunction => "syntax_function",
            Self::SyntaxVariable => "syntax_variable",
            Self::SyntaxString => "syntax_string",
            Self::SyntaxNumber => "syntax_number",
            Self::SyntaxType => "syntax_type",
            Self::SyntaxOperator => "syntax_operator",
            Self::SyntaxPunctuation => "syntax_punctuation",
            Self::ProgressTrack => "progress_track",
            Self::ProgressFill => "progress_fill",
        }
    }
}

/// A complete semantic color theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    name: String,
    appearance: ThemeAppearance,
    colors: [Rgb; SemanticToken::COUNT],
}

impl Theme {
    /// Creates a complete theme.
    ///
    /// The fixed-size `colors` array makes missing semantic tokens impossible.
    /// Values must follow [`SemanticToken::ALL`] order.
    pub fn new(
        name: impl Into<String>,
        appearance: ThemeAppearance,
        colors: [Rgb; SemanticToken::COUNT],
    ) -> Self {
        Self {
            name: name.into(),
            appearance,
            colors,
        }
    }

    /// Returns the theme's selection name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the background appearance this palette targets.
    #[must_use]
    pub const fn appearance(&self) -> ThemeAppearance {
        self.appearance
    }

    /// Returns the color assigned to `token`.
    #[must_use]
    pub const fn color(&self, token: SemanticToken) -> Rgb {
        self.colors[token as usize]
    }

    /// Returns every color in [`SemanticToken::ALL`] order.
    #[must_use]
    pub const fn colors(&self) -> &[Rgb; SemanticToken::COUNT] {
        &self.colors
    }
}

/// A configured theme-selection strategy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum ThemeSelection {
    /// Select from a detected background, with a dark fallback.
    #[default]
    Auto,
    /// Always use the built-in dark theme.
    Dark,
    /// Always use the built-in light theme.
    Light,
    /// Select a built-in or caller-provided named theme.
    Named(String),
}

/// Terminal background classification used by automatic selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BackgroundClass {
    /// A dark background.
    Dark,
    /// A light background.
    Light,
}

/// Why a theme was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ThemeSource {
    /// An explicit dark, light, or available named setting won.
    Explicit,
    /// Automatic selection used a detected background.
    Detected,
    /// Detection or named lookup failed, so `mcode-dark` was used.
    Fallback,
}

/// The selected theme and its resolution source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeResolution {
    theme: Theme,
    source: ThemeSource,
}

impl ThemeResolution {
    /// Returns the resolved theme.
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Returns why this theme was chosen.
    #[must_use]
    pub const fn source(&self) -> ThemeSource {
        self.source
    }

    /// Consumes the resolution and returns its theme.
    #[must_use]
    pub fn into_theme(self) -> Theme {
        self.theme
    }
}

/// Returns the built-in dark theme.
///
/// The palette uses a green accent, warm tool activity, and distinct
/// blue/cyan/purple syntax roles over near-black layered surfaces.
#[must_use]
pub fn mcode_dark() -> Theme {
    Theme::new(
        "mcode-dark",
        ThemeAppearance::Dark,
        [
            Rgb::new(0x10, 0x12, 0x11), // Background
            Rgb::new(0x17, 0x1b, 0x18), // Surface
            Rgb::new(0x20, 0x26, 0x21), // SurfaceRaised
            Rgb::new(0xe8, 0xee, 0xe9), // TextPrimary
            Rgb::new(0xa8, 0xb4, 0xaa), // TextMuted
            Rgb::new(0x78, 0x84, 0x7b), // TextDim
            Rgb::new(0x34, 0x3d, 0x36), // Border
            Rgb::new(0x44, 0xdf, 0x6c), // BorderFocus
            Rgb::new(0x44, 0xdf, 0x6c), // Accent
            Rgb::new(0x25, 0x66, 0x38), // AccentMuted
            Rgb::new(0x44, 0xdf, 0x6c), // Success
            Rgb::new(0xeb, 0xd1, 0x7c), // Warning
            Rgb::new(0xeb, 0x7c, 0x85), // Error
            Rgb::new(0x7c, 0xb7, 0xeb), // Info
            Rgb::new(0x24, 0x36, 0x2a), // SelectionBackground
            Rgb::new(0xf2, 0xff, 0xf5), // SelectionText
            Rgb::new(0x14, 0x18, 0x15), // InputBackground
            Rgb::new(0xe8, 0xee, 0xe9), // InputText
            Rgb::new(0x1a, 0x21, 0x1c), // StatusBackground
            Rgb::new(0xd8, 0xe4, 0xda), // StatusText
            Rgb::new(0xe9, 0xa1, 0x70), // ToolTitle
            Rgb::new(0xa8, 0xb4, 0xaa), // ToolOutput
            Rgb::new(0x44, 0xdf, 0x6c), // MarkdownHeading
            Rgb::new(0x7c, 0xb7, 0xeb), // MarkdownLink
            Rgb::new(0x7c, 0xeb, 0xe1), // MarkdownCode
            Rgb::new(0xa8, 0xb4, 0xaa), // MarkdownQuote
            Rgb::new(0x44, 0xdf, 0x6c), // DiffAdded
            Rgb::new(0xeb, 0x7c, 0x85), // DiffRemoved
            Rgb::new(0x78, 0x84, 0x7b), // DiffContext
            Rgb::new(0x78, 0x84, 0x7b), // SyntaxComment
            Rgb::new(0xbc, 0xa2, 0xe6), // SyntaxKeyword
            Rgb::new(0x7c, 0xb7, 0xeb), // SyntaxFunction
            Rgb::new(0xe8, 0xee, 0xe9), // SyntaxVariable
            Rgb::new(0x44, 0xdf, 0x6c), // SyntaxString
            Rgb::new(0xe9, 0xa1, 0x70), // SyntaxNumber
            Rgb::new(0xeb, 0xd1, 0x7c), // SyntaxType
            Rgb::new(0x7c, 0xeb, 0xe1), // SyntaxOperator
            Rgb::new(0xa8, 0xb4, 0xaa), // SyntaxPunctuation
            Rgb::new(0x34, 0x3d, 0x36), // ProgressTrack
            Rgb::new(0x44, 0xdf, 0x6c), // ProgressFill
        ],
    )
}

/// Returns the built-in light theme.
///
/// This palette uses warm neutral surfaces, dark readable text, restrained
/// green accents, and independently tuned syntax colors rather than inversion.
#[must_use]
pub fn mcode_light() -> Theme {
    Theme::new(
        "mcode-light",
        ThemeAppearance::Light,
        [
            Rgb::new(0xf7, 0xf7, 0xf2), // Background
            Rgb::new(0xff, 0xff, 0xff), // Surface
            Rgb::new(0xe9, 0xee, 0xe9), // SurfaceRaised
            Rgb::new(0x17, 0x20, 0x1b), // TextPrimary
            Rgb::new(0x46, 0x54, 0x4c), // TextMuted
            Rgb::new(0x5d, 0x68, 0x5f), // TextDim
            Rgb::new(0xb8, 0xc5, 0xbc), // Border
            Rgb::new(0x08, 0x7a, 0x32), // BorderFocus
            Rgb::new(0x08, 0x7a, 0x32), // Accent
            Rgb::new(0x9f, 0xd8, 0xaf), // AccentMuted
            Rgb::new(0x14, 0x6b, 0x33), // Success
            Rgb::new(0x8a, 0x5a, 0x00), // Warning
            Rgb::new(0xa5, 0x2d, 0x3a), // Error
            Rgb::new(0x15, 0x5d, 0x99), // Info
            Rgb::new(0xcd, 0xef, 0xd7), // SelectionBackground
            Rgb::new(0x15, 0x33, 0x1d), // SelectionText
            Rgb::new(0xff, 0xff, 0xff), // InputBackground
            Rgb::new(0x17, 0x20, 0x1b), // InputText
            Rgb::new(0x17, 0x46, 0x28), // StatusBackground
            Rgb::new(0xf8, 0xff, 0xf9), // StatusText
            Rgb::new(0x9a, 0x4d, 0x08), // ToolTitle
            Rgb::new(0x3f, 0x50, 0x46), // ToolOutput
            Rgb::new(0x08, 0x7a, 0x32), // MarkdownHeading
            Rgb::new(0x15, 0x5d, 0x99), // MarkdownLink
            Rgb::new(0x63, 0x36, 0x94), // MarkdownCode
            Rgb::new(0x4f, 0x5d, 0x55), // MarkdownQuote
            Rgb::new(0x14, 0x6b, 0x33), // DiffAdded
            Rgb::new(0xa5, 0x2d, 0x3a), // DiffRemoved
            Rgb::new(0x5d, 0x68, 0x5f), // DiffContext
            Rgb::new(0x5d, 0x68, 0x5f), // SyntaxComment
            Rgb::new(0x6b, 0x3a, 0xa1), // SyntaxKeyword
            Rgb::new(0x15, 0x5d, 0x99), // SyntaxFunction
            Rgb::new(0x17, 0x20, 0x1b), // SyntaxVariable
            Rgb::new(0x14, 0x6b, 0x33), // SyntaxString
            Rgb::new(0x9a, 0x4d, 0x08), // SyntaxNumber
            Rgb::new(0x7a, 0x59, 0x00), // SyntaxType
            Rgb::new(0x00, 0x6b, 0x6b), // SyntaxOperator
            Rgb::new(0x46, 0x54, 0x4c), // SyntaxPunctuation
            Rgb::new(0xb8, 0xc5, 0xbc), // ProgressTrack
            Rgb::new(0x08, 0x7a, 0x32), // ProgressFill
        ],
    )
}

/// Resolves a theme without reading process or terminal state.
///
/// Explicit `Dark`, `Light`, and available `Named` selections always take
/// precedence over `detected_background`. `Auto` uses detection when present.
/// Missing names and failed detection deterministically fall back to
/// `mcode-dark`.
#[must_use]
pub fn resolve_theme(
    selection: &ThemeSelection,
    detected_background: Option<BackgroundClass>,
    named_themes: &[Theme],
) -> ThemeResolution {
    let (theme, source) = match selection {
        ThemeSelection::Dark => (mcode_dark(), ThemeSource::Explicit),
        ThemeSelection::Light => (mcode_light(), ThemeSource::Explicit),
        ThemeSelection::Named(name) if name == "mcode-dark" => {
            (mcode_dark(), ThemeSource::Explicit)
        }
        ThemeSelection::Named(name) if name == "mcode-light" => {
            (mcode_light(), ThemeSource::Explicit)
        }
        ThemeSelection::Named(name) => match named_themes
            .iter()
            .find(|theme| theme.name() == name)
            .cloned()
        {
            Some(theme) => (theme, ThemeSource::Explicit),
            None => (mcode_dark(), ThemeSource::Fallback),
        },
        ThemeSelection::Auto => match detected_background {
            Some(BackgroundClass::Light) => (mcode_light(), ThemeSource::Detected),
            Some(BackgroundClass::Dark) => (mcode_dark(), ThemeSource::Detected),
            None => (mcode_dark(), ThemeSource::Fallback),
        },
    };

    ThemeResolution { theme, source }
}

/// Computes WCAG relative luminance for an sRGB color.
#[must_use]
pub fn relative_luminance(color: Rgb) -> f64 {
    fn linear(channel: u8) -> f64 {
        let value = f64::from(channel) / 255.0;
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    0.212_6 * linear(color.red) + 0.715_2 * linear(color.green) + 0.072_2 * linear(color.blue)
}

/// Computes the WCAG contrast ratio between two sRGB colors.
#[must_use]
pub fn contrast_ratio(first: Rgb, second: Rgb) -> f64 {
    let first = relative_luminance(first);
    let second = relative_luminance(second);
    let (lighter, darker) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    (lighter + 0.05) / (darker + 0.05)
}
