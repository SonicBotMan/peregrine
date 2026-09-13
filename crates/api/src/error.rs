//! Shared error type for the api surface.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
pub enum ApiError {
    #[error("task not found: {0}")]
    TaskNotFound(String),
    #[error("no engine supports url: {0}")]
    UnsupportedUrl(String),
    #[error("duplicate engine: {0}")]
    DuplicateEngine(String),
    #[error("http {status} from {url}")]
    Http { status: u16, url: String },
    #[error("too many redirects chasing {0}")]
    TooManyRedirects(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("storage error: {0}")]
    Storage(String),
    /// Not a failure — a routing directive: the segmented engine
    /// cannot proceed (server ignores Range) and the caller should
    /// restart the job as a single stream. Structural, not string-
    /// matched, so the auto-router (M1-c2) and the M2 task layer can
    /// both act on it deterministically.
    #[error("single stream required: {reason}")]
    SingleStreamRequired { reason: String },
    #[error("internal: {0}")]
    Internal(String),
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_and_from_io() {
        let e = ApiError::TaskNotFound("t1".into());
        assert_eq!(e.to_string(), "task not found: t1");
        let io: ApiError = std::io::Error::other("boom").into();
        assert_eq!(io.to_string(), "io error: boom");
        let d = ApiError::SingleStreamRequired {
            reason: "server ignores Range".into(),
        };
        assert_eq!(
            d.to_string(),
            "single stream required: server ignores Range"
        );
    }

    #[test]
    fn error_serde_roundtrip() {
        let e = ApiError::UnsupportedUrl("magnet:?x".into());
        let back: ApiError = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
}
