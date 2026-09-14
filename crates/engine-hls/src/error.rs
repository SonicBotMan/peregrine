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
    /// Upstream live stream went silent (window stopped sliding, no
    /// ENDLIST). A TRANSIENT/retryable condition — mapped to Network
    /// at the API boundary, never to UnsupportedUrl (R2 P2-2: a dead
    /// stream is not an unsupported URL).
    #[error("live stream stalled: {0}")]
    LiveStalled(String),
    /// A live recording's seq set has a hole (window slid past a
    /// segment we could not fetch). Retryable in principle (a fresh
    /// join heals it); must not surface as Internal.
    #[error("segment gap: {0}")]
    SegmentGap(String),
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
            HlsError::LiveStalled(m) => ApiError::Network(format!("live stream stalled: {m}")),
            HlsError::SegmentGap(m) => ApiError::Network(format!("segment gap: {m}")),
            HlsError::Decrypt(m) => ApiError::Internal(format!("hls decrypt: {m}")),
        }
    }
}
