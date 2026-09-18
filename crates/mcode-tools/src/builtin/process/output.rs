//! Bounded concurrent stdout/stderr capture for native execution builtins.
//!
//! Streams are drained together while a fixed retained prefix is kept. The
//! retained size covers a UTF-16 BOM plus two raw bytes per rendered UTF-8
//! byte so truncation remains observable after decoding.
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
use std::process::ExitStatus;

use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
use tokio::process::{Child, ChildStderr, ChildStdout};

/// Combined stdout+stderr cap per call (~50 KiB, then a notice).
pub(crate) const MAX_OUTPUT_BYTES: usize = 50 * 1024;

/// A fixed scratch buffer keeps discarded output from growing user-space memory.
const OUTPUT_READ_CHUNK_BYTES: usize = 16 * 1024;
/// BOM-marked UTF-16 ASCII uses two raw bytes per rendered UTF-8 byte. Retain
/// one rendered byte beyond the output cap so truncation remains observable.
pub(crate) const MAX_RETAINED_OUTPUT_BYTES: usize = 2 + 2 * (MAX_OUTPUT_BYTES + 1);

/// A bounded stream prefix and the total bytes drained from that stream.
#[derive(Debug, Default)]
pub(crate) struct CapturedStream {
    pub(crate) retained: Vec<u8>,
    pub(crate) total_bytes: u64,
}

impl CapturedStream {
    /// An empty capture with the retained-prefix capacity reserved.
    pub(crate) fn new() -> Self {
        Self {
            retained: Vec::with_capacity(MAX_RETAINED_OUTPUT_BYTES),
            total_bytes: 0,
        }
    }
}

/// Drain stdout and stderr concurrently, then wait for the leader.
///
/// Do not move the wait above the pipe-drain barrier. On Unix, an unreaped
/// live/zombie leader reserves its PID and therefore its PGID number while an
/// escaped descendant can keep collection pending.
///
/// # Errors
///
/// Returns the first I/O error from either pipe or from `Child::wait`.
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64"))]
pub(crate) async fn collect_child_output(
    child: &mut Child,
    stdout_pipe: &mut Option<ChildStdout>,
    stderr_pipe: &mut Option<ChildStderr>,
    stdout: &mut CapturedStream,
    stderr: &mut CapturedStream,
) -> std::io::Result<ExitStatus> {
    drain_pipes(stdout_pipe, stderr_pipe, stdout, stderr).await?;
    child.wait().await
}

/// Drain stdout and stderr concurrently while retaining bounded prefixes.
///
/// # Errors
///
/// Returns the first I/O error from either pipe.
pub(crate) async fn drain_pipes<Out, Err>(
    stdout_pipe: &mut Option<Out>,
    stderr_pipe: &mut Option<Err>,
    stdout: &mut CapturedStream,
    stderr: &mut CapturedStream,
) -> std::io::Result<()>
where
    Out: AsyncRead + Unpin,
    Err: AsyncRead + Unpin,
{
    let (out, err) = tokio::join!(
        read_bounded(stdout_pipe, stdout),
        read_bounded(stderr_pipe, stderr),
    );
    out.and(err)
}

/// Drain one stream to EOF while retaining only the prefix needed for rendering.
///
/// Continuing to read prevents capture limits from changing the child's exit
/// behavior, and a fixed scratch buffer bounds memory after the prefix is full.
///
/// # Errors
///
/// Returns an I/O error from the reader or when the total length overflows
/// `u64`.
pub(crate) async fn read_bounded<R>(
    pipe: &mut Option<R>,
    captured: &mut CapturedStream,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let Some(reader) = pipe.as_mut() else {
        return Ok(());
    };
    let retained_limit = MAX_RETAINED_OUTPUT_BYTES;
    let mut chunk = [0_u8; OUTPUT_READ_CHUNK_BYTES];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        captured.total_bytes = captured
            .total_bytes
            .checked_add(u64::try_from(count).map_err(|_| {
                std::io::Error::other("process output read length does not fit u64")
            })?)
            .ok_or_else(|| std::io::Error::other("process output length overflowed u64"))?;
        let retained = retained_limit.saturating_sub(captured.retained.len());
        captured
            .retained
            .extend_from_slice(&chunk[..count.min(retained)]);
    }
    drop(pipe.take());
    Ok(())
}

/// Decode captured process text without consulting a legacy system code page.
pub(crate) fn decode_captured_text(bytes: &[u8]) -> String {
    if let Some(payload) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(payload).into_owned();
    }
    if let Some(payload) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16(payload, u16::from_le_bytes);
    }
    if let Some(payload) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16(payload, u16::from_be_bytes);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16(payload: &[u8], decode_unit: fn([u8; 2]) -> u16) -> String {
    let (chunks, remainder) = payload.as_chunks::<2>();
    let units = chunks.iter().copied().map(decode_unit).collect::<Vec<_>>();
    let mut decoded = String::from_utf16_lossy(&units);
    if !remainder.is_empty() {
        decoded.push('\u{fffd}');
    }
    decoded
}
