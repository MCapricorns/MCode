//! Terminal capability and background-detection primitives.
//!
//! Capability inputs are supplied by the embedding application. No function in
//! this module reads or prints environment values. OSC 11 probing is optional,
//! writes the real protocol query, and waits through a bounded channel timeout;
//! it does not enable raw mode or retain terminal objects.
use std::io::{self, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::theme::{BackgroundClass, Rgb, relative_luminance};

/// Terminal color depth available to a renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColorCapability {
    /// Disable terminal colors, including when a host honors `NO_COLOR`.
    NoColor,
    /// The basic 16 ANSI colors.
    Basic,
    /// The indexed 256-color ANSI palette.
    Ansi256,
    /// Twenty-four-bit sRGB color.
    TrueColor,
}

/// Capabilities relevant to deterministic TUI rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TerminalCapabilities {
    color: ColorCapability,
    unicode: bool,
}

impl TerminalCapabilities {
    /// Creates an explicit capability description.
    #[must_use]
    pub const fn new(color: ColorCapability, unicode: bool) -> Self {
        Self { color, unicode }
    }

    /// Applies a host-provided `NO_COLOR` decision to detected capabilities.
    ///
    /// The `no_color` boolean should represent configuration or environment
    /// handling performed by the embedding application. This function itself
    /// does not inspect environment values.
    #[must_use]
    pub const fn from_detection(
        detected_color: ColorCapability,
        no_color: bool,
        unicode: bool,
    ) -> Self {
        Self {
            color: if no_color {
                ColorCapability::NoColor
            } else {
                detected_color
            },
            unicode,
        }
    }

    /// Returns the selected color depth.
    #[must_use]
    pub const fn color(self) -> ColorCapability {
        self.color
    }

    /// Returns whether Unicode terminal glyphs are permitted.
    ///
    /// The built-in renderer emits an ASCII-only buffer when this is `false`.
    #[must_use]
    pub const fn supports_unicode(self) -> bool {
        self.unicode
    }

    /// Returns whether any terminal colors are permitted.
    #[must_use]
    pub const fn supports_color(self) -> bool {
        !matches!(self.color, ColorCapability::NoColor)
    }
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self::new(ColorCapability::NoColor, false)
    }
}

/// OSC 11 query asking a terminal for its default background color.
pub const OSC11_QUERY: &[u8] = b"\x1b]11;?\x1b\\";

/// Maximum duration accepted by an OSC 11 probe.
///
/// The cap is intentionally short enough that unsupported terminals cannot
/// stall startup or a raw-mode transition indefinitely.
pub const MAX_OSC11_TIMEOUT: Duration = Duration::from_secs(2);

/// Configuration for one optional OSC 11 probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Osc11ProbeConfig {
    enabled: bool,
    timeout: Duration,
}

impl Osc11ProbeConfig {
    /// Creates a disabled probe that performs no output or waiting.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            timeout: Duration::ZERO,
        }
    }

    /// Creates an enabled probe with a finite, capped timeout.
    #[must_use]
    pub fn enabled(timeout: Duration) -> Self {
        Self {
            enabled: true,
            timeout: timeout.min(MAX_OSC11_TIMEOUT),
        }
    }

    /// Returns whether the query is enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    /// Returns the effective bounded timeout.
    #[must_use]
    pub const fn timeout(self) -> Duration {
        self.timeout
    }
}

impl Default for Osc11ProbeConfig {
    fn default() -> Self {
        Self::disabled()
    }
}

/// Sends a real OSC 11 query and classifies a complete response.
///
/// `responses` is fed by the embedding application's terminal-input pump. A
/// channel is used so this function can enforce `config.timeout` without a
/// blocking terminal read. Run the probe before entering raw mode when
/// possible, or leave it disabled. Timeout, disconnect, malformed data, and an
/// unsupported terminal all return `Ok(None)`.
///
/// # Errors
///
/// Returns an I/O error when writing or flushing the query fails.
pub fn query_background<W: Write>(
    output: &mut W,
    responses: &Receiver<Vec<u8>>,
    config: Osc11ProbeConfig,
) -> io::Result<Option<BackgroundClass>> {
    if !config.enabled {
        return Ok(None);
    }

    output.write_all(OSC11_QUERY)?;
    output.flush()?;

    match responses.recv_timeout(config.timeout) {
        Ok(response) => Ok(parse_osc11_response(&response).map(classify_background)),
        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => Ok(None),
    }
}

/// Parses an OSC 11 default-background response.
///
/// Both BEL and ST terminators are accepted, as are `rgb:r/g/b`,
/// `rgb:rr/gg/bb`, `rgb:rrrr/gggg/bbbb`, `#RGB`, `#RRGGBB`, and
/// `#RRRRGGGGBBBB` payloads. Component values are scaled to eight-bit sRGB.
#[must_use]
pub fn parse_osc11_response(input: &[u8]) -> Option<Rgb> {
    const ESC_PREFIX: &[u8] = b"\x1b]11;";
    const C1_PREFIX: &[u8] = b"\x9d11;";

    let payload = [ESC_PREFIX, C1_PREFIX].into_iter().find_map(|prefix| {
        input
            .windows(prefix.len())
            .position(|window| window == prefix)
            .and_then(|start| terminated_payload(&input[start + prefix.len()..]))
    })?;
    let payload = std::str::from_utf8(payload).ok()?.trim();

    if let Some(components) = payload.strip_prefix("rgb:") {
        parse_rgb_components(components)
    } else if let Some(hex) = payload.strip_prefix('#') {
        parse_hash_color(hex)
    } else {
        None
    }
}

/// Classifies an sRGB terminal background by relative luminance.
#[must_use]
pub fn classify_background(color: Rgb) -> BackgroundClass {
    // A 0.5 linear-luminance split keeps medium terminal grays in the dark
    // class while reserving light palettes for genuinely bright surfaces.
    if relative_luminance(color) >= 0.5 {
        BackgroundClass::Light
    } else {
        BackgroundClass::Dark
    }
}

/// Maps sRGB to the nearest ANSI 256-color palette entry.
///
/// Indices `0..=15` are intentionally excluded because their exact colors are
/// terminal-configurable. The nearest fixed cube or grayscale entry in
/// `16..=255` is returned.
#[must_use]
pub fn rgb_to_ansi256(color: Rgb) -> u8 {
    let mut best_index = 16;
    let mut best_distance = i32::MAX;

    for index in 16_u8..=u8::MAX {
        let candidate = ansi256_rgb(index);
        let red = i32::from(color.red) - i32::from(candidate.red);
        let green = i32::from(color.green) - i32::from(candidate.green);
        let blue = i32::from(color.blue) - i32::from(candidate.blue);
        let distance = red * red + green * green + blue * blue;
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }

    best_index
}

fn terminated_payload(input: &[u8]) -> Option<&[u8]> {
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            0x07 | 0x9c => return Some(&input[..index]),
            0x1b if input.get(index + 1) == Some(&b'\\') => return Some(&input[..index]),
            _ => index += 1,
        }
    }
    None
}

fn parse_rgb_components(input: &str) -> Option<Rgb> {
    let mut components = input.split('/');
    let red = parse_scaled_component(components.next()?)?;
    let green = parse_scaled_component(components.next()?)?;
    let blue = parse_scaled_component(components.next()?)?;
    if components.next().is_some() {
        return None;
    }
    Some(Rgb::new(red, green, blue))
}

fn parse_hash_color(input: &str) -> Option<Rgb> {
    if !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let component_length = match input.len() {
        3 => 1,
        6 => 2,
        12 => 4,
        _ => return None,
    };
    let red = parse_scaled_component(&input[..component_length])?;
    let green = parse_scaled_component(&input[component_length..component_length * 2])?;
    let blue = parse_scaled_component(&input[component_length * 2..])?;
    Some(Rgb::new(red, green, blue))
}

fn parse_scaled_component(input: &str) -> Option<u8> {
    if input.is_empty() || input.len() > 4 || !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(input, 16).ok()?;
    let bit_count = u32::try_from(input.len()).ok()?.checked_mul(4)?;
    let maximum = (1_u32.checked_shl(bit_count)?).checked_sub(1)?;
    let scaled = (value * 255 + maximum / 2) / maximum;
    u8::try_from(scaled).ok()
}

fn ansi256_rgb(index: u8) -> Rgb {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    if index < 232 {
        let offset = index - 16;
        let red = usize::from(offset / 36);
        let green = usize::from((offset % 36) / 6);
        let blue = usize::from(offset % 6);
        Rgb::new(LEVELS[red], LEVELS[green], LEVELS[blue])
    } else {
        let level = 8 + (index - 232) * 10;
        Rgb::new(level, level, level)
    }
}
