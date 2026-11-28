//! Acceptance tests for the agent report (`agent_report`): the pure judging helpers
//! ([`aggregate`], [`round_sig`], [`data_checks`], the JSON shape) and [`build_agent_report`]
//! against a real Postgres (`TEST_DATABASE_URL`) with bars written to and read back from Parquet.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::NaiveDate;
use nisse_bars::write_bars;
use nisse_core::StrategyVersionId;
use nisse_core::nautilus::{
    AggregationSource, Bar, BarAggregation, BarSpecification, BarType, ClientOrderId, InstrumentId,
    OrderSide, OrderType, Price, PriceType, Quantity, Symbol, TimeInForce, UnixNanos, Venue,
    VenueOrderId,
};
use nisse_core::{DecisionId, OrderEvent, OrderIntent, OrderState};
use nisse_eval::agent_report::{
    AgentReport, DataChecks, Execution, HOLDOUT_GATE, LABEL_QUEUE_MAX, LabelEntry, MIN_GROUP,
    MetricSummary, ReportArgs, TRACKING_GATE, VersionReport, aggregate, build_agent_report,
    data_checks, round_sig,
};
use nisse_eval::execution_job::METRIC_SHORTFALL_BPS;
use nisse_eval::gate_report::{ReportInputs, TrackingInput, build_report};
use nisse_eval::gates::Gates;
use nisse_eval::outcome::Regime;
use nisse_store::tables::order_event;
use nisse_store::testing::ScratchDb;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z, in nanoseconds: the report's `now`, so its month is 2026-01.
const NOW: u64 = 1_767_600_000_123_456_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A directory under the system temp directory, unique to one test and removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nisse-agent-report-{}-{n}-{name}",
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

fn gates() -> Gates {
    Gates::load(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/eval/gates.json"
    )))
    .unwrap()
}

/// 2026-01-`day` at 21:00 UTC (the session close the daily bars sit on), in nanoseconds.
fn close_of(day: u32) -> UnixNanos {
    let secs = NaiveDate::from_ymd_opt(2026, 1, day)
        .unwrap()
        .and_hms_opt(21, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    UnixNanos::from(u64::try_from(secs).unwrap() * NANOS_PER_SECOND)
}

/// The JSON object keys, sorted.
fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

// ---- bars -----------------------------------------------------------------------------------

/// A daily bar whose four prices are `close` and whose volume is `volume`.
fn bar(symbol: &str, ts: UnixNanos, close: &str, volume: &str) -> Bar {
    let instrument_id = InstrumentId::new(Symbol::new(symbol), Venue::new("ALPACA"));
    let spec = BarSpecification::new(1, BarAggregation::Day, PriceType::Last);
    let bar_type = BarType::new(instrument_id, spec, AggregationSource::External);
    let px = Price::from(close);
    Bar::new(bar_type, px, px, px, px, Quantity::from(volume), ts, ts)
}

/// Sorts bars into time order, as `BarReader::read` returns them.
fn in_time_order(mut bars: Vec<Bar>) -> Vec<Bar> {
    bars.sort_by_key(|bar| bar.ts_event.as_u64());
    bars
}

// ---- pure: aggregate ------------------------------------------------------------------------

#[test]
fn aggregate_gives_nothing_below_min_group() {
    let values: Vec<f64> = (0..MIN_GROUP - 1).map(|i| i as f64).collect();
    assert_eq!(aggregate(&values), None);
    assert_eq!(aggregate(&[]), None);
}

#[test]
fn aggregate_gives_figures_at_exactly_min_group() {
    let values: Vec<f64> = (1..=30).map(f64::from).collect();
    // Even count: the median is the mean of the two middle values, 15 and 16.
    assert_eq!(aggregate(&values), Some((30, 15.5, 15.5)));
}

#[test]
fn aggregate_takes_the_median_of_an_unsorted_cohort() {
    // 30 values in reverse order: the median and the mean of 1..=30 are both 15.5.
    let values: Vec<f64> = (1..=30).rev().map(f64::from).collect();
    assert_eq!(aggregate(&values), Some((30, 15.5, 15.5)));
}

#[test]
fn aggregate_mean_and_median_differ_on_a_skewed_group() {
    // 29 zeros and one outlier of 10: the mean is 10 / 30, the median is 0.
    let mut values = vec![0.0_f64; 29];
    values.push(10.0);
    assert_eq!(aggregate(&values), Some((30, 10.0 / 30.0, 0.0)));
}

#[test]
fn aggregate_of_59_uses_only_the_first_cohort_in_order() {
    // The first 30 are 1.0; the last 29 are extreme and must not be used.
    let mut values = vec![1.0_f64; MIN_GROUP];
    values.extend(std::iter::repeat_n(1.0e9, MIN_GROUP - 1));
    assert_eq!(aggregate(&values), Some((30, 1.0, 1.0)));
}

#[test]
fn aggregate_of_60_is_two_cohorts() {
    // 0..=59: the mean is 29.5 and the median is the mean of 29 and 30.
    let values: Vec<f64> = (0..60).map(f64::from).collect();
    assert_eq!(aggregate(&values), Some((60, 29.5, 29.5)));
}

#[test]
fn aggregate_cohort_is_the_prefix_not_a_selection() {
    // 30 values 0..=29, then 30 and an extreme: only the prefix counts, so the mean is 14.5.
    let mut values: Vec<f64> = (0..MIN_GROUP).map(|i| i as f64).collect();
    values.push(30.0);
    values.push(-1.0e9);
    assert_eq!(aggregate(&values), Some((30, 14.5, 14.5)));
}

#[test]
fn round_sig_keeps_four_significant_digits() {
    assert_eq!(round_sig(0.0, 4), 0.0);
    assert_eq!(round_sig(1234.5678, 4), 1235.0);
    assert_eq!(round_sig(0.000_123_456, 4), 0.0001235);
    assert_eq!(round_sig(1.0, 4), 1.0);
    // Rounding can carry into a new leading digit: 0.99999 -> 1.0.
    assert_eq!(round_sig(0.99999, 4), 1.0);
    assert_eq!(round_sig(10.0 / 30.0, 4), 0.3333);
}

#[test]
fn round_sig_is_symmetric_in_sign() {
    assert_eq!(round_sig(-1234.5678, 4), -1235.0);
    assert_eq!(round_sig(-0.000_123_456, 4), -0.0001235);
    assert_eq!(round_sig(-0.0, 4), 0.0);
}

#[test]
fn round_sig_leaves_non_finite_and_unscalable_values_alone() {
    assert!(round_sig(f64::NAN, 4).is_nan());
    assert_eq!(round_sig(f64::INFINITY, 4), f64::INFINITY);
    assert_eq!(round_sig(f64::NEG_INFINITY, 4), f64::NEG_INFINITY);
    // 10^313 is not finite, so the value is returned as it is.
    assert_eq!(round_sig(1.0e-310, 4), 1.0e-310);
}

// ---- pure: JSON shape -----------------------------------------------------------------------

/// A report with one version carrying `scores` and `label_queue` as given.
fn report_with(scores: Vec<(String, MetricSummary)>) -> AgentReport {
    AgentReport {
        generated_month: "2026-01".to_owned(),
        versions: vec![VersionReport {
            strategy_version_id: 3,
            git_sha: "a".repeat(40),
            config_hash: "b".repeat(64),
            visible_decisions: 0,
            by_action: Default::default(),
            scores: scores.into_iter().collect(),
            gates: Default::default(),
        }],
        data: None,
        regime: Regime::Unknown,
        label_queue: Vec::new(),
        notes: Vec::new(),
    }
}

#[test]
fn aggregates_are_published_with_four_significant_digits() {
    let value = report_with(vec![(
        "sharp".to_owned(),
        MetricSummary::Aggregate {
            n: 30,
            mean: 10.0 / 30.0,
            median: 1234.5678,
        },
    )])
    .to_json();
    assert_eq!(
        value["versions"][0]["scores"]["sharp"],
        json!({"n": 30, "mean": 0.3333, "median": 1235.0})
    );
}

#[test]
fn suppressed_metric_is_the_string_and_an_aggregate_has_three_keys() {
    let value = report_with(vec![
        ("sharp".to_owned(), MetricSummary::Suppressed),
        (
            "mean_only".to_owned(),
            MetricSummary::Aggregate {
                n: 30,
                mean: 1.5,
                median: 2.0,
            },
        ),
    ])
    .to_json();
    let scores = &value["versions"][0]["scores"];
    assert_eq!(scores["sharp"], json!("suppressed"));
    assert_eq!(
        scores["mean_only"],
        json!({"n": 30, "mean": 1.5, "median": 2.0})
    );
}

#[test]
fn report_json_has_the_documented_top_level_keys_and_no_decision_list() {
    let report = AgentReport {
        generated_month: "2026-01".to_owned(),
        versions: Vec::new(),
        data: None,
        regime: Regime::Unknown,
        label_queue: Vec::new(),
        notes: vec!["one note".to_owned()],
    };
    let value = report.to_json();
    assert_eq!(
        keys(&value),
        [
            "data",
            "generated_month",
            "label_queue",
            "min_group",
            "notes",
            "regime",
            "versions"
        ]
    );
    assert_eq!(value["min_group"], json!(MIN_GROUP));
    assert_eq!(value["data"], Value::Null);
    assert_eq!(value["regime"], json!("unknown"));
    assert_eq!(value["notes"], json!(["one note"]));
}

#[test]
fn label_entry_json_carries_execution_but_no_outcome_or_quality_fields() {
    let mut report = report_with(Vec::new());
    report.label_queue.push(LabelEntry {
        decision_id: 9,
        symbol: "SPY".to_owned(),
        action: "enter".to_owned(),
        reason_code: "trend_up".to_owned(),
        month: "2026-01".to_owned(),
        features: json!({"close": 100.0}),
        execution: Execution {
            orders: 2,
            filled_fraction: "1".to_owned(),
            shortfall_bps: None,
        },
    });
    let value = report.to_json();
    let entry = &value["label_queue"][0];
    assert_eq!(
        keys(entry),
        [
            "action",
            "decision_id",
            "execution",
            "features",
            "month",
            "reason_code",
            "symbol"
        ]
    );
    // No shortfall key when the decision has no shortfall score.
    assert_eq!(
        entry["execution"],
        json!({"orders": 2, "filled_fraction": "1"})
    );
    let text = value.to_string();
    for forbidden in [
        "forward_return",
        "benchmark_return",
        "pnl",
        "realized_r",
        "decision_quality",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} leaked into the report"
        );
    }
}

#[test]
fn data_json_names_the_fields_and_keeps_move_text_as_written() {
    let mut report = report_with(Vec::new());
    report.data = Some(nisse_eval::agent_report::DataReport {
        bar_file: "bars.parquet".to_owned(),
        checks: DataChecks {
            last_session: Some("2026-01-25".to_owned()),
            missing_on_last_session: vec!["IWM".to_owned()],
            zero_volume: Vec::new(),
            large_moves: vec![nisse_eval::agent_report::LargeMove {
                symbol: "SPY".to_owned(),
                date: "2026-01-22".to_owned(),
                move_pct: "-52.0".to_owned(),
            }],
        },
    });
    let value = report.to_json();
    assert_eq!(
        keys(&value["data"]),
        [
            "bar_file",
            "large_moves",
            "last_session",
            "missing_on_last_session",
            "zero_volume"
        ]
    );
    assert_eq!(
        value["data"]["large_moves"][0],
        json!({"symbol": "SPY", "date": "2026-01-22", "move_pct": "-52.0"})
    );
}

// ---- pure: data checks ----------------------------------------------------------------------

/// Three symbols over 25 sessions (2026-01-01..=25) with planted faults. The lookback window
/// is the 20 newest sessions, 2026-01-06..=25, so faults before day 6 must not be reported.
fn fault_bars() -> Vec<Bar> {
    let mut bars = Vec::new();
    for day in 1..=25_u32 {
        let mut spy = (100, 1000);
        let mut qqq = (100, 1000);
        // Outside the window: SPY doubles on day 3 (then halves on day 4) and has zero volume
        // on day 2.
        if day == 2 {
            spy.1 = 0;
        }
        if day == 3 {
            spy.0 = 200;
        }
        if day == 4 {
            spy.0 = 100;
        }
        // Inside the window: QQQ is 130 on day 10 only (+30%, then -23% back, not flagged),
        // has zero volume on day 7, and falls to 0 on day 15 (-100%) then recovers to 50 on
        // day 16 and stays there (a move from a zero close is not judged).
        if day == 10 {
            qqq.0 = 130;
        }
        if day == 7 {
            qqq.1 = 0;
        }
        if day == 15 {
            qqq.0 = 0;
        }
        if day >= 16 {
            qqq.0 = 50;
        }
        // SPY moves exactly +25% on day 21 (not over the limit), then -52% on day 22.
        if day == 21 {
            spy.0 = 125;
        }
        if day >= 22 {
            spy.0 = 60;
        }
        bars.push(bar(
            "SPY",
            close_of(day),
            &spy.0.to_string(),
            &spy.1.to_string(),
        ));
        bars.push(bar(
            "QQQ",
            close_of(day),
            &qqq.0.to_string(),
            &qqq.1.to_string(),
        ));
        if day <= 24 {
            bars.push(bar("IWM", close_of(day), "100", "1000"));
        }
    }
    in_time_order(bars)
}

#[test]
fn data_checks_flag_only_faults_inside_the_lookback() {
    let bars = fault_bars();
    let checks = data_checks(&bars, &["SPY", "QQQ", "IWM"]);

    assert_eq!(checks.last_session.as_deref(), Some("2026-01-25"));
    assert_eq!(checks.missing_on_last_session, vec!["IWM".to_owned()]);

    let zero: Vec<(&str, &str)> = checks
        .zero_volume
        .iter()
        .map(|z| (z.symbol.as_str(), z.date.as_str()))
        .collect();
    assert_eq!(
        zero,
        vec![("QQQ", "2026-01-07")],
        "day-2 zero volume is outside the window"
    );

    let moves: Vec<(&str, &str, &str)> = checks
        .large_moves
        .iter()
        .map(|m| (m.symbol.as_str(), m.date.as_str(), m.move_pct.as_str()))
        .collect();
    assert_eq!(
        moves,
        vec![
            ("QQQ", "2026-01-10", "30.0"),
            ("QQQ", "2026-01-15", "-100.0"),
            ("SPY", "2026-01-22", "-52.0"),
        ],
        "day-3 and day-4 moves are outside the window; the exact 25% and the move from a zero \
         close are not flagged"
    );
}

#[test]
fn data_checks_on_no_bars_has_no_session_and_no_faults() {
    let checks = data_checks(&[], &["SPY"]);
    assert_eq!(checks.last_session, None);
    assert_eq!(checks.missing_on_last_session, vec!["SPY".to_owned()]);
    assert!(checks.zero_volume.is_empty());
    assert!(checks.large_moves.is_empty());
}

#[test]
fn data_checks_window_starts_at_the_oldest_session_when_there_are_few() {
    // Two sessions only: the window starts at the oldest, so the day-1 zero volume counts.
    let bars = in_time_order(vec![
        bar("SPY", close_of(1), "100", "0"),
        bar("SPY", close_of(2), "100", "5"),
    ]);
    let checks = data_checks(&bars, &["SPY"]);
    assert_eq!(checks.zero_volume.len(), 1);
    assert_eq!(checks.zero_volume[0].date, "2026-01-01");
    assert!(checks.missing_on_last_session.is_empty());
}

#[test]
fn the_tracking_gate_is_in_the_gate_report_so_the_report_can_leave_it_out() {
    // If the gate report renames its tracking gate, the name this module filters on is stale and
    // the tracking result would reach agents: this fails instead.
    let inputs = ReportInputs {
        version: StrategyVersionId(1),
        gates: gates(),
        lifetime_trials: 0,
        holdout_looks: 0,
        trials: Vec::new(),
        replay: (0, 0),
        risk_breaches: 0,
        sent_orders: 0,
        live: Some(TrackingInput {
            environment: "paper",
            live: vec![0.01, 0.02],
            backtest: vec![0.01, 0.02],
            span: None,
            gap: None,
            lag_sessions: 0,
            same_experiment: true,
            unreadable: false,
        }),
    };
    let names: Vec<&str> = build_report(&inputs).gates.iter().map(|g| g.name).collect();
    assert!(
        names.contains(&TRACKING_GATE),
        "gate report names {names:?} do not contain {TRACKING_GATE:?}"
    );
}

#[test]
fn the_holdout_gate_is_in_the_gate_report_so_the_report_can_leave_it_out() {
    // If the gate report renames its holdout gate, the name this module filters on is stale and
    // the holdout result would reach agents: this fails instead.
    let inputs = ReportInputs {
        version: StrategyVersionId(1),
        gates: gates(),
        lifetime_trials: 0,
        holdout_looks: 0,
        trials: Vec::new(),
        replay: (0, 0),
        risk_breaches: 0,
        sent_orders: 0,
        live: None,
    };
    let names: Vec<&str> = build_report(&inputs).gates.iter().map(|g| g.name).collect();
    assert!(
        names.contains(&HOLDOUT_GATE),
        "gate report names {names:?} do not contain {HOLDOUT_GATE:?}"
    );
}

// ---- store: build_agent_report --------------------------------------------------------------

/// Inserts a strategy version; `n` keeps `(git_sha, config_hash)` unique per version.
async fn version(client: &Client, n: u32) -> i64 {
    client
        .query_one(
            "INSERT INTO strategy_version (git_sha, config_hash, params, event_ts) \
             VALUES ($1, $2, '{}', '2026-01-02T00:00:00Z') RETURNING id",
            &[&format!("{n:040x}"), &vec![n as u8; 32]],
        )
        .await
        .unwrap()
        .get(0)
}

/// Inserts one decision on `SPY` at `event_ts` (an RFC 3339 timestamp).
async fn decision(
    client: &Client,
    version: i64,
    action: &str,
    event_ts: &str,
    features: Value,
) -> DecisionId {
    let id: i64 = client
        .query_one(
            "INSERT INTO decision (strategy_version_id, symbol, action, intended_qty, features, \
                                   feature_hash, data_snapshot_id, rng_seed, reason_code, event_ts) \
             VALUES ($1, 'SPY', $2, 1.5, $3, decode(repeat('ab', 32), 'hex'), 'snap-1', \
                     42, 'trend_up', $4::text::timestamptz) RETURNING id",
            &[&version, &action, &features, &event_ts],
        )
        .await
        .unwrap()
        .get(0);
    DecisionId(id)
}

/// Makes `decision` visible (`hidden = false`) or held out (`hidden = true`).
async fn holdout(client: &Client, decision: DecisionId, hidden: bool) {
    client
        .execute(
            "INSERT INTO decision_holdout (decision_id, hidden, event_ts) \
             VALUES ($1, $2, '2026-01-05T21:00:00Z')",
            &[&decision.0, &hidden],
        )
        .await
        .unwrap();
}

/// Writes a shortfall score for `decision` under `eval_version`.
async fn shortfall(client: &Client, decision: DecisionId, value: f64, eval_version: &str) {
    client
        .execute(
            "INSERT INTO eval_score (subject_id, subject_type, metric, value, eval_version, event_ts) \
             VALUES ($1, 'decision', $2, $3::float8, $4, '2026-01-05T21:00:00Z')",
            &[&decision.0, &METRIC_SHORTFALL_BPS, &value, &eval_version],
        )
        .await
        .unwrap();
}

/// A visible `enter` decision with one order, filled in full in one fill.
async fn filled_entry(client: &Client, version: i64, event_ts: &str, n: u32) -> DecisionId {
    let id = decision(client, version, "enter", event_ts, json!({"close": 100.0})).await;
    holdout(client, id, false).await;
    let coid = format!("C-{n}");
    let order = OrderIntent {
        decision_id: id,
        client_order_id: ClientOrderId::new_checked(&coid).unwrap(),
        symbol: Symbol::new_checked("SPY").unwrap(),
        side: OrderSide::Buy,
        order_type: OrderType::Market,
        qty: Quantity::from("1.5"),
        limit_px: None,
        time_in_force: TimeInForce::Day,
        ts: close_of(5),
    };
    order_event::insert(client, &OrderEvent::intent(&order))
        .await
        .unwrap();
    let filled = OrderEvent::of(
        &order,
        OrderState::Filled,
        Some(VenueOrderId::from("V-1")),
        Quantity::from("1.5"),
        close_of(5),
    );
    order_event::insert(client, &filled).await.unwrap();
    client
        .execute(
            "INSERT INTO fill (client_order_id, broker_fill_id, px, qty, fee, event_ts) \
             VALUES ($1, $2, 100, 1.5, 0, '2026-01-05T21:00:00Z')",
            &[&coid, &format!("T-{coid}")],
        )
        .await
        .unwrap();
    id
}

/// The report over `bars_dir` with the repository gates, the given symbols and benchmark SPY.
async fn run_report(
    client: &Client,
    bars_dir: &Path,
    symbols: &[String],
) -> Result<AgentReport, nisse_eval::EvalError> {
    let gates = gates();
    build_agent_report(
        client,
        &ReportArgs {
            gates: &gates,
            bars_dir,
            symbols,
            benchmark: "SPY",
            now: UnixNanos::from(NOW),
        },
    )
    .await
}

/// The version's report with id `id`.
fn version_of(report: &AgentReport, id: i64) -> &VersionReport {
    report
        .versions
        .iter()
        .find(|x| x.strategy_version_id == id)
        .unwrap()
}

#[tokio::test]
async fn hidden_and_unassigned_decisions_are_never_counted_or_listed() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("hidden");

    let visible = decision(&client, v, "hold", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, visible, false).await;
    let held_out = decision(&client, v, "exit", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, held_out, true).await;
    // No holdout row at all: hidden from agents.
    let _unassigned = decision(&client, v, "skip", "2026-01-05T21:00:00Z", json!({})).await;

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    let version = version_of(&report, v);
    assert_eq!(version.visible_decisions, 1);
    assert_eq!(version.by_action.get("hold"), Some(&1));
    assert!(!version.by_action.contains_key("exit"));
    assert!(!version.by_action.contains_key("skip"));
    // A hold never reaches the label queue, and nothing here has an order.
    assert!(report.label_queue.is_empty());

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn score_is_suppressed_below_min_group_and_given_at_min_group() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("suppress");

    // Values 0..=28: 29 decisions, one short of the minimum group.
    for i in 0..(MIN_GROUP as u32 - 1) {
        let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
        holdout(&client, id, false).await;
        shortfall(&client, id, f64::from(i), "score-v1").await;
    }
    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    assert_eq!(
        version_of(&report, v).scores.get(METRIC_SHORTFALL_BPS),
        Some(&MetricSummary::Suppressed),
        "29 decisions are below the minimum group"
    );
    assert_eq!(
        report.to_json()["versions"][0]["scores"][METRIC_SHORTFALL_BPS],
        json!("suppressed")
    );

    // A thirtieth decision, scoring 100, brings the group to MIN_GROUP.
    let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, id, false).await;
    shortfall(&client, id, 100.0, "score-v1").await;
    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    match version_of(&report, v).scores.get(METRIC_SHORTFALL_BPS) {
        Some(MetricSummary::Aggregate { n, mean, median }) => {
            assert_eq!(*n, 30);
            // Values 0..=28 sum to 406; with 100 the sum is 506 over 30. The two middle values
            // are 14 and 15.
            assert!((mean - 506.0 / 30.0).abs() < 1e-9, "mean was {mean}");
            assert!((median - 14.5).abs() < 1e-9, "median was {median}");
        }
        other => panic!("expected an aggregate at {MIN_GROUP} decisions, got {other:?}"),
    }

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn newest_score_row_per_decision_is_the_one_counted_once() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("newest");

    for _ in 0..MIN_GROUP {
        let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
        holdout(&client, id, false).await;
        // An older score under score-v1 is superseded by the newer score-v2 row.
        shortfall(&client, id, 0.0, "score-v1").await;
        shortfall(&client, id, 1.0, "score-v2").await;
    }

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    // Each decision counts once, with its newest value: n is the decision count, not twice it.
    assert_eq!(
        version_of(&report, v).scores.get(METRIC_SHORTFALL_BPS),
        Some(&MetricSummary::Aggregate {
            n: MIN_GROUP,
            mean: 1.0,
            median: 1.0,
        })
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn label_queue_skips_holds_and_unordered_decisions_and_is_capped_newest_first() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("queue");

    // A visible hold, and a visible entry with no order: neither is listed.
    let hold = decision(&client, v, "hold", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, hold, false).await;
    let no_order = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, no_order, false).await;

    let mut filled = Vec::new();
    for n in 0..(LABEL_QUEUE_MAX as u32 + 2) {
        filled.push(filled_entry(&client, v, "2026-01-05T21:00:00Z", n).await.0);
    }
    let report = run_report(&client, dir.path(), &[]).await.unwrap();

    assert_eq!(report.label_queue.len(), LABEL_QUEUE_MAX);
    let listed: Vec<i64> = report.label_queue.iter().map(|e| e.decision_id).collect();
    let newest: Vec<i64> = filled.iter().rev().take(LABEL_QUEUE_MAX).copied().collect();
    assert_eq!(listed, newest, "newest first, capped at LABEL_QUEUE_MAX");
    assert!(!listed.contains(&hold.0));
    assert!(!listed.contains(&no_order.0));

    let first = &report.label_queue[0];
    assert_eq!(first.execution.orders, 1);
    assert_eq!(first.execution.filled_fraction, "1");
    assert_eq!(first.execution.shortfall_bps, None);
    assert_eq!(first.month, "2026-01");
    assert_eq!(first.features, json!({"close": 100.0}));

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn label_entry_carries_the_shortfall_score_when_there_is_one() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("shortfall");

    let id = filled_entry(&client, v, "2026-01-05T21:00:00Z", 0).await;
    shortfall(&client, id, 12.5, "score-v1").await;

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    assert_eq!(report.label_queue.len(), 1);
    assert_eq!(report.label_queue[0].execution.shortfall_bps, Some(12.5));
    assert_eq!(
        report.to_json()["label_queue"][0]["execution"]["shortfall_bps"],
        json!(12.5)
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn decisions_are_counted_per_version_and_the_month_is_the_report_month() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let first = version(&client, 1).await;
    let second = version(&client, 2).await;
    let dir = TempDir::new("versions");

    let a = decision(&client, first, "enter", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, a, false).await;
    let b = decision(&client, first, "enter", "2026-01-06T21:00:00Z", json!({})).await;
    holdout(&client, b, false).await;
    let c = decision(&client, second, "exit", "2026-01-06T21:00:00Z", json!({})).await;
    holdout(&client, c, false).await;

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    assert_eq!(report.generated_month, "2026-01");
    assert_eq!(report.versions.len(), 2);
    let one = version_of(&report, first);
    let two = version_of(&report, second);
    assert_eq!(one.visible_decisions, 2);
    assert_eq!(two.visible_decisions, 1);
    assert_eq!(two.by_action.get("exit"), Some(&1));

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn empty_bar_directory_gives_no_data_and_an_unknown_regime() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let dir = TempDir::new("nobars");

    let report = run_report(&client, dir.path(), &["SPY".to_owned()])
        .await
        .unwrap();
    assert!(report.data.is_none());
    assert_eq!(report.regime, Regime::Unknown);
    assert_eq!(report.to_json()["data"], Value::Null);

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn bar_file_gives_the_data_checks_for_the_config_symbols() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let dir = TempDir::new("bars");
    let bars = in_time_order(vec![
        bar("SPY", close_of(1), "100", "1000"),
        bar("QQQ", close_of(1), "50", "1000"),
        bar("SPY", close_of(2), "101", "1000"),
    ]);
    write_bars(&dir.path().join("bars.parquet"), &bars).unwrap();

    let symbols = vec!["QQQ".to_owned()];
    let report = run_report(&client, dir.path(), &symbols).await.unwrap();
    let data = report.data.as_ref().expect("a bar file gives data");
    assert_eq!(data.bar_file, "bars.parquet");
    assert_eq!(data.checks.last_session.as_deref(), Some("2026-01-02"));
    // QQQ has no bar on the newest session.
    assert_eq!(data.checks.missing_on_last_session, vec!["QQQ".to_owned()]);

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_bar_path_that_is_not_a_directory_is_an_error() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let dir = TempDir::new("file");
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();

    assert!(run_report(&client, &file, &[]).await.is_err());

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_cohort_leaves_out_the_thirty_first_decision() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("cohort");

    // Decisions 0..=29 score 0..=29; the 31st scores an extreme value.
    for i in 0..MIN_GROUP as u32 {
        let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
        holdout(&client, id, false).await;
        shortfall(&client, id, f64::from(i), "score-v1").await;
    }
    let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, id, false).await;
    shortfall(&client, id, 1.0e9, "score-v1").await;

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    assert_eq!(
        version_of(&report, v).scores.get(METRIC_SHORTFALL_BPS),
        Some(&MetricSummary::Aggregate {
            n: MIN_GROUP,
            mean: 14.5,
            median: 14.5,
        }),
        "the 31st decision is outside the first cohort"
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn adding_one_decision_leaves_the_published_scores_byte_identical() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("difference");

    for i in 0..MIN_GROUP as u32 {
        let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
        holdout(&client, id, false).await;
        shortfall(&client, id, f64::from(i) * 0.37 + 1.0, "score-v1").await;
    }
    let before = run_report(&client, dir.path(), &[])
        .await
        .unwrap()
        .to_json();

    // Tomorrow's report differs from today's by one decision with an extreme outcome.
    let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
    holdout(&client, id, false).await;
    shortfall(&client, id, 1.0e9, "score-v1").await;
    let after = run_report(&client, dir.path(), &[])
        .await
        .unwrap()
        .to_json();

    assert_eq!(
        after["versions"][0]["visible_decisions"],
        json!(MIN_GROUP + 1)
    );
    assert_eq!(
        before["versions"][0]["scores"].to_string(),
        after["versions"][0]["scores"].to_string()
    );
    assert_eq!(
        after["versions"][0]["scores"][METRIC_SHORTFALL_BPS]["n"],
        json!(MIN_GROUP)
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_cohort_is_chosen_by_decision_id_not_by_when_the_score_was_written() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("order");

    let mut ids = Vec::new();
    for _ in 0..=MIN_GROUP {
        let id = decision(&client, v, "enter", "2026-01-05T21:00:00Z", json!({})).await;
        holdout(&client, id, false).await;
        ids.push(id);
    }
    // Scores are written newest decision first, so eval_score ids run against decision ids.
    for (i, id) in ids.iter().enumerate().rev() {
        shortfall(&client, *id, i as f64, "score-v1").await;
    }

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    assert_eq!(
        version_of(&report, v).scores.get(METRIC_SHORTFALL_BPS),
        Some(&MetricSummary::Aggregate {
            n: MIN_GROUP,
            mean: 14.5,
            median: 14.5,
        }),
        "the cohort is decisions 0..=29 in decision id order"
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_gates_map_leaves_out_the_holdout_gate_and_the_notes_say_so() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("gates");

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    let gates = &version_of(&report, v).gates;
    assert!(
        gates.contains_key("trial_budget"),
        "the other gates are still shown"
    );
    assert!(
        !gates.contains_key(HOLDOUT_GATE),
        "the holdout gate is not shown"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("holdout gate is not shown to agents")),
        "notes: {:?}",
        report.notes
    );
    assert!(
        report.to_json()["notes"]
            .to_string()
            .contains("holdout gate is not shown")
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_tracking_gate_is_not_shown_to_agents() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let dir = TempDir::new("tracking");

    let report = run_report(&client, dir.path(), &[]).await.unwrap();
    let gates = &version_of(&report, v).gates;
    assert!(
        gates.contains_key("trial_budget"),
        "the other gates are still shown"
    );
    assert!(
        !gates.contains_key(TRACKING_GATE),
        "the tracking gate is not shown: {gates:?}"
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("tracking gate is not shown to agents")),
        "notes: {:?}",
        report.notes
    );
    assert!(
        report.to_json()["notes"]
            .to_string()
            .contains("tracking gate is not shown")
    );

    drop(client);
    db.drop_db().await.unwrap();
}
