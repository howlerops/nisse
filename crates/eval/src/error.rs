/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// A shared domain type failed to parse or validate.
    #[error(transparent)]
    Core(#[from] nisse_core::CoreError),
}
