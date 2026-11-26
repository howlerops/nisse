/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum RiskError {
    /// A shared domain type failed to parse or validate.
    #[error(transparent)]
    Core(#[from] nisse_core::CoreError),
    /// The limits file is missing a field, mistypes one, names an unknown one, or sets a
    /// value outside the range the code accepts.
    #[error("invalid limits: {0}")]
    InvalidLimits(String),
    /// The limits file could not be read.
    #[error("cannot read the limits file {path}: {source}")]
    LimitsFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    /// A stored risk event could not be understood. The risk state is unknown: do not trade.
    #[error("invalid risk event: {0}")]
    InvalidEvent(String),
}
