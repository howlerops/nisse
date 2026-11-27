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
    /// The gates file is missing a field, names an unknown one, or sets a threshold looser
    /// than the plan allows.
    #[error("invalid gates: {0}")]
    InvalidGates(String),
    /// A research config, its note or a proposal's set of changes breaks a rule.
    #[error("invalid proposal: {0}")]
    InvalidProposal(String),
    /// The risk limits file could not be read or is not acceptable.
    #[error(transparent)]
    Risk(#[from] nisse_risk::RiskError),
    /// Reading the bar files failed.
    #[error(transparent)]
    Bars(#[from] nisse_bars::BarsError),
    /// An ordinary backtest's range reaches into the holdout period, which only a holdout run
    /// may use.
    #[error("the run reaches into the holdout period: {0}")]
    HoldoutTouched(String),
    /// The holdout has been looked at as many times as the gates allow.
    #[error("the holdout is retired: {looks} looks of at most {max}")]
    HoldoutRetired { looks: i64, max: u32 },
}
