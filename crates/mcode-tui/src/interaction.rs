//! Bounded host interaction modal with no authorization policy.
//!
//! The TUI stores a display-only prompt, isolates modal input, sanitizes
//! display text, renders the prompt, and emits [`crate::Effect`] values when
//! the user selects an option or cancels. The host assigns semantics to those
//! responses. An unreadable viewport fail-closes by cancelling so hidden input
//! cannot accept a request.
use std::fmt;

use mcode_render::{display_width, sanitize_terminal_text, truncate_display_width};

use crate::labels::INTERACTION_TITLE;
use crate::state::Viewport;

/// Minimum columns that can present an interaction notice.
///
/// Narrower views cannot keep option rows readable, so the reducer
/// fail-closes to cancel instead of accepting an unseen prompt.
pub const INTERACTION_MIN_COLUMNS: u16 = 24;

/// Rows occupied by the input panel in the interaction layout contract.
///
/// Matches `render.rs`: bordered single-line input is 3 rows.
pub const INTERACTION_INPUT_ROWS: u16 = 3;
/// Rows occupied by the status bar when the viewport is non-empty.
pub const INTERACTION_STATUS_ROWS: u16 = 1;
/// Rows occupied by the interaction panel border.
pub const INTERACTION_BORDER_ROWS: u16 = 2;
/// Title row plus one body row reserved by the readability contract.
const INTERACTION_MIN_FIXED_INNER_ROWS: u16 = 2;

/// Maximum display columns used for the interaction title.
pub const INTERACTION_MAX_TITLE_COLUMNS: usize = 78;

/// Maximum display columns used for one interaction body line.
pub const INTERACTION_MAX_BODY_COLUMNS: usize = 76;

/// Maximum body lines stored on an interaction prompt.
pub const INTERACTION_MAX_BODY_LINES: usize = 12;

/// Maximum options stored on an interaction prompt.
///
/// Digit shortcuts are `1` through `9`. A tenth option would collide with
/// those keys or require a multi-key chord, so extras are dropped.
pub const INTERACTION_MAX_OPTIONS: usize = 9;

/// Maximum Unicode scalars stored in a request identifier.
///
/// Host tokens such as UUIDs fit in 36 characters; 64 leaves headroom
/// without unbounded reducer state.
pub const INTERACTION_MAX_REQUEST_ID_CHARS: usize = 64;

/// Maximum Unicode scalars stored in an option identifier.
pub const INTERACTION_MAX_OPTION_ID_CHARS: usize = 64;

/// Maximum display columns stored in an option label.
pub const INTERACTION_MAX_OPTION_LABEL_COLUMNS: usize = INTERACTION_MAX_BODY_COLUMNS;

/// Invalid opaque identifier supplied to an interaction prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionIdError {
    field: &'static str,
    reason: &'static str,
}

impl InteractionIdError {
    /// Returns the identifier field that failed validation.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        self.field
    }
}

impl fmt::Display for InteractionIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.field, self.reason)
    }
}

impl std::error::Error for InteractionIdError {}

/// First ASCII digit assigned to a stored option.
///
/// Options receive consecutive keys `1` through `9` after filtering.
const FIRST_OPTION_KEY: u8 = b'1';

/// One sanitized option shown by an interaction prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionOption {
    id: String,
    label: String,
}

impl InteractionOption {
    /// Creates an option with an exact opaque identifier.
    ///
    /// Labels keep one sanitized line of at most
    /// [`INTERACTION_MAX_OPTION_LABEL_COLUMNS`] display columns. An
    /// [`InteractionPrompt`] derives digit shortcuts from stored positions.
    ///
    /// # Errors
    ///
    /// Returns [`InteractionIdError`] when `id` is empty, contains a control
    /// character, or exceeds [`INTERACTION_MAX_OPTION_ID_CHARS`] scalars.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
    ) -> Result<Self, InteractionIdError> {
        Ok(Self {
            id: validate_identifier(id, INTERACTION_MAX_OPTION_ID_CHARS, "option ID")?,
            label: bound_line(label, INTERACTION_MAX_OPTION_LABEL_COLUMNS),
        })
    }

    /// Returns the bounded option identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the bounded option label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// User-facing interaction notice held as pure view state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionPrompt {
    request_id: String,
    title: String,
    body_lines: Vec<String>,
    options: Vec<InteractionOption>,
}

impl InteractionPrompt {
    /// Creates a bounded display-only interaction prompt.
    ///
    /// `request_id` is an opaque host token echoed in
    /// [`crate::Effect::InteractionResolved`]. Extra options, duplicate
    /// identifiers, and labels without visible content are dropped. This type
    /// does not evaluate authorization rules.
    ///
    /// # Examples
    ///
    /// ```
    /// use mcode_tui::{InteractionOption, InteractionPrompt};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let prompt = InteractionPrompt::new(
    ///     "req-1",
    ///     "Continue?",
    ///     "Run the next step",
    ///     [
    ///         InteractionOption::new("yes", "Continue")?,
    ///         InteractionOption::new("no", "Skip")?,
    ///     ],
    /// )?;
    /// assert_eq!(prompt.option_for_key('1').map(InteractionOption::id), Some("yes"));
    /// assert_eq!(prompt.options()[1].id(), "no");
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`InteractionIdError`] when `request_id` is empty, contains a
    /// control character, or exceeds [`INTERACTION_MAX_REQUEST_ID_CHARS`]
    /// scalars.
    pub fn new(
        request_id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        options: impl IntoIterator<Item = InteractionOption>,
    ) -> Result<Self, InteractionIdError> {
        let title = bound_line(title, INTERACTION_MAX_TITLE_COLUMNS);
        Ok(Self {
            request_id: validate_identifier(
                request_id,
                INTERACTION_MAX_REQUEST_ID_CHARS,
                "request ID",
            )?,
            title: if has_visible_content(&title) {
                title
            } else {
                INTERACTION_TITLE.to_owned()
            },
            body_lines: bound_body_lines(body),
            options: pack_options(options),
        })
    }

    /// Returns the host request identifier.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the bounded title line.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns bounded body lines in display order.
    #[must_use]
    pub fn body_lines(&self) -> &[String] {
        &self.body_lines
    }

    /// Returns bounded options in display order.
    #[must_use]
    pub fn options(&self) -> &[InteractionOption] {
        &self.options
    }

    /// Returns the option assigned to `key`, if any.
    #[must_use]
    pub fn option_for_key(&self, key: char) -> Option<&InteractionOption> {
        self.options
            .iter()
            .enumerate()
            .find_map(|(index, option)| (option_key(index) == Some(key)).then_some(option))
    }

    /// Returns whether `viewport` can show every stored option.
    #[must_use]
    pub fn presentable_in(&self, viewport: Viewport) -> bool {
        is_readable(viewport, self.options.len())
    }
}

/// Outcome produced by the interaction surface.
///
/// Selected identifiers are opaque host tokens. `Cancelled` covers Esc,
/// unreadably small viewports, and a rejected second request. The TUI does
/// not interpret either variant as an authorization decision.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum InteractionResponse {
    /// The user selected this option identifier.
    Selected(String),
    /// The user cancelled, or the host fail-closed the request.
    Cancelled,
}

impl fmt::Display for InteractionResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Selected(option_id) => {
                formatter.write_str("selected ")?;
                formatter.write_str(option_id)
            }
            Self::Cancelled => formatter.write_str("cancelled"),
        }
    }
}

/// Status-bar fields supplied by the host as plain data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusSurface {
    message: String,
}

impl StatusSurface {
    /// Creates a status surface from `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the status message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Returns inner rows required to show title, one body line, and every option.
#[must_use]
pub fn min_inner_rows(option_count: usize) -> u16 {
    let options = u16::try_from(option_count).unwrap_or(u16::MAX);
    INTERACTION_MIN_FIXED_INNER_ROWS.saturating_add(options)
}

/// Returns the body-panel height used when an interaction is visible.
///
/// Logo is given zero rows so the notice receives the space `is_readable`
/// reserved for it. Keep this in lock-step with `render.rs`.
#[must_use]
pub const fn interaction_body_height(viewport: Viewport) -> u16 {
    let status = if viewport.height > 0 {
        INTERACTION_STATUS_ROWS
    } else {
        0
    };
    let input = if viewport.height >= 4 {
        let budget = viewport.height.saturating_sub(status.saturating_add(1));
        if INTERACTION_INPUT_ROWS < budget {
            INTERACTION_INPUT_ROWS
        } else {
            budget
        }
    } else {
        viewport.height.saturating_sub(status)
    };
    viewport.height.saturating_sub(status.saturating_add(input))
}

/// Returns whether `viewport` can present `option_count` interaction options.
#[must_use]
pub fn is_readable(viewport: Viewport, option_count: usize) -> bool {
    if option_count == 0 || viewport.width < INTERACTION_MIN_COLUMNS {
        return false;
    }
    interaction_body_height(viewport).saturating_sub(INTERACTION_BORDER_ROWS)
        >= min_inner_rows(option_count)
}

fn validate_identifier(
    raw: impl Into<String>,
    max_chars: usize,
    field: &'static str,
) -> Result<String, InteractionIdError> {
    let identifier = raw.into();
    let reason = if identifier.is_empty() {
        Some("must not be empty")
    } else if identifier.chars().any(char::is_control) {
        Some("must not contain control characters")
    } else if identifier.chars().count() > max_chars {
        Some("exceeds its scalar limit")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(InteractionIdError { field, reason }),
        None => Ok(identifier),
    }
}

fn bound_line(raw: impl Into<String>, max_columns: usize) -> String {
    let clean = sanitize_terminal_text(raw.into());
    let line = clean.lines().next().unwrap_or_default();
    if has_visible_content(line) {
        truncate_display_width(line, max_columns)
    } else {
        String::new()
    }
}

fn bound_body_lines(raw: impl Into<String>) -> Vec<String> {
    let clean = sanitize_terminal_text(raw.into());
    clean
        .lines()
        .take(INTERACTION_MAX_BODY_LINES)
        .map(|line| truncate_display_width(line, INTERACTION_MAX_BODY_COLUMNS))
        .collect()
}

fn pack_options(options: impl IntoIterator<Item = InteractionOption>) -> Vec<InteractionOption> {
    let mut packed: Vec<InteractionOption> = Vec::with_capacity(INTERACTION_MAX_OPTIONS);
    for option in options {
        if packed.len() >= INTERACTION_MAX_OPTIONS {
            break;
        }
        if !has_visible_content(&option.label) {
            continue;
        }
        if packed.iter().any(|existing| existing.id == option.id) {
            continue;
        }
        packed.push(option);
    }
    packed
}

fn has_visible_content(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && display_width(trimmed) > 0
}

pub(crate) fn option_key(index: usize) -> Option<char> {
    if index >= INTERACTION_MAX_OPTIONS {
        return None;
    }
    let offset = u8::try_from(index).ok()?;
    Some(char::from(FIRST_OPTION_KEY.saturating_add(offset)))
}
