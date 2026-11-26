//! The off-path writer: a bounded queue drained by one task into batched transactions.

use nisse_core::{
    Fill, MarketSnapshot, OrderEvent, PositionEvent, RecordSink, RiskCheck, StoreRecord,
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use tokio_postgres::Client;

use crate::StoreError;
use crate::tables::{fill, market_snapshot, order_event, position_event, risk_check};

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
pub(crate) enum Message {
    Record(StoreRecord),
    /// Answered once everything queued before it has been committed.
    Flush(oneshot::Sender<()>),
}

/// A cheap, cloneable handle to the writer task.
#[derive(Debug, Clone)]
pub struct StoreWriter {
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

/// The records of one batch, sorted by table. The buffers are reused across batches:
/// `clear` keeps their capacity.
struct Buffers {
    snapshots: Vec<MarketSnapshot>,
    positions: Vec<PositionEvent>,
    risks: Vec<RiskCheck>,
    orders: Vec<(OrderEvent, Option<Fill>)>,
}

impl Buffers {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            snapshots: Vec::with_capacity(capacity),
            positions: Vec::with_capacity(capacity),
            risks: Vec::with_capacity(capacity),
            orders: Vec::with_capacity(capacity),
        }
    }

    fn push(&mut self, record: StoreRecord) {
        match record {
            StoreRecord::Snapshot(row) => self.snapshots.push(row),
            StoreRecord::Position(row) => self.positions.push(row),
            StoreRecord::Risk(row) => self.risks.push(row),
            StoreRecord::RiskEvent(_) => todo!("Phase 4 task S2. LEAD: must be gone before the PR"),
            StoreRecord::Order { event, fill } => self.orders.push((event, fill)),
        }
    }

    fn clear(&mut self) {
        self.snapshots.clear();
        self.positions.clear();
        self.risks.clear();
        self.orders.clear();
    }
}

/// Writes one batch in a single transaction. Nothing is committed unless every row is.
async fn write(client: &mut Client, buffers: &Buffers) -> Result<(), StoreError> {
    let tx = client.transaction().await?;
    market_snapshot::insert_many(&tx, &buffers.snapshots).await?;
    position_event::insert_many(&tx, &buffers.positions).await?;
    risk_check::insert_many(&tx, &buffers.risks).await?;
    for (event, fill_row) in &buffers.orders {
        order_event::insert(&tx, event).await?;
        if let Some(fill_row) = fill_row {
            // `false` means this execution id is already stored: recorded once, carry on.
            fill::insert(&tx, fill_row).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// The writer task. Returns `Ok` when every handle is gone and the queue is drained, and the
/// first database error otherwise (dropping the receiver and pending acks on the way out).
async fn run(
    mut client: Client,
    mut rx: mpsc::Receiver<Message>,
    config: WriterConfig,
) -> Result<(), StoreError> {
    let max_batch = config.max_batch.max(1);
    let mut buffers = Buffers::with_capacity(max_batch);
    let mut acks: Vec<oneshot::Sender<()>> = Vec::new();

    while let Some(first) = rx.recv().await {
        let mut message = first;
        let mut records = 0;
        loop {
            match message {
                Message::Record(record) => {
                    buffers.push(record);
                    records += 1;
                }
                Message::Flush(ack) => {
                    acks.push(ack);
                    break;
                }
            }
            if records >= max_batch {
                break;
            }
            match rx.try_recv() {
                Ok(next) => message = next,
                Err(_) => break,
            }
        }

        if records > 0 {
            if let Err(error) = write(&mut client, &buffers).await {
                tracing::error!(%error, "store writer stopped");
                return Err(error);
            }
            buffers.clear();
        }
        for ack in acks.drain(..) {
            // The caller may have stopped waiting; that is not an error for the writer.
            let _ = ack.send(());
        }
    }
    Ok(())
}

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
        let (tx, rx) = mpsc::channel(config.capacity.max(1));
        let handle = tokio::spawn(run(client, rx, config));
        (Self { tx }, handle)
    }

    /// Resolves once every record emitted before this call has been committed.
    pub async fn flush(&self) -> Result<(), StoreError> {
        let (ack, committed) = oneshot::channel();
        self.tx
            .send(Message::Flush(ack))
            .await
            .map_err(|_| StoreError::WriterStopped)?;
        committed.await.map_err(|_| StoreError::WriterStopped)
    }
}

impl RecordSink for StoreWriter {
    type Error = StoreError;

    /// Queues a record, waiting if the queue is full. Never drops a record.
    async fn emit(&self, record: StoreRecord) -> Result<(), StoreError> {
        self.tx
            .send(Message::Record(record))
            .await
            .map_err(|_| StoreError::WriterStopped)
    }
}
