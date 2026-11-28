//! Helpers shared by the app's integration tests: a scratch path, bar and calendar builders, a
//! fake market, the engine config, the simulated broker and a scratch database. Each test
//! binary uses part of this module, so unused items are allowed here.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "shared test helpers: each test binary uses a subset, and tests may unwrap"
)]

use std::{
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicUsize, Ordering},
};

use chrono::{Datelike, NaiveDate, Weekday};
use nisse_alpaca_adapter::{Feed, VENUE};
use nisse_app::{
    AppError,
    config::{EngineConfig, PAPER, SignatureRule},
    market::Market,
};
use nisse_core::nautilus::{
    AggregationSource, Bar, BarAggregation, BarSpecification, BarType, InstrumentId, Price,
    PriceType, Quantity, Symbol, UnixNanos, Venue,
};
use nisse_core::nautilus::{ClientOrderId, VenueOrderId};
use nisse_core::{
    AccountSnapshot, Broker, BrokerOrder, BrokerPosition, MarketCalendar, OpenOrders, OrderIntent,
    ReplaceRequest, Session, SubmitError,
};
use nisse_engine::{CostModel, SimBroker, SimError};
use nisse_store::{Store, StoreWriter, WriterConfig, testing::ScratchDb};
use rust_decimal::Decimal;

/// The universe the restore tests trade.
pub const UNIVERSE: [&str; 3] = ["AAPL", "MSFT", "NVDA"];
/// The benchmark the restore tests fetch.
pub const BENCHMARK: &str = "SPY";
const SECOND: u64 = 1_000_000_000;

/// Removes its directory when dropped, including on a failed assertion.
pub struct Scratch(PathBuf);

impl Scratch {
    /// A path under the system temp dir, unique to this process and call. Not created.
    pub fn new() -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("nisse-app-test-{}-{n}", process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    /// The same path, created as an empty directory.
    pub fn new_dir() -> Self {
        let scratch = Self::new();
        std::fs::create_dir_all(&scratch.0).unwrap();
        scratch
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn symbol(text: &str) -> Symbol {
    Symbol::new_checked(text).unwrap()
}

/// `hour:minute` UTC on `date`, in nanoseconds since the epoch.
pub fn utc_at(date: NaiveDate, hour: u32, minute: u32) -> UnixNanos {
    let secs = date
        .and_hms_opt(hour, minute, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    UnixNanos::from(u64::try_from(secs).unwrap() * SECOND)
}

/// 21:00 UTC on `date`: the close of a session on that date.
pub fn close_at(date: NaiveDate) -> UnixNanos {
    utc_at(date, 21, 0)
}

/// `ts` moved forward by `minutes`.
pub fn plus_minutes(ts: UnixNanos, minutes: u64) -> UnixNanos {
    UnixNanos::from(ts.as_u64() + minutes * 60 * SECOND)
}

/// `2026-01-day`.
pub fn jan(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, day).unwrap()
}

/// A daily bar for `symbol_text` stamped at `ts` (event and init time), with fixed prices.
pub fn bar(symbol_text: &str, ts: UnixNanos) -> Bar {
    bar_at(symbol_text, ts, 1_050)
}

/// A daily bar for `symbol_text` stamped at `ts`, with every price `close_cents` (open and
/// close), 50 cents above it (high) and below it (low).
pub fn bar_at(symbol_text: &str, ts: UnixNanos, close_cents: u64) -> Bar {
    let instrument_id = InstrumentId::new(symbol(symbol_text), Venue::new(VENUE));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let price = |cents: u64| Price::from(cents_text(cents).as_str());
    Bar::new(
        bar_type,
        price(close_cents),
        price(close_cents + 50),
        price(close_cents - 50),
        price(close_cents),
        Quantity::from("1000"),
        ts,
        ts,
    )
}

/// `cents` as a decimal price text with two places.
fn cents_text(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

/// The engine config the restore tests run with: the paper strategy, the three-symbol
/// universe and the benchmark.
pub fn config(bars_dir: PathBuf, limits_path: PathBuf) -> EngineConfig {
    let paper: serde_json::Value =
        serde_json::from_str(include_str!("../../../../config/engine/paper.json")).unwrap();
    EngineConfig {
        strategy: paper["strategy"].clone(),
        symbols: UNIVERSE.iter().map(|text| symbol(text)).collect(),
        benchmark: symbol(BENCHMARK),
        feed: Feed::Iex,
        history_start: NaiveDate::from_ymd_opt(2015, 1, 1).unwrap(),
        bars_dir,
        limits_path,
        seed: 0,
        source: "engine".to_owned(),
        signature: SignatureRule {
            manifest: PathBuf::from("research/signed/trend-v1.manifest.json"),
            required: false,
        },
        environment: PAPER,
    }
}

/// The repository's limits file.
pub fn repo_limits() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/limits/limits.json"
    ))
}

/// A simulated broker with no costs and `100_000` of cash.
pub fn broker() -> SimBroker {
    SimBroker::new(
        Decimal::from(100_000),
        CostModel::new(Decimal::ZERO, Decimal::ZERO).unwrap(),
    )
}

/// A database with a store on one connection and a record writer on another.
pub async fn setup() -> (ScratchDb, Store, StoreWriter) {
    let db = ScratchDb::create().await;
    let store = Store::new(db.client().await.unwrap());
    let (sink, _handle) = StoreWriter::spawn(db.client().await.unwrap(), WriterConfig::default());
    (db, store, sink)
}

/// One session for every weekday from 2025-11-03 to 2026-02-13: open 14:30 UTC, close 21:00 UTC.
pub fn weekday_calendar() -> MarketCalendar {
    let first = NaiveDate::from_ymd_opt(2025, 11, 3).unwrap();
    let last = NaiveDate::from_ymd_opt(2026, 2, 13).unwrap();
    let mut sessions = Vec::new();
    let mut day = first;
    while day <= last {
        if day.weekday() != Weekday::Sat && day.weekday() != Weekday::Sun {
            sessions.push(Session {
                date: day,
                open: utc_at(day, 14, 30),
                close: close_at(day),
            });
        }
        day = day.succ_opt().unwrap();
    }
    MarketCalendar::new(sessions).unwrap()
}

/// The price of `symbol_index` on the `day`-th session of a calendar, in cents: a rising trend
/// with a deterministic wobble of up to 240 cents, so the volatility is never zero.
fn price_cents(symbol_index: usize, day: usize) -> u64 {
    let s = u64::try_from(symbol_index).unwrap();
    let d = u64::try_from(day).unwrap();
    let wobble = (d * 7 + s * 3) % 5;
    10_000 + s * 2_500 + d * 40 + wobble * 60
}

/// One bar per session and symbol, stamped at the session's close, with a deterministic price
/// path (see `price_cents`). `symbols` is the order of the symbol index in the price path.
pub fn session_bars(calendar: &MarketCalendar, symbols: &[&str]) -> Vec<Bar> {
    let mut bars = Vec::with_capacity(calendar.sessions().len() * symbols.len());
    for (day, session) in calendar.sessions().iter().enumerate() {
        for (index, text) in symbols.iter().enumerate() {
            bars.push(bar_at(text, session.close, price_cents(index, day)));
        }
    }
    bars
}

/// A market that serves a fixed calendar and fixed bars, and counts its bar calls.
pub struct FakeMarket {
    calendar: MarketCalendar,
    bars: Vec<Bar>,
    calls: AtomicUsize,
}

impl FakeMarket {
    pub fn new(calendar: MarketCalendar, bars: Vec<Bar>) -> Self {
        Self {
            calendar,
            bars,
            calls: AtomicUsize::new(0),
        }
    }

    /// How many times `daily_bars` has been called.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Market for FakeMarket {
    async fn calendar(
        &self,
        _start: NaiveDate,
        _end: NaiveDate,
    ) -> Result<MarketCalendar, AppError> {
        Ok(self.calendar.clone())
    }

    async fn daily_bars(
        &self,
        symbols: &[Symbol],
        start: NaiveDate,
        end: NaiveDate,
        _calendar: &MarketCalendar,
        as_of: UnixNanos,
    ) -> Result<Vec<Bar>, AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // A bar belongs to the session whose close stamps it.
        let date_of = |ts: UnixNanos| {
            self.calendar
                .sessions()
                .iter()
                .find(|session| session.close == ts)
                .map(|session| session.date)
        };
        let mut out: Vec<Bar> = self
            .bars
            .iter()
            .filter(|bar| {
                let symbol = bar.bar_type.instrument_id().symbol;
                symbols.contains(&symbol)
                    && bar.ts_event <= as_of
                    && date_of(bar.ts_event).is_some_and(|date| date >= start && date <= end)
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.bar_type
                .instrument_id()
                .symbol
                .as_str()
                .cmp(b.bar_type.instrument_id().symbol.as_str())
                .then(a.ts_event.cmp(&b.ts_event))
        });
        Ok(out)
    }
}

/// The paper config, with the strategy's windows shortened so that the ~40 sessions of history
/// in the fake market are enough to decide: `lookback_days` 20 (above `skip_days` 5),
/// `vol_window` 10. The paper universe, benchmark and limits are kept.
pub fn session_config(bars_dir: PathBuf) -> EngineConfig {
    let paper = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/engine/paper.json"
    ));
    let mut config = EngineConfig::load(paper).unwrap();
    config.strategy["lookback_days"] = serde_json::json!(20);
    config.strategy["skip_days"] = serde_json::json!(5);
    config.strategy["vol_window"] = serde_json::json!(10);
    config.bars_dir = bars_dir;
    config.limits_path = repo_limits();
    config
}

/// `SELECT count(*)` of `table`.
pub async fn count(store: &Store, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    store
        .client()
        .query_one(sql.as_str(), &[])
        .await
        .unwrap()
        .get(0)
}

/// The simulated broker, except that cancelling every open order fails.
pub struct CancelFails<'a>(pub &'a SimBroker);

impl Broker for CancelFails<'_> {
    type Error = SimError;

    async fn submit(&self, intent: &OrderIntent) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.0.submit(intent).await
    }

    async fn cancel(&self, venue_order_id: VenueOrderId) -> Result<(), SimError> {
        self.0.cancel(venue_order_id).await
    }

    async fn cancel_all(&self) -> Result<(), SimError> {
        Err(SimError("the broker refused the cancel".to_owned()))
    }

    async fn replace(
        &self,
        venue_order_id: VenueOrderId,
        request: &ReplaceRequest,
    ) -> Result<BrokerOrder, SubmitError<SimError>> {
        self.0.replace(venue_order_id, request).await
    }

    async fn order_by_client_id(
        &self,
        client_order_id: ClientOrderId,
    ) -> Result<Option<BrokerOrder>, SimError> {
        self.0.order_by_client_id(client_order_id).await
    }

    async fn open_orders(&self) -> Result<OpenOrders, SimError> {
        self.0.open_orders().await
    }

    async fn positions(&self) -> Result<Vec<BrokerPosition>, SimError> {
        self.0.positions().await
    }

    async fn account(&self) -> Result<AccountSnapshot, SimError> {
        self.0.account().await
    }
}

/// The repository limits with `flatten_on_kill` on, written into `dir`.
pub fn flattening_limits(dir: &Path) -> PathBuf {
    let mut limits: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(repo_limits()).unwrap()).unwrap();
    limits["flatten_on_kill"] = serde_json::json!(true);
    let path = dir.join("limits.json");
    std::fs::write(&path, limits.to_string()).unwrap();
    path
}
