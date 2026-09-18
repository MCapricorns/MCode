//! Root pure-state view and crossterm input translation.
//!
//! [`AppView`] owns state, an injectable [`ActionRegistry`], theme resolution,
//! and redraw invalidation. It emits plain [`Effect`] values but
//! never executes them.
use crossterm::event::Event;
use ratatui::Frame;

use crate::actions::{Action, ActionRegistry, Effect, InputOutcome, Invalidation, reduce};
use crate::render;
use crate::state::{AppState, Viewport};
use crate::terminal::TerminalCapabilities;
use crate::theme::{BackgroundClass, Theme, ThemeResolution, ThemeSelection, resolve_theme};

/// Root component for deterministic input, updates, and drawing.
#[derive(Debug, Clone)]
pub struct AppView {
    state: AppState,
    capabilities: TerminalCapabilities,
    action_registry: ActionRegistry,
    named_themes: Vec<Theme>,
    theme: ThemeResolution,
    invalidation: Option<Invalidation>,
}

impl AppView {
    /// Creates an empty view with automatic theme selection.
    #[must_use]
    pub fn new(viewport: Viewport, capabilities: TerminalCapabilities) -> Self {
        let state = AppState::new(viewport);
        let theme = resolve_theme(state.theme_selection(), state.detected_background(), &[]);
        Self {
            state,
            capabilities,
            action_registry: ActionRegistry::default(),
            named_themes: Vec::new(),
            theme,
            invalidation: Some(Invalidation::Layout),
        }
    }

    /// Installs caller-provided keyboard bindings.
    #[must_use]
    pub fn with_action_registry(mut self, action_registry: ActionRegistry) -> Self {
        self.action_registry = action_registry;
        self
    }

    /// Applies an initial theme selection.
    #[must_use]
    pub fn with_theme_selection(mut self, selection: ThemeSelection) -> Self {
        self.dispatch(Action::SelectTheme(selection));
        self
    }

    /// Applies an initial detected background.
    #[must_use]
    pub fn with_detected_background(mut self, background: Option<BackgroundClass>) -> Self {
        self.dispatch(Action::DetectBackground(background));
        self
    }

    /// Installs caller-provided named themes.
    #[must_use]
    pub fn with_named_themes(mut self, named_themes: Vec<Theme>) -> Self {
        self.named_themes = named_themes;
        let resolved = resolve_theme(
            self.state.theme_selection(),
            self.state.detected_background(),
            &self.named_themes,
        );
        if resolved != self.theme {
            self.theme = resolved;
            self.merge_invalidation(Invalidation::Theme);
        }
        self
    }

    /// Returns the current pure state.
    #[must_use]
    pub const fn state(&self) -> &AppState {
        &self.state
    }

    /// Returns terminal rendering capabilities supplied by the host.
    #[must_use]
    pub const fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }

    /// Returns the installed input action registry.
    #[must_use]
    pub const fn action_registry(&self) -> &ActionRegistry {
        &self.action_registry
    }

    /// Replaces the installed input action registry.
    ///
    /// Status hints and help panels are generated from the registry, so
    /// replacing it merges a [`Invalidation::Content`] redraw request. Hosts
    /// driving redraws from [`AppView::invalidation`] therefore repaint the
    /// new keys without waiting for an unrelated state change.
    pub fn set_action_registry(&mut self, action_registry: ActionRegistry) {
        self.action_registry = action_registry;
        self.merge_invalidation(Invalidation::Content);
    }

    /// Returns current theme resolution and fallback source.
    #[must_use]
    pub const fn theme_resolution(&self) -> &ThemeResolution {
        &self.theme
    }

    /// Returns pending redraw invalidation without clearing it.
    #[must_use]
    pub const fn invalidation(&self) -> Option<Invalidation> {
        self.invalidation
    }

    /// Takes and clears pending redraw invalidation.
    #[must_use]
    pub fn take_invalidation(&mut self) -> Option<Invalidation> {
        self.invalidation.take()
    }

    /// Applies one action and returns host effects.
    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        let transition = reduce(&self.state, action);
        let (state, effects) = transition.into_parts();
        self.state = state;

        for effect in &effects {
            if let Effect::Redraw(invalidation) = effect {
                self.merge_invalidation(*invalidation);
            }
        }

        self.theme = resolve_theme(
            self.state.theme_selection(),
            self.state.detected_background(),
            &self.named_themes,
        );
        effects
    }

    /// Translates and applies one crossterm event.
    pub fn handle_input(&mut self, event: &Event) -> InputOutcome {
        match self.action_registry.action_for_event(event, &self.state) {
            Some(action) => InputOutcome::Handled(self.dispatch(action)),
            None => InputOutcome::Ignored,
        }
    }

    /// Draws the current state and clears pending invalidation.
    ///
    /// Key hints are rendered from the injected [`ActionRegistry`] against
    /// the current state, so displayed keys always match dispatch.
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        render::draw(
            frame,
            &self.state,
            &self.action_registry,
            self.theme.theme(),
            self.capabilities,
        );
        self.invalidation = None;
    }

    fn merge_invalidation(&mut self, incoming: Invalidation) {
        self.invalidation = Some(match self.invalidation {
            Some(current) if current.priority() >= incoming.priority() => current,
            _ => incoming,
        });
    }
}

impl Default for AppView {
    fn default() -> Self {
        Self::new(Viewport::default(), TerminalCapabilities::default())
    }
}

/// Converts a crossterm event using the default action registry and state.
///
/// Use [`ActionRegistry::action_for_event`] or [`AppView::handle_input`] when
/// bindings or context predicates are customized. Key-release events are
/// ignored; ordinary text input including AltGr characters reported as
/// `CONTROL | ALT` is inserted by the defaults.
#[must_use]
pub fn action_for_event(event: &Event) -> Option<Action> {
    ActionRegistry::default().action_for_event(event, &AppState::default())
}
