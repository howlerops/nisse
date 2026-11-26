/// Errors returned by this crate.
#[derive(Debug, thiserror::Error)]
pub enum BarsError {
    /// A shared domain type failed to parse or validate.
    #[error(transparent)]
    Core(#[from] nisse_core::CoreError),
    /// DuckDB refused a statement or could not read or write a file.
    #[error("bar storage error: {0}")]
    DuckDb(#[from] duckdb::Error),
    /// The file system refused an operation (creating the directory, moving the file).
    #[error("bar file error: {0}")]
    Io(#[from] std::io::Error),
    /// A stored or supplied value cannot be used (an unknown symbol shape, a price Nautilus
    /// cannot hold, a path that is not valid UTF-8).
    #[error("bad bar data: {0}")]
    Data(String),
}
