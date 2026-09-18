//! Fixed English product labels used by the TUI foundation.
//!
//! Keeping built-in copy in one module makes the language policy testable.
//! Dynamic model, tool, and session content is not included here.
/// Tagline shown with every logo variant.
pub const LOGO_TAGLINE: &str = "TERMINAL CODE AGENT";
/// Workflow text shown by the wide logo.
pub const LOGO_WORKFLOW: &str = "PLAN | BUILD | VERIFY";
/// Default status before a session is connected.
pub const STATUS_READY: &str = "Ready";
/// Placeholder shown when the input buffer is empty.
pub const INPUT_PLACEHOLDER: &str = "Ask MCode to build something...";
/// Title of the render-block viewport.
pub const TRANSCRIPT_TITLE: &str = "Conversation";
/// Title of the input area.
pub const INPUT_TITLE: &str = "Input";
/// Status-bar label for the help toggle action.
pub const HINT_HELP_LABEL: &str = "Help";
/// Status-bar label for the submit action.
pub const HINT_SEND_LABEL: &str = "Send";
/// Status-bar label for the quit action.
pub const HINT_QUIT_LABEL: &str = "Quit";
/// Help panel title.
pub const HELP_TITLE: &str = "Keyboard help";
/// Help-panel label for submitting input.
pub const HELP_SEND_LABEL: &str = "Send input";
/// Help-panel label for deleting input.
pub const HELP_BACKSPACE_LABEL: &str = "Delete previous character";
/// Help-panel label for closing the application.
pub const HELP_QUIT_LABEL: &str = "Quit";
/// Help-panel label for closing the help panel.
pub const HELP_CLOSE_LABEL: &str = "Close help";
/// Help-panel marker for an action without a key binding.
pub const UNBOUND_LABEL: &str = "unbound";
/// Fallback message for an unavailable named theme.
pub const THEME_FALLBACK_ERROR: &str = "Requested theme unavailable; using mcode-dark.";
/// Fallback title when an interaction prompt has no usable heading.
pub const INTERACTION_TITLE: &str = "Request";
/// Interaction choice that cancels the active host request.
pub const INTERACTION_CANCEL: &str = "Cancel";

/// Every fixed product label covered by the English/ASCII policy.
pub const BUILTIN_UI_LABELS: &[&str] = &[
    LOGO_TAGLINE,
    LOGO_WORKFLOW,
    STATUS_READY,
    INPUT_PLACEHOLDER,
    TRANSCRIPT_TITLE,
    INPUT_TITLE,
    HINT_HELP_LABEL,
    HINT_SEND_LABEL,
    HINT_QUIT_LABEL,
    HELP_TITLE,
    HELP_SEND_LABEL,
    HELP_BACKSPACE_LABEL,
    HELP_QUIT_LABEL,
    HELP_CLOSE_LABEL,
    UNBOUND_LABEL,
    THEME_FALLBACK_ERROR,
    INTERACTION_TITLE,
    INTERACTION_CANCEL,
];
