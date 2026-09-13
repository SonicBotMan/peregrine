//! Errors for the HLS engine.

#[derive(Debug, thiserror::Error)]
pub enum HlsError {
    #[error("bad playlist: {0}")]
    BadPlaylist(String),
    #[error("unsupported feature: {0}")]
    Unsupported(String),
    #[error("http {status} fetching {url}")]
    Http { status: u16, url: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("decryption failed: {0}")]
    Decrypt(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl HlsError {
    pub fn network(e: impl std::fmt::Display) -> Self {
        Self::Network(e.to_string())
    }
}

impl From<HlsError> for peregrine_api::ApiError {
    fn from(e: HlsError) -> Self {
        use peregrine_api::ApiError;
        match e {
            HlsError::Http { status, url } => ApiError::Http { status, url },
            HlsError::Network(m) => ApiError::Network(m),
            HlsError::Io(io) => ApiError::Io(io.to_string()),
            // Caller-facing "this engine won't ever handle it":
            HlsError::Unsupported(m) => ApiError::UnsupportedUrl(format!("hls: {m}")),
            // A malformed playlist IS a task failure the user sees:
            HlsError::BadPlaylist(m) => ApiError::Internal(format!("hls playlist: {m}")),
            HlsError::Decrypt(m) => ApiError::Internal(format!("hls decrypt: {m}")),
        }
    }
}
