//! Core error type shared across MYCode crates.

use serde::{Deserialize, Serialize};

/// Unified error type for MYCode.
///
/// All payloads are strings so the type stays `Clone` + `Serialize`: errors
/// travel through `AgentEvent` broadcasts. Subsystems keep their detailed
/// error types locally and convert at the boundary via the `From` impls or by
/// formatting into the matching variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum MycodeError {
    /// Filesystem or general I/O failure.
    #[error("io error: {0}")]
    Io(String),
    /// JSON (de)serialization failure.
    #[error("serde error: {0}")]
    Serde(String),
    /// LLM provider failure (network, auth, rate limit, API error).
    #[error("provider error: {0}")]
    Provider(String),
    /// Tool execution failure.
    #[error("tool error: {0}")]
    Tool(String),
    /// Plugin loading or hook failure.
    #[error("plugin error: {0}")]
    Plugin(String),
}

impl From<std::io::Error> for MycodeError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<serde_json::Error> for MycodeError {
    fn from(err: serde_json::Error) -> Self {
        Self::Serde(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_variants() -> Vec<MycodeError> {
        vec![
            MycodeError::Io("disk full".into()),
            MycodeError::Serde("bad json".into()),
            MycodeError::Provider("rate limited".into()),
            MycodeError::Tool("exit code 1".into()),
            MycodeError::Plugin("wit bind failed".into()),
        ]
    }

    #[test]
    fn error_roundtrip() {
        for err in all_variants() {
            let json = serde_json::to_string(&err).unwrap();
            let back: MycodeError = serde_json::from_str(&json).unwrap();
            assert_eq!(back, err);
        }
    }

    #[test]
    fn converts_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing file");
        let err = MycodeError::from(io_err);
        assert!(matches!(err, MycodeError::Io(_)));
        assert!(err.to_string().contains("missing file"));
    }

    #[test]
    fn converts_from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let err = MycodeError::from(json_err);
        assert!(matches!(err, MycodeError::Serde(_)));
    }
}
