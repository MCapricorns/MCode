use std::collections::HashSet;
use std::sync::mpsc;
use std::time::Duration;

use mcode_tui::labels::BUILTIN_UI_LABELS;
use mcode_tui::{
    BackgroundClass, ColorCapability, Osc11ProbeConfig, Rgb, SemanticToken, TerminalCapabilities,
    Theme, ThemeAppearance, ThemeSelection, ThemeSource, classify_background, contrast_ratio,
    mcode_dark, mcode_light, parse_osc11_response, query_background, resolve_theme, rgb_to_ansi256,
};

#[test]
fn built_in_themes_cover_every_semantic_token() {
    let token_names = SemanticToken::ALL
        .iter()
        .map(|token| token.as_str())
        .collect::<HashSet<_>>();

    assert_eq!(SemanticToken::ALL.len(), SemanticToken::COUNT);
    assert_eq!(token_names.len(), SemanticToken::COUNT);
    assert!(token_names.iter().all(|name| name.is_ascii()));

    for theme in [mcode_dark(), mcode_light()] {
        assert_eq!(theme.colors().len(), SemanticToken::COUNT);
        for token in SemanticToken::ALL {
            assert_eq!(theme.color(token), theme.colors()[token as usize]);
        }
    }
}

#[test]
fn built_in_palettes_preserve_required_visual_semantics() {
    let dark = mcode_dark();
    let light = mcode_light();

    assert_eq!(dark.name(), "mcode-dark");
    assert_eq!(dark.appearance(), ThemeAppearance::Dark);
    assert_eq!(
        dark.color(SemanticToken::Accent),
        Rgb::new(0x44, 0xdf, 0x6c)
    );
    assert_eq!(
        dark.color(SemanticToken::ToolTitle),
        Rgb::new(0xe9, 0xa1, 0x70)
    );
    assert_ne!(
        dark.color(SemanticToken::SyntaxKeyword),
        dark.color(SemanticToken::SyntaxFunction)
    );
    assert_ne!(
        dark.color(SemanticToken::SyntaxFunction),
        dark.color(SemanticToken::SyntaxOperator)
    );

    assert_eq!(light.name(), "mcode-light");
    assert_eq!(light.appearance(), ThemeAppearance::Light);
    let inverted_dark_background = dark.color(SemanticToken::Background);
    assert_ne!(
        light.color(SemanticToken::Background),
        Rgb::new(
            255 - inverted_dark_background.red,
            255 - inverted_dark_background.green,
            255 - inverted_dark_background.blue,
        )
    );
}

#[test]
fn text_pairs_have_wcag_like_contrast() {
    for theme in [mcode_dark(), mcode_light()] {
        for (foreground, background, minimum) in [
            (SemanticToken::TextPrimary, SemanticToken::Background, 7.0),
            (SemanticToken::TextMuted, SemanticToken::Background, 4.5),
            (SemanticToken::TextDim, SemanticToken::Background, 4.5),
            (SemanticToken::Accent, SemanticToken::Background, 4.5),
            (
                SemanticToken::InputText,
                SemanticToken::InputBackground,
                7.0,
            ),
            (
                SemanticToken::StatusText,
                SemanticToken::StatusBackground,
                4.5,
            ),
            (
                SemanticToken::SelectionText,
                SemanticToken::SelectionBackground,
                7.0,
            ),
        ] {
            let ratio = contrast_ratio(theme.color(foreground), theme.color(background));
            assert!(
                ratio >= minimum,
                "{} {} on {} has contrast {ratio:.2}, expected {minimum:.2}",
                theme.name(),
                foreground.as_str(),
                background.as_str(),
            );
        }
    }
}

#[test]
fn explicit_selection_wins_and_auto_has_a_dark_fallback() {
    let explicit = resolve_theme(&ThemeSelection::Dark, Some(BackgroundClass::Light), &[]);
    assert_eq!(explicit.theme().name(), "mcode-dark");
    assert_eq!(explicit.source(), ThemeSource::Explicit);

    let detected = resolve_theme(&ThemeSelection::Auto, Some(BackgroundClass::Light), &[]);
    assert_eq!(detected.theme().name(), "mcode-light");
    assert_eq!(detected.source(), ThemeSource::Detected);

    let fallback = resolve_theme(&ThemeSelection::Auto, None, &[]);
    assert_eq!(fallback.theme().name(), "mcode-dark");
    assert_eq!(fallback.source(), ThemeSource::Fallback);
}

#[test]
fn named_selection_supports_custom_themes_and_missing_name_fallback() {
    let custom = Theme::new(
        "paper",
        ThemeAppearance::Light,
        [Rgb::new(20, 30, 40); SemanticToken::COUNT],
    );
    let resolved = resolve_theme(
        &ThemeSelection::Named("paper".into()),
        Some(BackgroundClass::Dark),
        std::slice::from_ref(&custom),
    );
    assert_eq!(resolved.theme(), &custom);
    assert_eq!(resolved.source(), ThemeSource::Explicit);

    let missing = resolve_theme(
        &ThemeSelection::Named("missing".into()),
        Some(BackgroundClass::Light),
        &[custom],
    );
    assert_eq!(missing.theme().name(), "mcode-dark");
    assert_eq!(missing.source(), ThemeSource::Fallback);
}

#[test]
fn no_color_overrides_detected_depth_without_reading_environment() {
    let capabilities = TerminalCapabilities::from_detection(ColorCapability::TrueColor, true, true);
    assert_eq!(capabilities.color(), ColorCapability::NoColor);
    assert!(!capabilities.supports_color());
    assert!(capabilities.supports_unicode());

    let basic = TerminalCapabilities::from_detection(ColorCapability::Basic, false, true);
    assert_eq!(basic.color(), ColorCapability::Basic);
    assert!(basic.supports_color());
}

#[test]
fn rgb_to_ansi256_uses_fixed_cube_and_grayscale_entries() {
    assert_eq!(rgb_to_ansi256(Rgb::new(255, 0, 0)), 196);
    assert_eq!(rgb_to_ansi256(Rgb::new(0, 255, 0)), 46);
    assert_eq!(rgb_to_ansi256(Rgb::new(128, 128, 128)), 244);
    assert_eq!(rgb_to_ansi256(Rgb::new(255, 255, 255)), 231);
}

#[test]
fn osc11_parser_accepts_common_component_sizes_and_terminators() {
    assert_eq!(
        parse_osc11_response(b"\x1b]11;rgb:1111/2222/eeee\x07"),
        Some(Rgb::new(17, 34, 238))
    );
    assert_eq!(
        parse_osc11_response(b"prefix\x1b]11;rgb:00/80/ff\x1b\\suffix"),
        Some(Rgb::new(0, 128, 255))
    );
    assert_eq!(
        parse_osc11_response(b"\x9d11;#fff\x9c"),
        Some(Rgb::new(255, 255, 255))
    );
    assert_eq!(parse_osc11_response("\u{1b}]11;#éx\u{7}".as_bytes()), None);
    assert_eq!(parse_osc11_response(b"\x1b]11;rgb:00/00/00"), None);
}

#[test]
fn background_classifier_distinguishes_bright_and_dark_surfaces() {
    assert_eq!(
        classify_background(Rgb::new(0, 0, 0)),
        BackgroundClass::Dark
    );
    assert_eq!(
        classify_background(Rgb::new(255, 255, 255)),
        BackgroundClass::Light
    );
    assert_eq!(
        classify_background(Rgb::new(100, 100, 100)),
        BackgroundClass::Dark
    );
}

#[test]
fn osc11_query_is_disabled_or_time_bounded() {
    let (_sender, responses) = mpsc::channel();
    let mut output = Vec::new();
    let result = query_background(&mut output, &responses, Osc11ProbeConfig::disabled())
        .expect("disabled probing must succeed");
    assert_eq!(result, None);
    assert!(output.is_empty());

    let config = Osc11ProbeConfig::enabled(Duration::from_secs(30));
    assert_eq!(config.timeout(), Duration::from_secs(2));
    let result = query_background(
        &mut output,
        &responses,
        Osc11ProbeConfig::enabled(Duration::ZERO),
    )
    .expect("zero-timeout probing must return");
    assert_eq!(result, None);
    assert_eq!(output, mcode_tui::terminal::OSC11_QUERY);
}

#[test]
fn osc11_query_classifies_a_received_response() {
    let (sender, responses) = mpsc::channel();
    sender
        .send(b"\x1b]11;rgb:ffff/ffff/ffff\x07".to_vec())
        .expect("test response channel must be open");
    let mut output = Vec::new();

    let background = query_background(
        &mut output,
        &responses,
        Osc11ProbeConfig::enabled(Duration::from_millis(50)),
    )
    .expect("probe write must succeed");

    assert_eq!(background, Some(BackgroundClass::Light));
    assert_eq!(output, mcode_tui::terminal::OSC11_QUERY);
}

#[test]
fn every_fixed_product_label_is_ascii_english_copy() {
    assert!(!BUILTIN_UI_LABELS.is_empty());
    assert!(BUILTIN_UI_LABELS.iter().all(|label| label.is_ascii()));
    assert!(
        BUILTIN_UI_LABELS
            .iter()
            .all(|label| !label.trim().is_empty())
    );
}
