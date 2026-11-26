use nisse_core::CoreError;

/// Errors returned by the Alpaca adapter.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// A shared domain type failed to parse or validate.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The request never got an HTTP response (connect failure, timeout, dropped connection).
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// Alpaca answered with a non-success status. `code` and `message` come from its JSON
    /// error body when there is one.
    #[error("alpaca returned HTTP {status}: {message}")]
    Status {
        status: u16,
        code: Option<i64>,
        message: String,
    },
    /// A response or stream message did not have the expected shape or values.
    #[error("cannot decode alpaca payload: {0}")]
    Decode(String),
    /// The request asks for something this adapter deliberately does not do.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// A required environment variable is not set (the value is never included).
    #[error("environment variable {0} is not set")]
    MissingEnv(&'static str),
    /// The order stream failed (handshake, authentication or protocol).
    #[error("order stream error: {0}")]
    Stream(String),
}

impl AdapterError {
    /// The HTTP status, when Alpaca answered at all.
    #[must_use]
    pub const fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Alpaca definitely refused the request, so nothing was created: a 4xx other than 408
    /// (timeout) and 429 (rate limit). Anything else, including transport failures and 5xx,
    /// leaves the outcome unknown.
    #[must_use]
    pub const fn is_definite_rejection(&self) -> bool {
        matches!(self, Self::Status { status, .. } if *status >= 400 && *status < 500 && *status != 408 && *status != 429)
    }

    /// Worth retrying an idempotent read: transport failure, 408, 429 or 5xx.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Status { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            _ => false,
        }
    }
}
