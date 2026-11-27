//! Acceptance tests for the execution and exit scores (`execution_job`, task JB1b) against a
//! real Postgres (`TEST_DATABASE_URL`), with daily bars written to and read back from Parquet.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::NaiveDate;
use nisse_bars::{BarReader, write_bars};
use nisse_core::nautilus::{
    AggregationSource, Bar, BarAggregation, BarSpecification, BarType, ClientOrderId, InstrumentId,
    OrderSide, OrderType, Price, PriceType, Quantity, Symbol, TimeInForce, UnixNanos, Venue,
    VenueOrderId,
};
use nisse_core::{DecisionId, OrderEvent, OrderIntent, OrderState, RiskCheck, StrategyVersionId};
use nisse_eval::execution_job::{self, EVAL_VERSION, ExecutionReport, ExitReport, SETTLE_NANOS};
use nisse_store::tables::{order_event, risk_check};
use nisse_store::testing::ScratchDb;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio_postgres::Client;

const NOW: u64 = 1_767_600_000_123_456_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);
static FILL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp directory, unique to one test and removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nisse-execution-job-{}-{n}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn now() -> UnixNanos {
    UnixNanos::from(NOW)
}

/// 2026-01-`day` at `hour`:00 UTC, as nanoseconds since the epoch.
fn at(day: u32, hour: u32) -> UnixNanos {
    let secs = NaiveDate::from_ymd_opt(2026, 1, day)
        .unwrap()
        .and_hms_opt(hour, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    UnixNanos::from(u64::try_from(secs).unwrap() * NANOS_PER_SECOND)
}

/// The session close of 2026-01-`day` (21:00 UTC), where the daily bars sit.
fn close_of(day: u32) -> UnixNanos {
    at(day, 21)
}

// ---- bars -----------------------------------------------------------------------------------

/// A daily bar whose four prices are all `close`.
fn bar(symbol: &str, ts: UnixNanos, close: &str) -> Bar {
    let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let px = Price::from(close);
    Bar::new(bar_type, px, px, px, px, Quantity::from("1000"), ts, ts)
}

/// A fresh directory holding `bars` (`(symbol, ts, close)`) in one Parquet file, and a reader
/// on it.
fn bars_dir(name: &str, bars: &[(&str, UnixNanos, &str)]) -> (TempDir, BarReader) {
    let dir = TempDir::new(name);
    let rows: Vec<Bar> = bars.iter().map(|(s, ts, c)| bar(s, *ts, c)).collect();
    write_bars(&dir.path().join("bars.parquet"), &rows).unwrap();
    let reader = BarReader::open(dir.path()).unwrap();
    (dir, reader)
}

// ---- store rows -----------------------------------------------------------------------------

/// Inserts a strategy version (one per scratch database).
async fn version(client: &Client) -> StrategyVersionId {
    let id: i64 = client
        .query_one(
            "INSERT INTO strategy_version (git_sha, config_hash, params, event_ts) \
             VALUES ($1, $2, '{}', '2026-01-02T00:00:00Z') RETURNING id",
            &[&"a".repeat(40), &vec![7_u8; 32]],
        )
        .await
        .unwrap()
        .get(0);
    StrategyVersionId(id)
}

/// Inserts one decision on `symbol` with the given features.
async fn decision(
    client: &Client,
    version: StrategyVersionId,
    symbol: &str,
    features: Value,
) -> DecisionId {
    let id: i64 = client
        .query_one(
            "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, \
                                   feature_hash, data_snapshot_id, rng_seed, reason_code, event_ts) \
             VALUES ($1, $2, 'enter', 1.5, $3, decode(repeat('ab', 32), 'hex'), 'snap-1', \
                     42, 'trend_up', '2026-01-05T21:00:00Z') RETURNING id",
            &[&version.0, &symbol, &features],
        )
        .await
        .unwrap()
        .get(0);
    DecisionId(id)
}

/// Writes the intent row of an order for `decision`, and returns the order.
async fn place(
    client: &Client,
    decision: DecisionId,
    symbol: &str,
    coid: &str,
    side: OrderSide,
    qty: &str,
    ts: UnixNanos,
) -> OrderIntent {
    let order = OrderIntent {
        decision_id: decision,
        client_order_id: ClientOrderId::new_checked(coid).unwrap(),
        symbol: Symbol::new_checked(symbol).unwrap(),
        side,
        order_type: OrderType::Market,
        qty: Quantity::from(qty),
        limit_px: None,
        time_in_force: TimeInForce::Day,
        ts,
    };
    order_event::insert(client, &OrderEvent::intent(&order))
        .await
        .unwrap();
    order
}

/// Appends a row moving `order` to `state` with `cum` filled so far.
async fn set_state(
    client: &Client,
    order: &OrderIntent,
    state: OrderState,
    cum: &str,
    ts: UnixNanos,
) {
    let event = OrderEvent::of(
        order,
        state,
        Some(VenueOrderId::from("V-1")),
        Quantity::from(cum),
        ts,
    );
    order_event::insert(client, &event).await.unwrap();
}

/// One fill of order `coid`, with its fee. `fill` rows are written here directly: the store's
/// `fill::insert` always records a zero fee, and the fee is what these tests exercise.
async fn fill(client: &Client, coid: &str, px: &str, qty: &str, fee: &str, ts: UnixNanos) {
    let trade = format!("T-{coid}-{}", FILL_COUNTER.fetch_add(1, Ordering::Relaxed));
    let secs = i64::try_from(ts.as_u64() / NANOS_PER_SECOND).unwrap();
    client
        .execute(
            "INSERT INTO fill (client_order_id, broker_fill_id, px, qty, fee, event_ts) \
             VALUES ($1, $2, $3::numeric, $4::numeric, $5::numeric, to_timestamp($6::bigint::float8))",
            &[&coid, &trade, &dec(px), &dec(qty), &dec(fee), &secs],
        )
        .await
        .unwrap();
}

/// An order for `decision` that filled `qty` in one fill at `px` with no fee, at `ts`.
#[allow(
    clippy::too_many_arguments,
    reason = "a test helper that names every field of one filled order"
)]
async fn filled(
    client: &Client,
    decision: DecisionId,
    symbol: &str,
    coid: &str,
    side: OrderSide,
    qty: &str,
    px: &str,
    ts: UnixNanos,
) {
    let order = place(client, decision, symbol, coid, side, qty, ts).await;
    set_state(client, &order, OrderState::Filled, qty, ts).await;
    fill(client, coid, px, qty, "0", ts).await;
}

/// The `risk_per_trade` figure the gate recorded for `decision`.
async fn risk(client: &Client, decision: DecisionId, actual: &str) {
    let row = RiskCheck {
        decision_id: decision,
        rule: "risk_per_trade".to_owned(),
        limit_value: dec("1000"),
        actual_value: dec(actual),
        passed: true,
        ts: at(1, 21),
    };
    risk_check::insert_many(client, &[row]).await.unwrap();
}

/// Every `eval_score` row, in insertion order: `(subject id, metric, value)`.
async fn scores(client: &Client) -> Vec<(i64, String, f64)> {
    client
        .query(
            "SELECT subject_id, metric, value FROM eval_score ORDER BY id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| {
            let (id, metric, value): (i64, String, f64) = (row.get(0), row.get(1), row.get(2));
            (id, metric, value)
        })
        .collect()
}

/// The value of `metric` written for `decision`, if any.
fn value(rows: &[(i64, String, f64)], decision: DecisionId, metric: &str) -> Option<f64> {
    rows.iter()
        .find(|(id, m, _)| *id == decision.0 && m == metric)
        .map(|(_, _, v)| *v)
}

/// How many rows were written for `decision`.
fn rows_for(rows: &[(i64, String, f64)], decision: DecisionId) -> usize {
    rows.iter().filter(|(id, _, _)| *id == decision.0).count()
}

/// Asserts an optional score is present and within 1e-9 of `expected`.
fn assert_close(actual: Option<f64>, expected: f64) {
    assert!(
        actual.is_some_and(|a| (a - expected).abs() < 1e-9),
        "expected Some({expected}), got {actual:?}"
    );
}

// ---- score_execution ------------------------------------------------------------------------

#[tokio::test]
async fn filled_buy_scores_shortfall_fees_and_no_miss() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    let order = place(&client, d, "SPY", "O-1", OrderSide::Buy, "100", at(1, 21)).await;
    set_state(&client, &order, OrderState::Filled, "100", at(1, 21)).await;
    fill(&client, "O-1", "100.10", "60", "0.25", at(1, 21)).await;
    fill(&client, "O-1", "100.20", "40", "0.25", at(1, 21)).await;

    let report = execution_job::score_execution(&client, now())
        .await
        .unwrap();
    assert_eq!(
        report,
        ExecutionReport {
            decisions_scored: 1,
            waiting: 0,
            no_arrival: 0
        }
    );
    let rows = scores(&client).await;
    assert_close(value(&rows, d, "shortfall_bps"), 14.0);
    assert_close(value(&rows, d, "fees_bps"), 0.5);
    assert_close(value(&rows, d, "missed_fill_ratio"), 0.0);
    assert_eq!(rows_for(&rows, d), 3);
    let tagged: i64 = client
        .query_one(
            "SELECT count(*) FROM eval_score \
             WHERE subject_type = 'decision' AND eval_version = $1",
            &[&EVAL_VERSION],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(tagged, 3);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn open_order_waits_then_a_partial_cancel_scores_the_miss() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    let order = place(&client, d, "SPY", "O-1", OrderSide::Buy, "100", at(1, 21)).await;
    set_state(&client, &order, OrderState::Accepted, "0", at(1, 21)).await;

    let waiting = execution_job::score_execution(&client, now())
        .await
        .unwrap();
    assert_eq!(
        waiting,
        ExecutionReport {
            decisions_scored: 0,
            waiting: 1,
            no_arrival: 0
        }
    );
    assert!(scores(&client).await.is_empty());

    set_state(&client, &order, OrderState::Canceled, "25", at(1, 22)).await;
    fill(&client, "O-1", "100", "25", "0", at(1, 22)).await;
    let done = execution_job::score_execution(&client, now())
        .await
        .unwrap();
    assert_eq!(done.decisions_scored, 1);
    assert_eq!(done.waiting, 0);
    let rows = scores(&client).await;
    assert_close(value(&rows, d, "missed_fill_ratio"), 0.75);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn decision_with_only_an_intent_is_ignored() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    place(&client, d, "SPY", "O-1", OrderSide::Buy, "100", at(1, 21)).await;

    let report = execution_job::score_execution(&client, now())
        .await
        .unwrap();
    assert_eq!(report, ExecutionReport::default());
    assert!(scores(&client).await.is_empty());
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn no_close_feature_counts_no_arrival_and_writes_only_the_miss() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({})).await;
    filled(
        &client,
        d,
        "SPY",
        "O-1",
        OrderSide::Buy,
        "100",
        "100",
        at(1, 21),
    )
    .await;

    let report = execution_job::score_execution(&client, now())
        .await
        .unwrap();
    assert_eq!(report.no_arrival, 1);
    assert_eq!(report.decisions_scored, 1);
    let rows = scores(&client).await;
    assert_close(value(&rows, d, "missed_fill_ratio"), 0.0);
    assert_eq!(value(&rows, d, "shortfall_bps"), None);
    assert_eq!(value(&rows, d, "fees_bps"), None);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn second_execution_run_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    filled(
        &client,
        d,
        "SPY",
        "O-1",
        OrderSide::Buy,
        "100",
        "100.10",
        at(1, 21),
    )
    .await;
    execution_job::score_execution(&client, now())
        .await
        .unwrap();
    let before = scores(&client).await;

    let again = execution_job::score_execution(&client, now())
        .await
        .unwrap();
    assert_eq!(again, ExecutionReport::default());
    assert_eq!(scores(&client).await, before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn sell_above_arrival_is_a_negative_shortfall() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    let order = place(&client, d, "SPY", "O-1", OrderSide::Sell, "100", at(1, 21)).await;
    set_state(&client, &order, OrderState::Filled, "100", at(1, 21)).await;
    fill(&client, "O-1", "100.10", "60", "0.3", at(1, 21)).await;
    fill(&client, "O-1", "100.20", "40", "0.2", at(1, 21)).await;

    execution_job::score_execution(&client, now())
        .await
        .unwrap();
    let rows = scores(&client).await;
    assert_close(value(&rows, d, "shortfall_bps"), -14.0);
    assert_close(value(&rows, d, "fees_bps"), 0.5);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn rejected_order_is_a_full_miss_with_no_price_scores() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    let order = place(&client, d, "SPY", "O-1", OrderSide::Buy, "100", at(1, 21)).await;
    set_state(&client, &order, OrderState::Rejected, "0", at(1, 21)).await;

    execution_job::score_execution(&client, now())
        .await
        .unwrap();
    let rows = scores(&client).await;
    assert_close(value(&rows, d, "missed_fill_ratio"), 1.0);
    assert_eq!(value(&rows, d, "shortfall_bps"), None);
    assert_eq!(value(&rows, d, "fees_bps"), None);
    db.drop_db().await.unwrap();
}

// ---- score_exits ----------------------------------------------------------------------------

/// Buys 100 @ 10 on day 1 (decision returned first, risk 50 recorded) and sells 100 @ 12 on
/// day 4 (decision returned second). SPY bars: 10, 13, 9, 12 on days 1 to 4.
async fn spy_round_trip(client: &Client) -> (DecisionId, DecisionId, TempDir, BarReader) {
    let v = version(client).await;
    let buy = decision(client, v, "SPY", json!({})).await;
    let sell = decision(client, v, "SPY", json!({})).await;
    risk(client, buy, "50").await;
    filled(
        client,
        buy,
        "SPY",
        "B-1",
        OrderSide::Buy,
        "100",
        "10",
        close_of(1),
    )
    .await;
    filled(
        client,
        sell,
        "SPY",
        "S-1",
        OrderSide::Sell,
        "100",
        "12",
        close_of(4),
    )
    .await;
    let (dir, reader) = bars_dir(
        "spy",
        &[
            ("SPY", close_of(1), "10"),
            ("SPY", close_of(2), "13"),
            ("SPY", close_of(3), "9"),
            ("SPY", close_of(4), "12"),
        ],
    );
    (buy, sell, dir, reader)
}

#[tokio::test]
async fn round_trip_scores_realized_r_and_excursions() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (buy, _sell, _dir, reader) = spy_round_trip(&client).await;

    let report = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(
        report,
        ExitReport {
            entries_scored: 1,
            open: 0,
            unmatched_sell_qty: Decimal::ZERO
        }
    );
    let rows = scores(&client).await;
    assert_close(value(&rows, buy, "realized_r"), 4.0);
    assert_close(value(&rows, buy, "mfe_r"), 6.0);
    assert_close(value(&rows, buy, "mae_r"), -2.0);
    assert_close(value(&rows, buy, "capture"), 200.0 / 300.0);
    assert_eq!(rows_for(&rows, buy), 4);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn partial_sell_is_open_then_scored_once_over_both_trips() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let buy = decision(&client, v, "SPY", json!({})).await;
    risk(&client, buy, "50").await;
    filled(
        &client,
        buy,
        "SPY",
        "B-1",
        OrderSide::Buy,
        "100",
        "10",
        close_of(1),
    )
    .await;
    filled(
        &client,
        decision(&client, v, "SPY", json!({})).await,
        "SPY",
        "S-1",
        OrderSide::Sell,
        "40",
        "12",
        close_of(2),
    )
    .await;
    let (_dir, reader) = bars_dir(
        "partial",
        &[
            ("SPY", close_of(1), "10"),
            ("SPY", close_of(2), "13"),
            ("SPY", close_of(3), "9"),
        ],
    );

    let first = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(first.open, 1);
    assert_eq!(first.entries_scored, 0);
    assert!(scores(&client).await.is_empty());

    filled(
        &client,
        decision(&client, v, "SPY", json!({})).await,
        "SPY",
        "S-2",
        OrderSide::Sell,
        "60",
        "11",
        close_of(3),
    )
    .await;
    let second = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(second.entries_scored, 1);
    assert_eq!(second.open, 0);
    let rows = scores(&client).await;
    // 40 × (12 − 10) + 60 × (11 − 10) = 140 over a risk of 50.
    assert_close(value(&rows, buy, "realized_r"), 2.8);
    assert_eq!(
        rows.iter()
            .filter(|(id, m, _)| *id == buy.0 && m == "realized_r")
            .count(),
        1
    );
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn entry_without_a_risk_row_is_not_scored() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let buy = decision(&client, v, "SPY", json!({})).await;
    filled(
        &client,
        buy,
        "SPY",
        "B-1",
        OrderSide::Buy,
        "100",
        "10",
        close_of(1),
    )
    .await;
    filled(
        &client,
        decision(&client, v, "SPY", json!({})).await,
        "SPY",
        "S-1",
        OrderSide::Sell,
        "100",
        "12",
        close_of(2),
    )
    .await;
    let (_dir, reader) = bars_dir(
        "norisk",
        &[("SPY", close_of(1), "10"), ("SPY", close_of(2), "12")],
    );

    let report = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(report.entries_scored, 0);
    assert_eq!(report.open, 0);
    assert!(scores(&client).await.is_empty());
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn sell_without_a_buy_is_unmatched_and_does_not_panic() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let sell = decision(&client, v, "SPY", json!({})).await;
    filled(
        &client,
        sell,
        "SPY",
        "S-1",
        OrderSide::Sell,
        "30",
        "10",
        close_of(2),
    )
    .await;
    let (_dir, reader) = bars_dir("unmatched", &[("SPY", close_of(2), "10")]);

    let report = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(report.unmatched_sell_qty, dec("30"));
    assert_eq!(report.entries_scored, 0);
    assert!(scores(&client).await.is_empty());
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn second_exit_run_writes_nothing() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let (_buy, _sell, _dir, reader) = spy_round_trip(&client).await;
    execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    let before = scores(&client).await;

    let again = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(again.entries_scored, 0);
    assert_eq!(again.open, 0);
    assert_eq!(scores(&client).await, before);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn two_symbols_on_the_same_days_do_not_mix() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let aaa_buy = decision(&client, v, "AAA", json!({})).await;
    let bbb_buy = decision(&client, v, "BBB", json!({})).await;
    let aaa_sell = decision(&client, v, "AAA", json!({})).await;
    let bbb_sell = decision(&client, v, "BBB", json!({})).await;
    risk(&client, aaa_buy, "50").await;
    risk(&client, bbb_buy, "50").await;
    filled(
        &client,
        aaa_buy,
        "AAA",
        "A-B",
        OrderSide::Buy,
        "100",
        "10",
        close_of(1),
    )
    .await;
    filled(
        &client,
        bbb_buy,
        "BBB",
        "B-B",
        OrderSide::Buy,
        "100",
        "10",
        close_of(1),
    )
    .await;
    filled(
        &client,
        aaa_sell,
        "AAA",
        "A-S",
        OrderSide::Sell,
        "100",
        "12",
        close_of(3),
    )
    .await;
    filled(
        &client,
        bbb_sell,
        "BBB",
        "B-S",
        OrderSide::Sell,
        "100",
        "11",
        close_of(3),
    )
    .await;
    let (_dir, reader) = bars_dir(
        "two",
        &[
            ("AAA", close_of(1), "10"),
            ("AAA", close_of(2), "20"),
            ("AAA", close_of(3), "12"),
            ("BBB", close_of(1), "10"),
            ("BBB", close_of(2), "9"),
            ("BBB", close_of(3), "11"),
        ],
    );

    let report = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(report.entries_scored, 2);
    let rows = scores(&client).await;
    // AAA: highest 20 (MFE 1000), lowest 10 (MAE 0), realized 200 over 50.
    assert_close(value(&rows, aaa_buy, "mfe_r"), 20.0);
    assert_close(value(&rows, aaa_buy, "mae_r"), 0.0);
    assert_close(value(&rows, aaa_buy, "realized_r"), 4.0);
    // BBB: highest 11 (MFE 100), lowest 9 (MAE −100), realized 100 over 50.
    assert_close(value(&rows, bbb_buy, "mfe_r"), 2.0);
    assert_close(value(&rows, bbb_buy, "mae_r"), -2.0);
    assert_close(value(&rows, bbb_buy, "realized_r"), 2.0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_entry_off_a_bar_time_starts_at_the_next_bar() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let buy = decision(&client, v, "SPY", json!({})).await;
    risk(&client, buy, "50").await;
    // Bought at 22:00 on day 1, between the day-1 bar (close 9) and the day-2 bar (close 10).
    filled(
        &client,
        buy,
        "SPY",
        "B-1",
        OrderSide::Buy,
        "100",
        "10",
        at(1, 22),
    )
    .await;
    filled(
        &client,
        decision(&client, v, "SPY", json!({})).await,
        "SPY",
        "S-1",
        OrderSide::Sell,
        "100",
        "12",
        close_of(3),
    )
    .await;
    let (_dir, reader) = bars_dir(
        "offbar",
        &[
            ("SPY", close_of(1), "9"),
            ("SPY", close_of(2), "10"),
            ("SPY", close_of(3), "12"),
        ],
    );

    execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    let rows = scores(&client).await;
    // The day-1 bar (9) is from before the position existed, so the window is 10 to 12: the
    // lowest close is 10, MAE = 0; the highest is 12, MFE = 200 / 50 = 4.
    assert_close(value(&rows, buy, "mae_r"), 0.0);
    assert_close(value(&rows, buy, "mfe_r"), 4.0);
    db.drop_db().await.unwrap();
}

const DAY_NANOS: u64 = 86_400 * NANOS_PER_SECOND;

/// `t` moved forward by `nanos`.
fn after(t: UnixNanos, nanos: u64) -> UnixNanos {
    UnixNanos::from(t.as_u64() + nanos)
}

/// One 100-share SPY trip on version `v`: bought at `entry_px` at `entry_at` (risk 50 recorded
/// for the entry) and sold at `exit_px` at `exit_at`. Returns the entry decision.
async fn trip(
    client: &Client,
    v: StrategyVersionId,
    entry_at: UnixNanos,
    entry_px: &str,
    exit_px: &str,
    exit_at: UnixNanos,
) -> DecisionId {
    let buy = decision(client, v, "SPY", json!({})).await;
    risk(client, buy, "50").await;
    filled(
        client,
        buy,
        "SPY",
        "B-1",
        OrderSide::Buy,
        "100",
        entry_px,
        entry_at,
    )
    .await;
    filled(
        client,
        decision(client, v, "SPY", json!({})).await,
        "SPY",
        "S-1",
        OrderSide::Sell,
        "100",
        exit_px,
        exit_at,
    )
    .await;
    buy
}

#[tokio::test]
async fn execution_is_scored_only_once_the_orders_have_settled() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    filled(
        &client,
        d,
        "SPY",
        "O-1",
        OrderSide::Buy,
        "100",
        "100",
        at(1, 21),
    )
    .await;
    let last = at(1, 21);

    // One day after the last row: terminal, but not settled.
    let early = execution_job::score_execution(&client, after(last, DAY_NANOS))
        .await
        .unwrap();
    assert_eq!(
        early,
        ExecutionReport {
            decisions_scored: 0,
            waiting: 1,
            no_arrival: 0
        }
    );
    assert!(scores(&client).await.is_empty());

    // One nanosecond short of settling: still waiting.
    let short = execution_job::score_execution(&client, after(last, SETTLE_NANOS - 1))
        .await
        .unwrap();
    assert_eq!(short.waiting, 1);
    assert!(scores(&client).await.is_empty());

    // Exactly SETTLE_NANOS after the last row: scored.
    let settled = execution_job::score_execution(&client, after(last, SETTLE_NANOS))
        .await
        .unwrap();
    assert_eq!(
        settled,
        ExecutionReport {
            decisions_scored: 1,
            waiting: 0,
            no_arrival: 0
        }
    );
    let rows = scores(&client).await;
    assert_close(value(&rows, d, "missed_fill_ratio"), 0.0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_late_fill_restarts_the_settle_clock() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    let order = place(&client, d, "SPY", "O-1", OrderSide::Buy, "100", at(1, 21)).await;
    set_state(&client, &order, OrderState::Accepted, "0", at(1, 21)).await;
    // The order fills two days later: the last change is the fill, not the accepted row.
    set_state(&client, &order, OrderState::Filled, "100", at(3, 21)).await;
    fill(&client, "O-1", "100", "100", "0", at(3, 21)).await;

    // Three days after the accepted row, but one day after the fill: still waiting.
    let early = execution_job::score_execution(&client, after(at(1, 21), 3 * DAY_NANOS))
        .await
        .unwrap();
    assert_eq!(early.waiting, 1);
    assert!(scores(&client).await.is_empty());

    let settled = execution_job::score_execution(&client, after(at(3, 21), SETTLE_NANOS))
        .await
        .unwrap();
    assert_eq!(settled.decisions_scored, 1);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_last_change_after_now_waits() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    filled(
        &client,
        d,
        "SPY",
        "O-1",
        OrderSide::Buy,
        "100",
        "100",
        at(2, 21),
    )
    .await;

    // `now` is before the last change: the difference saturates to zero and nothing settles.
    let report = execution_job::score_execution(&client, at(1, 21))
        .await
        .unwrap();
    assert_eq!(report.waiting, 1);
    assert_eq!(report.decisions_scored, 0);
    assert!(scores(&client).await.is_empty());
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_rejection_overturned_by_a_late_fill_is_scored_once_with_the_fill() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    let d = decision(&client, v, "SPY", json!({"close": "100"})).await;
    let order = place(&client, d, "SPY", "O-1", OrderSide::Buy, "100", at(1, 21)).await;
    set_state(&client, &order, OrderState::Rejected, "0", at(1, 21)).await;

    // A day after the rejection it is still provisional: nothing is written.
    let provisional = execution_job::score_execution(&client, after(at(1, 21), DAY_NANOS))
        .await
        .unwrap();
    assert_eq!(
        provisional,
        ExecutionReport {
            decisions_scored: 0,
            waiting: 1,
            no_arrival: 0
        }
    );
    assert!(scores(&client).await.is_empty());

    // Reconciliation overturns the rejection a day later: a fill and a filled row.
    set_state(&client, &order, OrderState::Filled, "100", at(2, 21)).await;
    fill(&client, "O-1", "100", "100", "0", at(2, 21)).await;

    // Five days after the rejection, the order has settled: scored as filled, not as missed.
    let late = execution_job::score_execution(&client, after(at(1, 21), 5 * DAY_NANOS))
        .await
        .unwrap();
    assert_eq!(
        late,
        ExecutionReport {
            decisions_scored: 1,
            waiting: 0,
            no_arrival: 0
        }
    );
    let rows = scores(&client).await;
    assert_close(value(&rows, d, "missed_fill_ratio"), 0.0);

    // A later run writes nothing: the decision was scored exactly once.
    let again = execution_job::score_execution(&client, after(at(1, 21), 6 * DAY_NANOS))
        .await
        .unwrap();
    assert_eq!(again, ExecutionReport::default());
    let missed = scores(&client)
        .await
        .iter()
        .filter(|(id, m, _)| *id == d.0 && m == "missed_fill_ratio")
        .count();
    assert_eq!(missed, 1);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_backfilled_buy_still_closes_the_sell_stored_before_it() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    // The sell is stored first but happened on day 4; the buy is a backfill stored after it.
    let sell = decision(&client, v, "SPY", json!({})).await;
    filled(
        &client,
        sell,
        "SPY",
        "S-1",
        OrderSide::Sell,
        "100",
        "12",
        close_of(4),
    )
    .await;
    let buy = decision(&client, v, "SPY", json!({})).await;
    risk(&client, buy, "50").await;
    filled(
        &client,
        buy,
        "SPY",
        "B-1",
        OrderSide::Buy,
        "100",
        "10",
        close_of(1),
    )
    .await;
    let (_dir, reader) = bars_dir(
        "backfill",
        &[
            ("SPY", close_of(1), "10"),
            ("SPY", close_of(2), "13"),
            ("SPY", close_of(3), "9"),
            ("SPY", close_of(4), "12"),
        ],
    );

    let report = execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    assert_eq!(
        report,
        ExitReport {
            entries_scored: 1,
            open: 0,
            unmatched_sell_qty: Decimal::ZERO
        }
    );
    let rows = scores(&client).await;
    assert_close(value(&rows, buy, "realized_r"), 4.0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn excursions_use_the_closes_from_the_entry_fill_on() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    // Bought at 100 at 12:00 on day 2, between the day-1 close (105, from before the position
    // existed) and the day-2 close (101).
    let buy = trip(&client, v, at(2, 12), "100", "102", close_of(4)).await;
    let (_dir, reader) = bars_dir(
        "excursions",
        &[
            ("SPY", close_of(1), "105"),
            ("SPY", close_of(2), "101"),
            ("SPY", close_of(3), "103"),
            ("SPY", close_of(4), "102"),
        ],
    );

    execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    let rows = scores(&client).await;
    assert_close(value(&rows, buy, "realized_r"), 4.0);
    // Closes 101, 103, 102 (not 105): the highest is 103, MFE = 3 × 100 / 50 = 6.
    assert_close(value(&rows, buy, "mfe_r"), 6.0);
    assert_close(value(&rows, buy, "mae_r"), 2.0);
    assert_close(value(&rows, buy, "capture"), 200.0 / 300.0);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn an_entry_on_a_bar_time_includes_that_bar() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client).await;
    // Bought at 100 exactly at the day-2 bar (close 110): that bar's close is in the window.
    let buy = trip(&client, v, close_of(2), "100", "102", close_of(4)).await;
    let (_dir, reader) = bars_dir(
        "ontime",
        &[
            ("SPY", close_of(1), "105"),
            ("SPY", close_of(2), "110"),
            ("SPY", close_of(3), "103"),
            ("SPY", close_of(4), "102"),
        ],
    );

    execution_job::score_exits(&client, &reader, now())
        .await
        .unwrap();
    let rows = scores(&client).await;
    // Window 110, 103, 102: MFE = 10 × 100 / 50 = 20, MAE = 2 × 100 / 50 = 4.
    assert_close(value(&rows, buy, "mfe_r"), 20.0);
    assert_close(value(&rows, buy, "mae_r"), 4.0);
    db.drop_db().await.unwrap();
}
