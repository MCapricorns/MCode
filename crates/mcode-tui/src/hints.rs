//! English keyboard hints derived from the injected [`ActionRegistry`].
//!
//! Help panels and status-bar hints are generated from the same registry that
//! dispatches input, and only from bindings that are live in the evaluated
//! [`AppState`](crate::state::AppState): bindings whose `When` predicate is
//! inactive, or whose key is claimed by a later registration, are not
//! advertised. Reconfigured bindings are therefore shown truthfully and
//! unbound actions never claim built-in keys.
use crossterm::event::{KeyCode, KeyModifiers};

use crate::actions::{ActionId, ActionRegistry, KeyPattern};
use crate::labels::{
    HELP_BACKSPACE_LABEL, HELP_CLOSE_LABEL, HELP_QUIT_LABEL, HELP_SEND_LABEL, HINT_HELP_LABEL,
    HINT_QUIT_LABEL, HINT_SEND_LABEL, UNBOUND_LABEL,
};
use crate::state::AppState;

/// Column width that aligns help-panel keys with their action labels.
const HELP_KEY_WIDTH: usize = 9;

/// Returns the single-key label for `pattern`, for example `Ctrl+Shift+C`.
///
/// Every modifier required by the pattern is rendered in `Ctrl+Alt+Shift+
/// Super+Hyper+Meta` order, including `Shift` on character keys, and every
/// [`KeyCode`] variant receives its own name, so the label always identifies
/// the exact bound key.
///
/// Returns `None` for the [`KeyPattern::Text`] fallback because it has no
/// dedicated key to display.
#[must_use]
pub fn pattern_label(pattern: &KeyPattern) -> Option<String> {
    let KeyPattern::Exact { code, modifiers } = pattern else {
        return None;
    };

    let mut label = String::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        label.push_str("Ctrl+");
    }
    if modifiers.contains(KeyModifiers::ALT) {
        label.push_str("Alt+");
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        label.push_str("Shift+");
    }
    if modifiers.contains(KeyModifiers::SUPER) {
        label.push_str("Super+");
    }
    if modifiers.contains(KeyModifiers::HYPER) {
        label.push_str("Hyper+");
    }
    if modifiers.contains(KeyModifiers::META) {
        label.push_str("Meta+");
    }
    label.push_str(&key_name(*code));
    Some(label)
}

/// Returns the key label for the primary live binding of `action` in `state`.
///
/// Bindings whose [`When`](crate::actions::When) predicate is inactive or
/// whose key is claimed by a later registration are skipped, so the label
/// always names a key that dispatches to this action in `state`. Returns
/// `None` when the action has no live exact-key binding, in which case hints
/// must omit or mark it instead of inventing a key.
#[must_use]
pub fn key_label(registry: &ActionRegistry, state: &AppState, action: ActionId) -> Option<String> {
    registry
        .binding_for(action, state)
        .and_then(|binding| pattern_label(binding.pattern()))
}

/// Builds the compact status-bar hint string, for example
/// `F1 Help | Enter Send | Ctrl+C Quit`.
///
/// Actions without a live exact-key binding are omitted so an empty registry
/// never advertises built-in keys.
#[must_use]
pub fn status_key_hints(registry: &ActionRegistry, state: &AppState) -> String {
    const SECTIONS: [(ActionId, &str); 3] = [
        (ActionId::ToggleHelp, HINT_HELP_LABEL),
        (ActionId::Submit, HINT_SEND_LABEL),
        (ActionId::Quit, HINT_QUIT_LABEL),
    ];

    SECTIONS
        .iter()
        .filter_map(|(action, label)| {
            key_label(registry, state, *action).map(|keys| format!("{keys} {label}"))
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Builds one help-panel line per supported action, in display order.
///
/// Actions with a live binding show `Key<padding>Action`; actions without one
/// are marked with the English `unbound` label instead of a built-in key.
#[must_use]
pub fn help_lines(registry: &ActionRegistry, state: &AppState) -> Vec<String> {
    const ENTRIES: [(ActionId, &str); 4] = [
        (ActionId::Submit, HELP_SEND_LABEL),
        (ActionId::Backspace, HELP_BACKSPACE_LABEL),
        (ActionId::Quit, HELP_QUIT_LABEL),
        (ActionId::ToggleHelp, HELP_CLOSE_LABEL),
    ];

    ENTRIES
        .iter()
        .map(
            |(action, label)| match key_label(registry, state, *action) {
                Some(keys) => format!("{keys:<HELP_KEY_WIDTH$} {label}"),
                None => format!("{label} ({UNBOUND_LABEL})"),
            },
        )
        .collect()
}

/// Formats one key code without modifiers.
///
/// Every [`KeyCode`] variant is matched explicitly so an exact binding is
/// never displayed as a different key.
fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char(character) => character.to_ascii_uppercase().to_string(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "BackTab".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::F(function) => format!("F{function}"),
        KeyCode::Null => "Null".into(),
        KeyCode::CapsLock => "CapsLock".into(),
        KeyCode::ScrollLock => "ScrollLock".into(),
        KeyCode::NumLock => "NumLock".into(),
        KeyCode::PrintScreen => "PrintScreen".into(),
        KeyCode::Pause => "Pause".into(),
        KeyCode::Menu => "Menu".into(),
        KeyCode::KeypadBegin => "KeypadBegin".into(),
        // Inner key Debug names stay distinct per key, for example
        // `Media(Play)` and `Modifier(LeftShift)`.
        KeyCode::Media(media) => format!("Media({media:?})"),
        KeyCode::Modifier(modifier) => format!("Modifier({modifier:?})"),
    }
}
