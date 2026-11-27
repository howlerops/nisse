/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The engine config file is missing a field, mistypes one, names an unknown one, or asks
    /// for something this build refuses (an environment other than paper).
    #[error("invalid engine config: {0}")]
    InvalidConfig(String),
    /// The engine's config requires a signed deployment and does not have one: the manifest
    /// is missing, does not verify, or is for a different deployment or stage.
    #[error("unsigned deployment: {0}")]
    Unsigned(String),
    /// The kill switch could not be enforced, or could not have been if it were engaged: no
    /// broker client, or a cancel was refused.
    #[error("kill switch not enforced: {0}")]
    KillNotEnforced(String),
    /// Another engine process holds the database lock. Nothing was done.
    #[error("another engine process is running")]
    Busy,
    /// The market data or calendar could not be fetched, or is not what a step needs (a
    /// symbol with no bar for the session, for example).
    #[error("market data: {0}")]
    Market(String),
    #[error(transparent)]
    Core(#[from] nisse_core::CoreError),
    #[error(transparent)]
    Engine(#[from] nisse_engine::EngineError),
    #[error(transparent)]
    Risk(#[from] nisse_risk::RiskError),
    #[error(transparent)]
    Store(#[from] nisse_store::StoreError),
    #[error(transparent)]
    Bars(#[from] nisse_bars::BarsError),
    #[error(transparent)]
    Adapter(#[from] nisse_alpaca_adapter::AdapterError),
}
