//! The off-path writer: a bounded queue drained by one task into batched transactions.

use nisse_core::{RecordSink, StoreRecord};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_postgres::Client;

use crate::StoreError;

/// Queue and batch sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterConfig {
    /// Records that can wait in the queue before `emit` starts waiting (back-pressure).
    pub capacity: usize,
    /// Most records written in one transaction.
    pub max_batch: usize,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            capacity: 1024,
            max_batch: 256,
        }
    }
}

/// What travels down the queue.
#[allow(dead_code, reason = "stub: Phase 2 task S4 uses it")]
pub(crate) enum Message {
    Record(StoreRecord),
    /// Answered once everything queued before it has been committed.
    Flush(tokio::sync::oneshot::Sender<()>),
}

/// A cheap, cloneable handle to the writer task.
#[derive(Debug, Clone)]
pub struct StoreWriter {
    #[allow(dead_code, reason = "stub: Phase 2 task S4 uses it")]
    tx: mpsc::Sender<Message>,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Record(record) => f.debug_tuple("Record").field(record).finish(),
            Self::Flush(_) => f.write_str("Flush"),
        }
    }
}

#[allow(unused_variables, reason = "stub: Phase 2 task S4")]
impl StoreWriter {
    /// Starts the writer task on its own connection.
    ///
    /// The task waits for a message, then takes whatever else is already queued (up to
    /// `max_batch` records) and writes the batch in **one transaction**: snapshots, positions
    /// and risk checks with one multi-row insert each, order updates one by one (the order
    /// row, then its fill if any; a fill whose execution id is already stored is skipped).
    /// Order within each table follows arrival order.
    ///
    /// It stops with `Ok(())` when every handle has been dropped and the queue is empty, and
    /// with `Err` on the first database error. After an error nothing more is written and
    /// every `emit` and `flush` returns [`StoreError::WriterStopped`]: the engine must treat
    /// that as a reason to stop trading, not retry.
    #[must_use]
    pub fn spawn(
        client: Client,
        config: WriterConfig,
    ) -> (Self, JoinHandle<Result<(), StoreError>>) {
        todo!("Phase 2 task S4")
    }

    /// Resolves once every record emitted before this call has been committed.
    pub async fn flush(&self) -> Result<(), StoreError> {
        todo!("Phase 2 task S4")
    }
}

#[allow(unused_variables, reason = "stub: Phase 2 task S4")]
impl RecordSink for StoreWriter {
    type Error = StoreError;

    /// Queues a record, waiting if the queue is full. Never drops a record.
    async fn emit(&self, record: StoreRecord) -> Result<(), StoreError> {
        todo!("Phase 2 task S4")
    }
}
