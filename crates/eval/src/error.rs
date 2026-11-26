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
    /// A backtest job file is missing a field, mistypes one or names an unknown one.
    #[error("invalid backtest job: {0}")]
    InvalidJob(String),
    /// The build has uncommitted changes, so its decisions could not be replayed.
    #[error(
        "this build has uncommitted changes; its decisions could not be replayed \
         (pass --allow-dirty to run anyway)"
    )]
    DirtyBuild,
    /// The engine failed while building or running a strategy.
    #[error(transparent)]
    Engine(#[from] nisse_engine::EngineError),
    /// Reading the bar files failed.
    #[error(transparent)]
    Bars(#[from] nisse_bars::BarsError),
}
