/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A shared domain type failed to parse or validate.
    #[error(transparent)]
    Core(#[from] nisse_core::CoreError),
    /// Postgres refused the statement or the connection failed.
    #[error("database error: {0}")]
    Db(#[from] tokio_postgres::Error),
    /// A stored value does not fit the type it is read into (an unknown enum string, a
    /// timestamp out of range, a quantity Nautilus cannot hold).
    #[error("bad stored data: {0}")]
    Data(String),
    /// The writer task has stopped (after a database error), so nothing more can be written.
    /// The engine must stop trading: decisions and fills can no longer be recorded.
    #[error("the store writer has stopped")]
    WriterStopped,
}
