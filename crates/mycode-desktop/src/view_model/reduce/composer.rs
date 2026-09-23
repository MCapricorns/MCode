//! Composer mention parsing: turns the draft into an active `@` file or `/`
//! command autocomplete.

use crate::view_model::{COMPOSER_COMMANDS, ComposerMention, MentionKind};

/// Parses the composer draft into an active mention, if any: a trailing
/// `@fragment` token selects files; a leading `/name` (still the whole
/// draft) selects commands.
pub(super) fn parse_mention(text: &str) -> Option<ComposerMention> {
    if text.ends_with(char::is_whitespace) || text.is_empty() {
        return None;
    }
    let token = text.split_whitespace().last().unwrap_or_default();
    if let Some(fragment) = token.strip_prefix('@')
        && !fragment.contains('@')
    {
        return Some(ComposerMention {
            kind: MentionKind::File,
            fragment: fragment.to_owned(),
            items: Vec::new(),
        });
    }
    if let Some(fragment) = text.strip_prefix('/')
        && !fragment.contains(char::is_whitespace)
    {
        let items = COMPOSER_COMMANDS
            .iter()
            .filter(|(name, _)| name[1..].starts_with(fragment))
            .map(|(name, label)| ((*name).to_owned(), format!("{name} \u{b7} {label}")))
            .collect();
        return Some(ComposerMention {
            kind: MentionKind::Command,
            fragment: fragment.to_owned(),
            items,
        });
    }
    None
}
