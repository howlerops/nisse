/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// A shared domain type failed to parse or validate.
    #[error(transparent)]
    Core(#[from] nisse_core::CoreError),
    /// Reading or writing the store failed.
    #[error(transparent)]
    Store(#[from] nisse_store::StoreError),
    /// A strategy version's parameters do not describe a strategy this build knows.
    #[error("unknown strategy: {0}")]
    UnknownStrategy(String),
}
