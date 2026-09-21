//! MSVC-style Windows argv quoting for structured exec.
//!
//! `CreateProcessW` receives a single UTF-16 command line. argv0 is always
//! quoted. Empty arguments and arguments containing whitespace or `"` are
//! wrapped; `n` backslashes immediately before a quote become `2n+1`
//! backslashes plus the escaped quote, and `n` trailing backslashes before a
//! closing quote become `2n`.
use std::ffi::OsStr;

use crate::tool::ToolError;

/// Documented `CreateProcessW` UTF-16 command-line limit, including NUL.
pub(super) const WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS: usize = 32_767;

/// Encode `argv0` plus `args` as a UTF-16 command line without terminator.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArgs`] when an argument contains an interior
/// NUL or the encoded command line exceeds 32,767 UTF-16 units including NUL.
pub(super) fn windows_command_line_utf16(
    argv0: &OsStr,
    args: &[String],
) -> Result<Vec<u16>, ToolError> {
    let mut cmd = Vec::new();
    append_quoted(&mut cmd, encode_os(argv0)?, true);
    for arg in args {
        reject_nul(arg, "argument")?;
        cmd.push(u16::from(b' '));
        append_quoted(&mut cmd, arg.encode_utf16(), needs_quotes(arg.chars()));
    }
    let with_terminator = cmd.len().saturating_add(1);
    if with_terminator > WINDOWS_COMMAND_LINE_LIMIT_UTF16_UNITS {
        return Err(ToolError::InvalidArgs(format!(
            "command line is too long for CreateProcessW's 32,767 UTF-16-code-unit limit \
             (including the terminator): encoded length is {with_terminator}"
        )));
    }
    Ok(cmd)
}

fn encode_os(value: &OsStr) -> Result<Vec<u16>, ToolError> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let units: Vec<u16> = value.encode_wide().collect();
        if units.contains(&0) {
            return Err(ToolError::InvalidArgs(
                "program path contains an interior NUL".into(),
            ));
        }
        Ok(units)
    }
    #[cfg(not(windows))]
    {
        reject_nul(&value.to_string_lossy(), "program path")?;
        Ok(value.to_string_lossy().encode_utf16().collect())
    }
}

fn needs_quotes<I>(chars: I) -> bool
where
    I: IntoIterator<Item = char>,
{
    let mut empty = true;
    for c in chars {
        empty = false;
        if matches!(c, ' ' | '\t' | '\n' | '\r' | '"') {
            return true;
        }
    }
    empty
}

fn append_quoted<I>(cmd: &mut Vec<u16>, units: I, quote: bool)
where
    I: IntoIterator<Item = u16>,
{
    if quote {
        cmd.push(u16::from(b'"'));
    }
    let mut backslashes = 0usize;
    for unit in units {
        if unit == u16::from(b'\\') {
            backslashes += 1;
            cmd.push(unit);
            continue;
        }
        if unit == u16::from(b'"') {
            cmd.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            backslashes = 0;
            cmd.push(unit);
            continue;
        }
        backslashes = 0;
        cmd.push(unit);
    }
    if quote {
        cmd.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
        cmd.push(u16::from(b'"'));
    }
}

fn reject_nul(value: &str, what: &str) -> Result<(), ToolError> {
    if value.contains('\0') {
        Err(ToolError::InvalidArgs(format!(
            "{what} contains an interior NUL"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn quote_windows_arg(arg: &str) -> String {
    let mut cmd = Vec::new();
    append_quoted(&mut cmd, arg.encode_utf16(), needs_quotes(arg.chars()));
    String::from_utf16_lossy(&cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_line(argv0: &str, args: &[&str]) -> String {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let units = windows_command_line_utf16(OsStr::new(argv0), &owned).unwrap();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn empty_argument_is_quoted() {
        assert_eq!(quote_windows_arg(""), "\"\"");
        assert_eq!(command_line("app.exe", &[""]), "\"app.exe\" \"\"");
    }

    #[test]
    fn plain_and_unicode_arguments_stay_unquoted() {
        assert_eq!(quote_windows_arg("cargo"), "cargo");
        assert_eq!(quote_windows_arg("中文"), "中文");
        assert_eq!(
            command_line("app.exe", &["-v", "中文"]),
            "\"app.exe\" -v 中文"
        );
    }

    #[test]
    fn spaces_and_tabs_force_quotes() {
        assert_eq!(quote_windows_arg("hello world"), "\"hello world\"");
        assert_eq!(quote_windows_arg("hello\tworld"), "\"hello\tworld\"");
    }

    #[test]
    fn embedded_quotes_are_escaped_and_wrapped() {
        assert_eq!(quote_windows_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_windows_arg("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn backslash_runs_before_internal_quotes_are_doubled() {
        assert_eq!(quote_windows_arg("foo\\\"bar"), "\"foo\\\\\\\"bar\"");
        assert_eq!(quote_windows_arg("foo\\\\\"bar"), "\"foo\\\\\\\\\\\"bar\"");
        assert_eq!(quote_windows_arg("a\\b c"), "\"a\\b c\"");
    }

    #[test]
    fn trailing_backslash_runs_before_the_closing_quote_are_doubled() {
        assert_eq!(quote_windows_arg("a b\\"), "\"a b\\\\\"");
        assert_eq!(quote_windows_arg("a b\\\\"), "\"a b\\\\\\\\\"");
        assert_eq!(quote_windows_arg("ends\\"), "ends\\");
        assert_eq!(quote_windows_arg("dir\\ "), "\"dir\\ \"");
    }

    #[test]
    fn argv0_is_always_quoted() {
        assert_eq!(command_line("C:\\app.exe", &[]), "\"C:\\app.exe\"");
        assert_eq!(
            command_line("C:\\Program Files\\app.exe", &[]),
            "\"C:\\Program Files\\app.exe\""
        );
    }

    #[test]
    fn command_line_joins_every_quoting_case() {
        let line = command_line(
            "C:\\app.exe",
            &[
                "",
                "hello world",
                "say \"hi\"",
                "ends\\",
                "中文",
                "foo\\\"bar",
            ],
        );
        assert_eq!(
            line,
            "\"C:\\app.exe\" \"\" \"hello world\" \"say \\\"hi\\\"\" ends\\ 中文 \"foo\\\\\\\"bar\""
        );
    }

    #[test]
    fn interior_nul_is_rejected() {
        let owned = vec!["ok\0bad".to_owned()];
        let err = windows_command_line_utf16(OsStr::new("app"), &owned).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
        assert!(err.to_string().contains("NUL"), "{err}");
    }

    #[test]
    fn overlong_command_line_is_rejected() {
        let filler = "x".repeat(32_760);
        let err = windows_command_line_utf16(OsStr::new("app.exe"), &[filler]).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
        assert!(err.to_string().contains("32,767 UTF-16-code-unit"), "{err}");
    }
}
