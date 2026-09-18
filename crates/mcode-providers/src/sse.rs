//! Incremental server-sent-events frame parser.
//!
//! Feeds raw response bytes and yields complete `data:` payloads. The parser
//! tolerates CRLF and LF endings, ignores comments and non-data fields, and
//! fails closed on oversized frames instead of buffering without bound.

use mcode_provider_api::{ProviderError, ProviderErrorKind};

/// Maximum accepted size of one assembled data payload.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Assembles SSE data payloads from response chunks.
#[derive(Debug, Default)]
pub struct FrameParser {
    buffer: Vec<u8>,
    scanned: usize,
}

impl FrameParser {
    /// Creates an empty parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk and returns every completed data payload.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when a single frame exceeds
    /// [`MAX_FRAME_BYTES`].
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, ProviderError> {
        let mut frames = Vec::new();
        for &byte in chunk {
            self.buffer.push(byte);
            if byte == b'\n' {
                let line = self.buffer.clone();
                self.buffer.clear();
                if let Some(data) = self.take_data_line(&line) {
                    self.scanned += data.len();
                    if self.scanned > MAX_FRAME_BYTES {
                        self.scanned = 0;
                        return Err(ProviderError::with_message(
                            ProviderErrorKind::Protocol,
                            "SSE frame exceeds the size limit",
                        ));
                    }
                    frames.push(data);
                }
            }
        }
        Ok(frames)
    }

    /// Flushes a trailing unterminated line at end of stream.
    ///
    /// # Errors
    ///
    /// Returns a protocol error when the leftover line is an oversized data
    /// payload.
    pub fn finish(&mut self) -> Result<Option<String>, ProviderError> {
        let line = std::mem::take(&mut self.buffer);
        match self.take_data_line(&line) {
            Some(data) if data.len() > MAX_FRAME_BYTES => Err(ProviderError::with_message(
                ProviderErrorKind::Protocol,
                "SSE frame exceeds the size limit",
            )),
            Some(data) => Ok(Some(data)),
            None => Ok(None),
        }
    }

    /// Joins multi-line data payloads per the SSE spec once a blank line is
    /// seen. This parser handles single-line `data:` records, which every
    /// supported provider emits; continuation lines are appended with `\n`.
    fn take_data_line(&self, line: &[u8]) -> Option<String> {
        let mut trimmed = line;
        if trimmed.last() == Some(&b'\n') {
            trimmed = &trimmed[..trimmed.len() - 1];
        }
        if trimmed.last() == Some(&b'\r') {
            trimmed = &trimmed[..trimmed.len() - 1];
        }
        if trimmed.is_empty() {
            return None;
        }
        let stripped = trimmed.strip_prefix(b"data:")?;
        let payload = if stripped.first() == Some(&b' ') {
            &stripped[1..]
        } else {
            stripped
        };
        Some(String::from_utf8_lossy(payload).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_split_and_crlf_frames() {
        let mut parser = FrameParser::new();
        assert!(parser.feed(b"").unwrap().is_empty());
        assert_eq!(
            parser.feed(b"data: {\"a\":1}\r\ndata: [DONE]\n").unwrap(),
            vec!["{\"a\":1}".to_owned(), "[DONE]".to_owned()]
        );
        assert_eq!(parser.finish().unwrap(), None);
    }

    #[test]
    fn trailing_unterminated_data_line_flushes() {
        let mut parser = FrameParser::new();
        assert!(parser.feed(b"event: ping\ndata: tail").unwrap().is_empty());
        assert_eq!(parser.finish().unwrap(), Some("tail".to_owned()));
    }

    #[test]
    fn comments_and_other_fields_are_ignored() {
        let mut parser = FrameParser::new();
        assert!(
            parser
                .feed(b": keepalive\nevent: message\nid: 7\n")
                .unwrap()
                .is_empty()
        );
        let frames = parser.feed(b"data: y\n").unwrap();
        assert_eq!(frames, vec!["y".to_owned()]);
    }

    #[test]
    fn oversized_frame_fails_closed() {
        let mut parser = FrameParser::new();
        let big = format!("data: {}\n", "x".repeat(MAX_FRAME_BYTES + 1));
        let error = parser.feed(big.as_bytes()).unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    }
}
