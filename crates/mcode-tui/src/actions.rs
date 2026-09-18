//! Pure actions, configurable input bindings, effects, and state reduction.
//!
//! [`ActionRegistry`] translates crossterm events through ordered bindings and
//! [`When`] predicates. [`reduce`] is deterministic and performs no I/O.
//! Effects remain plain data for an embedding event loop to execute.
use std::fmt;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use mcode_render::RenderBlock;

use crate::interaction::{InteractionPrompt, InteractionResponse};
use crate::layout::transcript_viewport;
use crate::scrollback::MaterializeBudget;
use crate::state::{AppState, Viewport};
use crate::theme::{BackgroundClass, ThemeSelection};

/// A user or host intent applied to [`AppState`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Action {
    /// Insert one visible character into the input buffer.
    Insert(char),
    /// Insert a newline into the multiline input buffer.
    InsertNewline,
    /// Remove the previous grapheme cluster.
    Backspace,
    /// Insert sanitized pasted text at the caret.
    Paste(String),
    /// Replace the complete input buffer.
    ReplaceInput(String),
    /// Submit non-empty input and clear the buffer.
    Submit,
    /// Replace render blocks in display order, bounded by scrollback capacity.
    ReplaceBlocks(Vec<RenderBlock>),
    /// Replace status-bar text.
    SetStatus(String),
    /// Present a display-only host interaction prompt.
    PresentInteraction(InteractionPrompt),
    /// Select the option bound to this digit key on the active prompt.
    SelectInteractionOption(char),
    /// Cancel the active host interaction prompt.
    CancelInteraction,
    /// Scroll the transcript toward older history when positive.
    ScrollBy(i32),
    /// Apply a terminal resize.
    Resize(Viewport),
    /// Change explicit or automatic theme selection.
    SelectTheme(ThemeSelection),
    /// Apply an optional externally detected background.
    DetectBackground(Option<BackgroundClass>),
    /// Toggle the built-in keyboard help panel.
    ToggleHelp,
    /// Request application shutdown.
    Quit,
}

/// Stable identifier for an action available to keyboard bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActionId {
    /// Insert the character carried by a matching key event.
    InsertCharacter,
    /// Insert a newline into the multiline input buffer.
    InsertNewline,
    /// Remove the previous grapheme cluster.
    Backspace,
    /// Submit non-empty input.
    Submit,
    /// Toggle the built-in help panel.
    ToggleHelp,
    /// Request application shutdown.
    Quit,
    /// Select the interaction option bound to a digit key.
    SelectInteractionOption,
    /// Cancel the pending host interaction request.
    CancelInteraction,
}

impl ActionId {
    /// Returns the stable ASCII configuration name for this action.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InsertCharacter => "insert_character",
            Self::InsertNewline => "insert_newline",
            Self::Backspace => "backspace",
            Self::Submit => "submit",
            Self::ToggleHelp => "toggle_help",
            Self::Quit => "quit",
            Self::SelectInteractionOption => "select_interaction_option",
            Self::CancelInteraction => "cancel_interaction",
        }
    }

    fn action(self, key: &KeyEvent) -> Option<Action> {
        match self {
            Self::InsertCharacter => match key.code {
                KeyCode::Char(character) => Some(Action::Insert(character)),
                _ => None,
            },
            Self::InsertNewline => Some(Action::InsertNewline),
            Self::Backspace => Some(Action::Backspace),
            Self::Submit => Some(Action::Submit),
            Self::ToggleHelp => Some(Action::ToggleHelp),
            Self::Quit => Some(Action::Quit),
            Self::SelectInteractionOption => match key.code {
                KeyCode::Char(digit) if matches!(digit, '1'..='9') => {
                    Some(Action::SelectInteractionOption(digit))
                }
                _ => None,
            },
            Self::CancelInteraction => Some(Action::CancelInteraction),
        }
    }
}

/// A keyboard event pattern used by an [`ActionBinding`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyPattern {
    /// One exact key code and modifier set.
    Exact {
        /// Required key code.
        code: KeyCode,
        /// Required modifiers; extra modifiers do not match.
        modifiers: KeyModifiers,
    },
    /// Any printable character typed as ordinary text.
    ///
    /// `Shift` is allowed, and `Ctrl+Alt` is allowed as a pair because
    /// Windows terminals report AltGr as `CONTROL | ALT` plus a printable
    /// `KeyCode::Char` (for example `@`, `€`, or braces on many layouts).
    /// Lone `Ctrl`, lone `Alt`, and other command modifiers never match, so
    /// they remain available to explicit [`KeyPattern::Exact`] bindings.
    Text,
}

impl KeyPattern {
    /// Creates a pattern for one exact key and modifier set.
    #[must_use]
    pub const fn exact(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self::Exact { code, modifiers }
    }

    /// Creates a pattern for ordinary character input.
    #[must_use]
    pub const fn text() -> Self {
        Self::Text
    }

    /// Returns whether this is the [`KeyPattern::Text`] input fallback.
    pub(crate) const fn is_text(&self) -> bool {
        matches!(self, Self::Text)
    }

    fn matches(&self, key: &KeyEvent) -> bool {
        match self {
            Self::Exact { code, modifiers } => key.code == *code && key.modifiers == *modifiers,
            Self::Text => match key.code {
                KeyCode::Char(character) => is_text_input(character, key.modifiers),
                _ => false,
            },
        }
    }
}

/// Modifier pair that Windows terminals report for AltGr.
const ALT_GR: KeyModifiers = KeyModifiers::CONTROL.union(KeyModifiers::ALT);

/// Returns whether `modifiers` accompany `character` as ordinary text input.
///
/// Accepted sets are `NONE`, `SHIFT`, and `CONTROL | ALT` with or without
/// `SHIFT` (AltGr on Windows terminals). Everything else is a command
/// combination left to exact bindings.
fn is_text_input(character: char, modifiers: KeyModifiers) -> bool {
    !character.is_control()
        && (modifiers == KeyModifiers::NONE
            || modifiers == KeyModifiers::SHIFT
            || modifiers == ALT_GR
            || modifiers == ALT_GR.union(KeyModifiers::SHIFT))
}

/// A named context predicate controlling whether a binding is active.
#[derive(Clone, Copy)]
pub struct When {
    name: &'static str,
    predicate: fn(&AppState) -> bool,
}

impl When {
    /// Predicate that is active in every application state.
    pub const ALWAYS: Self = Self::new("always", when_always);
    /// Predicate that is active while keyboard help is visible.
    pub const HELP_VISIBLE: Self = Self::new("help_visible", when_help_visible);
    /// Predicate that is active while keyboard help is hidden.
    pub const HELP_HIDDEN: Self = Self::new("help_hidden", when_help_hidden);
    /// Predicate that is active while the input buffer is empty.
    pub const INPUT_EMPTY: Self = Self::new("input_empty", when_input_empty);
    /// Predicate that is active while the input buffer is not empty.
    pub const INPUT_NOT_EMPTY: Self = Self::new("input_not_empty", when_input_not_empty);
    /// Predicate that is active while a host interaction prompt is visible.
    pub const INTERACTION_VISIBLE: Self =
        Self::new("interaction_visible", when_interaction_visible);
    /// Predicate that is active while no host interaction prompt is visible.
    pub const INTERACTION_HIDDEN: Self = Self::new("interaction_hidden", when_interaction_hidden);

    /// Creates a named predicate from a non-capturing function.
    ///
    /// `name` is used for diagnostics and configuration integration. The
    /// function must be deterministic and side-effect free so input routing
    /// remains testable.
    #[must_use]
    pub const fn new(name: &'static str, predicate: fn(&AppState) -> bool) -> Self {
        Self { name, predicate }
    }

    /// Returns this predicate's stable diagnostic name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Evaluates this predicate against the current state.
    #[must_use]
    pub fn matches(self, state: &AppState) -> bool {
        (self.predicate)(state)
    }
}

impl fmt::Debug for When {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("When").field(&self.name).finish()
    }
}

/// One ordered mapping from a key pattern to an action identifier.
#[derive(Debug, Clone)]
pub struct ActionBinding {
    pattern: KeyPattern,
    action: ActionId,
    when: When,
}

impl ActionBinding {
    /// Creates an always-active binding.
    #[must_use]
    pub const fn new(pattern: KeyPattern, action: ActionId) -> Self {
        Self {
            pattern,
            action,
            when: When::ALWAYS,
        }
    }

    /// Applies a context predicate to this binding.
    #[must_use]
    pub const fn when(mut self, when: When) -> Self {
        self.when = when;
        self
    }

    /// Returns the matched key pattern.
    #[must_use]
    pub const fn pattern(&self) -> &KeyPattern {
        &self.pattern
    }

    /// Returns the action identifier produced by this binding.
    #[must_use]
    pub const fn action(&self) -> ActionId {
        self.action
    }

    /// Returns the context predicate controlling this binding.
    #[must_use]
    pub const fn condition(&self) -> When {
        self.when
    }
}

/// Ordered configurable keyboard bindings for an [`AppState`].
///
/// Resolution has two tiers so command bindings are never swallowed by
/// character insertion: exact-key bindings are consulted first, then the
/// [`KeyPattern::Text`] fallback. Within each tier later registrations have
/// priority, allowing host configuration to override defaults without
/// mutating reducer logic. Resize events are translated directly because they
/// are terminal geometry, not keyboard bindings.
#[derive(Debug, Clone)]
pub struct ActionRegistry {
    bindings: Vec<ActionBinding>,
}

impl ActionRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// Registers one binding with priority over existing bindings.
    pub fn register(&mut self, binding: ActionBinding) {
        self.bindings.push(binding);
    }

    /// Registers one binding and returns this registry.
    #[must_use]
    pub fn with_binding(mut self, binding: ActionBinding) -> Self {
        self.register(binding);
        self
    }

    /// Returns bindings in registration order.
    #[must_use]
    pub fn bindings(&self) -> &[ActionBinding] {
        &self.bindings
    }

    /// Returns the earliest live binding that dispatches to `action` in
    /// `state`.
    ///
    /// Live means the binding's [`When`] predicate accepts `state` and
    /// pressing the binding's exact key would resolve to this action under
    /// normal dispatch precedence, so a later registration claiming the same
    /// key hides the shadowed binding. This is the canonical binding shown by
    /// help and status hints. Returns `None` when the action has no live
    /// dedicated key.
    #[must_use]
    pub fn binding_for(&self, action: ActionId, state: &AppState) -> Option<&ActionBinding> {
        self.bindings
            .iter()
            .find(|binding| binding.action == action && self.binding_is_live(binding, state))
    }

    /// Returns whether pressing `binding`'s key in `state` dispatches to the
    /// binding's own action.
    ///
    /// The synthesized event is pushed through [`ActionRegistry::action_for_event`]
    /// resolution, so inactive [`When`] predicates and later registrations on
    /// the same key cannot leave a dead binding advertised. The
    /// [`KeyPattern::Text`] fallback has no dedicated key and is never live
    /// for display.
    fn binding_is_live(&self, binding: &ActionBinding, state: &AppState) -> bool {
        let KeyPattern::Exact { code, modifiers } = binding.pattern else {
            return false;
        };
        if !binding.when.matches(state) {
            return false;
        }

        let key = KeyEvent::new(code, modifiers);
        match binding.action.action(&key) {
            Some(action) => self.resolve_key(&key, state) == Some(action),
            // A binding whose action cannot be produced from its own key (for
            // example text insertion bound to a non-character key) never
            // dispatches and is therefore never advertised.
            None => false,
        }
    }

    /// Resolves one crossterm event against `state` without changing state.
    #[must_use]
    pub fn action_for_event(&self, event: &Event, state: &AppState) -> Option<Action> {
        match event {
            Event::Resize(width, height) => Some(Action::Resize(Viewport::new(*width, *height))),
            Event::Paste(data) if When::INTERACTION_HIDDEN.matches(state) => {
                Some(Action::Paste(data.clone()))
            }
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.resolve_key(key, state)
            }
            _ => None,
        }
    }

    /// Matches `key` against one binding tier.
    ///
    /// Exact-key bindings are matched before the text fallback regardless of
    /// registration order, so an explicit `Ctrl+Alt` command binding always
    /// wins over AltGr character insertion reported as `CONTROL | ALT`.
    fn resolve_key(&self, key: &KeyEvent, state: &AppState) -> Option<Action> {
        [false, true].into_iter().find_map(|text_tier| {
            self.bindings.iter().rev().find_map(|binding| {
                if binding.pattern.is_text() == text_tier
                    && binding.when.matches(state)
                    && binding.pattern.matches(key)
                {
                    binding.action.action(key)
                } else {
                    None
                }
            })
        })
    }
}

impl Default for ActionRegistry {
    fn default() -> Self {
        let mut registry = Self::new();
        registry.register(
            ActionBinding::new(KeyPattern::text(), ActionId::InsertCharacter)
                .when(When::INTERACTION_HIDDEN),
        );
        registry.register(
            ActionBinding::new(
                KeyPattern::exact(KeyCode::Backspace, KeyModifiers::NONE),
                ActionId::Backspace,
            )
            .when(When::INTERACTION_HIDDEN),
        );
        registry.register(
            ActionBinding::new(
                KeyPattern::exact(KeyCode::Enter, KeyModifiers::NONE),
                ActionId::Submit,
            )
            .when(When::INTERACTION_HIDDEN),
        );
        registry.register(
            ActionBinding::new(
                KeyPattern::exact(KeyCode::Enter, KeyModifiers::SHIFT),
                ActionId::InsertNewline,
            )
            .when(When::INTERACTION_HIDDEN),
        );
        registry.register(ActionBinding::new(
            KeyPattern::exact(KeyCode::F(1), KeyModifiers::NONE),
            ActionId::ToggleHelp,
        ));
        registry.register(ActionBinding::new(
            KeyPattern::exact(KeyCode::Char('c'), KeyModifiers::CONTROL),
            ActionId::Quit,
        ));
        registry.register(ActionBinding::new(
            KeyPattern::exact(KeyCode::Char('C'), KeyModifiers::CONTROL),
            ActionId::Quit,
        ));
        for character in ['c', 'C'] {
            registry.register(ActionBinding::new(
                KeyPattern::exact(
                    KeyCode::Char(character),
                    KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
                ),
                ActionId::Quit,
            ));
        }
        for digit in b'1'..=b'9' {
            registry.register(
                ActionBinding::new(
                    KeyPattern::exact(KeyCode::Char(char::from(digit)), KeyModifiers::NONE),
                    ActionId::SelectInteractionOption,
                )
                .when(When::INTERACTION_VISIBLE),
            );
        }
        registry.register(
            ActionBinding::new(
                KeyPattern::exact(KeyCode::Esc, KeyModifiers::NONE),
                ActionId::CancelInteraction,
            )
            .when(When::INTERACTION_VISIBLE),
        );
        registry
    }
}

fn when_always(_: &AppState) -> bool {
    true
}

fn when_help_visible(state: &AppState) -> bool {
    state.is_help_visible()
}

fn when_help_hidden(state: &AppState) -> bool {
    !state.is_help_visible()
}

fn when_input_empty(state: &AppState) -> bool {
    state.input().is_empty()
}

fn when_input_not_empty(state: &AppState) -> bool {
    !state.input().is_empty()
}

fn when_interaction_visible(state: &AppState) -> bool {
    state.interaction().is_some()
}

fn when_interaction_hidden(state: &AppState) -> bool {
    state.interaction().is_none()
}

/// The part of the view invalidated by a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Invalidation {
    /// Text or blocks changed without affecting outer geometry.
    Content,
    /// Viewport geometry changed.
    Layout,
    /// Theme resolution or mapped styles changed.
    Theme,
}

impl Invalidation {
    pub(crate) const fn priority(self) -> u8 {
        match self {
            Self::Content => 1,
            Self::Layout => 2,
            Self::Theme => 3,
        }
    }
}

/// Side-effect request emitted by a pure transition.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Effect {
    /// Redraw the invalidated portion of the view.
    Redraw(Invalidation),
    /// Submit captured input to a host-owned session boundary.
    SubmitInput(String),
    /// Ask the host event loop to terminate cleanly.
    RequestQuit,
    /// Report an interaction answer as data; the host owns any policy.
    InteractionResolved {
        /// Identifier supplied when the prompt was presented.
        request_id: String,
        /// Selected option or a cancel/fail-closed outcome.
        response: InteractionResponse,
    },
}

/// Result of translating and applying one terminal input event.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InputOutcome {
    /// The event has no foundation binding.
    Ignored,
    /// The event was consumed and produced these effects.
    Handled(Vec<Effect>),
}

impl InputOutcome {
    /// Returns emitted effects, or an empty slice for an ignored event.
    #[must_use]
    pub fn effects(&self) -> &[Effect] {
        match self {
            Self::Ignored => &[],
            Self::Handled(effects) => effects,
        }
    }

    /// Returns whether the input event was consumed.
    #[must_use]
    pub const fn is_handled(&self) -> bool {
        matches!(self, Self::Handled(_))
    }
}

/// Owned result of one pure [`reduce`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    state: AppState,
    effects: Vec<Effect>,
}

impl Transition {
    /// Returns the next application state.
    #[must_use]
    pub const fn state(&self) -> &AppState {
        &self.state
    }

    /// Returns effects for the host event loop.
    #[must_use]
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    /// Consumes this transition into state and effects.
    #[must_use]
    pub fn into_parts(self) -> (AppState, Vec<Effect>) {
        (self.state, self.effects)
    }
}

/// Applies `action` to `state` without performing side effects.
#[must_use]
pub fn reduce(state: &AppState, action: Action) -> Transition {
    let previous_transcript = transcript_viewport(state);
    let mut next = state.clone();
    let mut effects = Vec::new();

    match action {
        Action::Insert(character) if !character.is_control() => {
            if next.editor.insert(character) {
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::Insert(_) => {}
        Action::InsertNewline => {
            if next.editor.insert_newline() {
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::Backspace => {
            if next.editor.backspace() {
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::Paste(text) => {
            if next.interaction.is_none() && next.editor.paste(text) {
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::ReplaceInput(input) => {
            if next.editor.replace(input) {
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::Submit => {
            if !next.editor.as_str().trim().is_empty() {
                let input = next.editor.take();
                effects.push(Effect::SubmitInput(input));
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::ReplaceBlocks(blocks) => {
            if next.scrollback.replace(blocks) {
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::SetStatus(status) => {
            if next.status != status {
                next.status = status;
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::PresentInteraction(prompt) => {
            present_interaction(&mut next, prompt, &mut effects);
        }
        Action::SelectInteractionOption(key) => {
            select_interaction_option(&mut next, key, &mut effects);
        }
        Action::CancelInteraction => {
            cancel_interaction(&mut next, &mut effects);
        }
        Action::ScrollBy(older_lines) => {
            let budget = MaterializeBudget::from_viewport(transcript_viewport(&next), 0);
            if next.scrollback.scroll_by(older_lines, budget) {
                effects.push(Effect::Redraw(Invalidation::Content));
            }
        }
        Action::Resize(viewport) => {
            let changed = next.viewport != viewport;
            next.viewport = viewport;
            if changed {
                effects.push(Effect::Redraw(Invalidation::Layout));
            }
            let unreadable = next
                .interaction
                .as_ref()
                .is_some_and(|prompt| !prompt.presentable_in(next.viewport));
            if unreadable && let Some(prompt) = next.interaction.take() {
                effects.push(interaction_cancelled(prompt.request_id()));
                if !changed {
                    effects.push(Effect::Redraw(Invalidation::Layout));
                }
            }
        }
        Action::SelectTheme(selection) => {
            if next.theme_selection != selection {
                next.theme_selection = selection;
                effects.push(Effect::Redraw(Invalidation::Theme));
            }
        }
        Action::DetectBackground(background) => {
            if next.detected_background != background {
                next.detected_background = background;
                // Custom `When` predicates can read the detected background,
                // so live bindings and their hints may change even when an
                // explicit selection keeps theme resolution fixed. Always
                // request a redraw: `Auto` re-resolves the theme, while
                // explicit selections still need a content repaint for the
                // hints that follow binding liveness.
                let invalidation = if matches!(next.theme_selection, ThemeSelection::Auto) {
                    Invalidation::Theme
                } else {
                    Invalidation::Content
                };
                effects.push(Effect::Redraw(invalidation));
            }
        }
        Action::ToggleHelp => {
            next.help_visible = !next.help_visible;
            effects.push(Effect::Redraw(Invalidation::Content));
        }
        Action::Quit => {
            if let Some(prompt) = next.interaction.take() {
                effects.push(interaction_cancelled(prompt.request_id()));
                effects.push(Effect::Redraw(Invalidation::Layout));
            }
            effects.push(Effect::RequestQuit);
        }
    }

    let current_transcript = transcript_viewport(&next);
    if current_transcript != previous_transcript {
        let budget = MaterializeBudget::from_viewport(current_transcript, 0);
        let offset_changed = next.scrollback.scroll_by(0, budget);
        let redraw_pending = effects
            .iter()
            .any(|effect| matches!(effect, Effect::Redraw(_)));
        if offset_changed && !redraw_pending {
            effects.push(Effect::Redraw(Invalidation::Content));
        }
    }

    Transition {
        state: next,
        effects,
    }
}

fn present_interaction(next: &mut AppState, prompt: InteractionPrompt, effects: &mut Vec<Effect>) {
    let incoming_id = prompt.request_id().to_owned();
    if !prompt.presentable_in(next.viewport) {
        effects.push(interaction_cancelled(&incoming_id));
        if let Some(current) = next.interaction.as_ref() {
            let same_id = current.request_id() == incoming_id;
            if same_id || !current.presentable_in(next.viewport) {
                let current_id = current.request_id().to_owned();
                next.interaction = None;
                if !same_id {
                    effects.push(interaction_cancelled(&current_id));
                }
                effects.push(Effect::Redraw(Invalidation::Layout));
            }
        }
        return;
    }

    if let Some(current) = next.interaction.as_ref()
        && current.request_id() != incoming_id
    {
        // Keep the active prompt. The new request is cancelled so its host
        // token cannot hang without an InteractionResolved.
        effects.push(interaction_cancelled(&incoming_id));
        return;
    }

    if next.interaction.as_ref() != Some(&prompt) {
        let invalidation = if next.interaction.is_some() {
            Invalidation::Content
        } else {
            Invalidation::Layout
        };
        next.interaction = Some(prompt);
        effects.push(Effect::Redraw(invalidation));
    }
}

fn select_interaction_option(next: &mut AppState, key: char, effects: &mut Vec<Effect>) {
    let Some(prompt) = next.interaction.as_ref() else {
        return;
    };
    if !prompt.presentable_in(next.viewport) {
        let request_id = prompt.request_id().to_owned();
        next.interaction = None;
        effects.push(interaction_cancelled(&request_id));
        effects.push(Effect::Redraw(Invalidation::Layout));
        return;
    }
    let Some(option_id) = prompt
        .option_for_key(key)
        .map(|option| option.id().to_owned())
    else {
        return;
    };
    let request_id = prompt.request_id().to_owned();
    next.interaction = None;
    effects.push(Effect::InteractionResolved {
        request_id,
        response: InteractionResponse::Selected(option_id),
    });
    effects.push(Effect::Redraw(Invalidation::Layout));
}

fn cancel_interaction(next: &mut AppState, effects: &mut Vec<Effect>) {
    if let Some(prompt) = next.interaction.take() {
        effects.push(interaction_cancelled(prompt.request_id()));
        effects.push(Effect::Redraw(Invalidation::Layout));
    }
}

fn interaction_cancelled(request_id: &str) -> Effect {
    Effect::InteractionResolved {
        request_id: request_id.to_owned(),
        response: InteractionResponse::Cancelled,
    }
}
