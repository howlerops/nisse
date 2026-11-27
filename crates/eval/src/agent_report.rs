//! The report agents read: aggregates only, never a hidden decision and never an outcome
//! beside a decision. See `docs/phases/phase-7.md`, "Agent report".
//!
//! Rules, each one a safety property with a test:
//! - a decision is visible only through a `decision_holdout` row with `hidden = false`; a
//!   decision with no row is hidden;
//! - a score metric is given as `n`, `mean` and `median` only when at least `MIN_GROUP`
//!   decisions have it, otherwise as `"suppressed"`;
//! - score aggregates are cohorts: for each version and metric the visible decisions are
//!   ordered by decision id ascending and only the first `k * MIN_GROUP` are aggregated, where
//!   `k = floor(count / MIN_GROUP)`. So `n` is always a multiple of `MIN_GROUP`, and the
//!   figures change only when 30 more decisions have accrued. Why: the report is rewritten
//!   every night, and with `n` and a full-precision mean per night, the difference between two
//!   consecutive nights would be exactly the newest decision's score (for an outcome metric,
//!   its outcome). A difference over a whole cohort is not one decision. `mean` and `median`
//!   are rounded to [`SIG_DIGITS`] significant digits in the JSON so that no finer difference
//!   is left to subtract;
//! - the holdout gate is not shown (see [`HOLDOUT_GATE`]), because its pass or fail would give
//!   feedback on the holdout, which is only looked at a few times on purpose;
//! - a label queue entry says what was decided and how it was executed, and never carries a
//!   forward return, a benchmark return, P&L or a `decision_quality_*` score;
//! - decisions are dated by month; only the data checks use days.
//!
//! Loading is in this module (`SELECT` only, through `GenericClient`); the judging helpers
//! ([`aggregate`], [`data_checks`]) are pure and tested without a database.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use nisse_bars::BarReader;
use nisse_core::StrategyVersionId;
use nisse_core::nautilus::{Bar, UnixNanos};
use nisse_store::StoreError;
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use tokio_postgres::GenericClient;

use crate::EvalError;
use crate::execution_job::METRIC_SHORTFALL_BPS;
use crate::gate_report::{GateStatus, build_report, load_inputs};
use crate::gates::Gates;
use crate::outcome::{BarPoint, Regime, regime};

/// Fewest decisions a score metric needs before its aggregate is given.
pub const MIN_GROUP: usize = 30;
/// Most entries in the label queue.
pub const LABEL_QUEUE_MAX: usize = 20;
/// Sessions (distinct bar times, newest last) the data checks look back over.
pub const DATA_LOOKBACK_SESSIONS: usize = 20;
/// A one-bar close-to-close move above this many percent is flagged.
pub const LARGE_MOVE_PCT: u32 = 25;

/// Significant digits `mean` and `median` are rounded to in the JSON.
pub const SIG_DIGITS: u32 = 4;
/// The gate that judges the holdout period, as its name in the gate report. It is left out of
/// the per-version `gates` map. Matched by name because the gate report keeps the name in a
/// private constant; a test fails if the gate report stops using this name.
pub const HOLDOUT_GATE: &str = "holdout";

/// The value given for a score metric with fewer than [`MIN_GROUP`] decisions.
const SUPPRESSED: &str = "suppressed";
/// The note that tells agents the holdout gate is left out of the report.
const HOLDOUT_NOTE: &str = "The holdout gate is not shown to agents: its result would give \
     feedback on the holdout period, which is limited to a few deliberate looks.";

/// What the report is built from: the gate thresholds, the bar directory, the research
/// config's symbols and benchmark, and the time the report is made at.
#[derive(Debug, Clone, Copy)]
pub struct ReportArgs<'a> {
    pub gates: &'a Gates,
    pub bars_dir: &'a Path,
    pub symbols: &'a [String],
    pub benchmark: &'a str,
    pub now: UnixNanos,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentReport {
    /// `YYYY-MM`, UTC, of `ReportArgs::now`.
    pub generated_month: String,
    pub versions: Vec<VersionReport>,
    /// `None` when the bar directory holds no Parquet file.
    pub data: Option<DataReport>,
    pub regime: Regime,
    pub label_queue: Vec<LabelEntry>,
    /// Things that could not be reported, one sentence each.
    pub notes: Vec<String>,
}

/// One strategy version's figures over the decisions agents may see.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionReport {
    pub strategy_version_id: i64,
    pub git_sha: String,
    pub config_hash: String,
    pub visible_decisions: i64,
    /// Visible decisions per action, for every action that occurs.
    pub by_action: BTreeMap<String, i64>,
    /// Per score metric over the visible decisions.
    pub scores: BTreeMap<String, MetricSummary>,
    /// Per gate name. Empty when the gate report could not be built (see the report's notes).
    pub gates: BTreeMap<String, GateStatus>,
}

/// One score metric over a group of decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricSummary {
    Aggregate {
        n: usize,
        mean: f64,
        median: f64,
    },
    /// Fewer than [`MIN_GROUP`] decisions: no figure is given.
    Suppressed,
}

/// The data checks and the bar file they were made from.
#[derive(Debug, Clone, PartialEq)]
pub struct DataReport {
    pub bar_file: String,
    pub checks: DataChecks,
}

/// The result of [`data_checks`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DataChecks {
    /// The UTC date of the newest bar, `None` when there are no bars.
    pub last_session: Option<String>,
    /// Symbols with no bar at the newest bar time, in symbol order.
    pub missing_on_last_session: Vec<String>,
    /// Bars with zero volume in the lookback window, in time order.
    pub zero_volume: Vec<ZeroVolume>,
    /// Close-to-close moves over [`LARGE_MOVE_PCT`] that end in the lookback window.
    pub large_moves: Vec<LargeMove>,
}

/// A bar with zero volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZeroVolume {
    pub symbol: String,
    pub date: String,
}

/// A one-bar move beyond [`LARGE_MOVE_PCT`]. `move_pct` is decimal text with one decimal place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LargeMove {
    pub symbol: String,
    pub date: String,
    pub move_pct: String,
}

/// One decision in the label queue: what was decided and how it was executed. No outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelEntry {
    pub decision_id: i64,
    pub symbol: String,
    pub action: String,
    pub reason_code: String,
    /// `YYYY-MM` of the decision.
    pub month: String,
    /// The decision's features, as the engine recorded them at decision time.
    pub features: Value,
    pub execution: Execution,
}

/// How a decision's orders were carried out.
#[derive(Debug, Clone, PartialEq)]
pub struct Execution {
    pub orders: i64,
    /// Filled quantity over intended quantity, as decimal text; `"0"` when nothing filled.
    pub filled_fraction: String,
    /// The decision's `shortfall_bps` score, when it has one.
    pub shortfall_bps: Option<f64>,
}

impl AgentReport {
    /// The report as the JSON object agents read.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "generated_month": self.generated_month,
            "min_group": MIN_GROUP,
            "versions": self.versions.iter().map(VersionReport::to_json).collect::<Vec<Value>>(),
            "data": self.data.as_ref().map_or(Value::Null, DataReport::to_json),
            "regime": self.regime.as_str(),
            "label_queue": self.label_queue.iter().map(LabelEntry::to_json).collect::<Vec<Value>>(),
            "notes": self.notes,
        })
    }
}

impl VersionReport {
    fn to_json(&self) -> Value {
        json!({
            "strategy_version_id": self.strategy_version_id,
            "git_sha": self.git_sha,
            "config_hash": self.config_hash,
            "visible_decisions": self.visible_decisions,
            "by_action": object(self.by_action.iter().map(|(k, v)| (k.clone(), json!(v)))),
            "scores": object(self.scores.iter().map(|(k, v)| (k.clone(), v.to_json()))),
            "gates": object(self.gates.iter().map(|(k, v)| (k.clone(), json!(v.as_str())))),
        })
    }
}

impl MetricSummary {
    fn to_json(self) -> Value {
        match self {
            Self::Aggregate { n, mean, median } => json!({
                "n": n,
                "mean": round_sig(mean, SIG_DIGITS),
                "median": round_sig(median, SIG_DIGITS),
            }),
            Self::Suppressed => json!(SUPPRESSED),
        }
    }
}

impl DataReport {
    fn to_json(&self) -> Value {
        json!({
            "bar_file": self.bar_file,
            "last_session": self.checks.last_session,
            "missing_on_last_session": self.checks.missing_on_last_session,
            "zero_volume": self.checks.zero_volume.iter().map(|z| json!({
                "symbol": z.symbol, "date": z.date,
            })).collect::<Vec<Value>>(),
            "large_moves": self.checks.large_moves.iter().map(|m| json!({
                "symbol": m.symbol, "date": m.date, "move_pct": m.move_pct,
            })).collect::<Vec<Value>>(),
        })
    }
}

impl LabelEntry {
    fn to_json(&self) -> Value {
        json!({
            "decision_id": self.decision_id,
            "symbol": self.symbol,
            "action": self.action,
            "reason_code": self.reason_code,
            "month": self.month,
            "features": self.features,
            "execution": self.execution.to_json(),
        })
    }
}

impl Execution {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("orders".to_owned(), json!(self.orders));
        map.insert("filled_fraction".to_owned(), json!(self.filled_fraction));
        if let Some(shortfall) = self.shortfall_bps {
            map.insert("shortfall_bps".to_owned(), json!(shortfall));
        }
        Value::Object(map)
    }
}

/// A JSON object from (key, value) pairs.
fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
    Value::Object(entries.into_iter().collect::<Map<String, Value>>())
}

/// Builds the report from the store and the bar directory. Reads only.
///
/// A version whose gate report cannot be built gets an empty `gates` object and a note; the
/// rest of the report is still given.
pub async fn build_agent_report<C: GenericClient>(
    client: &C,
    args: &ReportArgs<'_>,
) -> Result<AgentReport, EvalError> {
    let generated_month = month_of(args.now)?;
    let versions = read_versions(client).await?;
    let mut counts = group_counts(read_action_counts(client).await?);
    let mut scores = group_scores(read_decision_scores(client).await?);
    let label_queue = read_label_queue(client).await?;
    let (data, regime) = read_data(args)?;

    let mut report_versions = Vec::with_capacity(versions.len());
    let mut notes = vec![HOLDOUT_NOTE.to_owned()];
    for version in versions {
        let by_action = counts.remove(&version.id).unwrap_or_default();
        let visible_decisions = by_action.values().sum();
        let metrics = scores.remove(&version.id).unwrap_or_default();
        let summaries = metrics
            .into_iter()
            .map(|(metric, values)| {
                // Values are in decision id order, as `read_decision_scores` delivers them.
                let summary = match aggregate(&values) {
                    Some((n, mean, median)) => MetricSummary::Aggregate { n, mean, median },
                    None => MetricSummary::Suppressed,
                };
                (metric, summary)
            })
            .collect();
        let gates = match gate_statuses(client, version.id, args.gates).await {
            Ok(statuses) => statuses,
            Err(err) => {
                notes.push(format!(
                    "strategy version {}: gate report not built: {err}",
                    version.id
                ));
                BTreeMap::new()
            }
        };
        report_versions.push(VersionReport {
            strategy_version_id: version.id,
            git_sha: version.git_sha,
            config_hash: version.config_hash,
            visible_decisions,
            by_action,
            scores: summaries,
            gates,
        });
    }

    Ok(AgentReport {
        generated_month,
        versions: report_versions,
        data,
        regime,
        label_queue,
        notes,
    })
}

/// Summarises one metric's values given in decision id order: `(n, mean, median)` over the
/// cohort, the first `k * MIN_GROUP` values where `k = floor(len / MIN_GROUP)`. `None` when
/// `k` is zero, that is when there are fewer than [`MIN_GROUP`] values. The values past the
/// cohort are not used at all. The median of an even count is the mean of the two middle
/// values. Figures are full precision; rounding for the JSON is in [`MetricSummary`].
#[must_use]
pub fn aggregate(values_in_decision_id_order: &[f64]) -> Option<(usize, f64, f64)> {
    let k = values_in_decision_id_order.len() / MIN_GROUP;
    if k == 0 {
        return None;
    }
    let n = k * MIN_GROUP;
    let mut cohort = values_in_decision_id_order.get(..n)?.to_vec();
    cohort.sort_by(f64::total_cmp);
    let mean = cohort.iter().sum::<f64>() / n as f64;
    let middle = n / 2;
    let median = (*cohort.get(middle - 1)? + *cohort.get(middle)?) / 2.0;
    Some((n, mean, median))
}

/// `value` rounded to `digits` significant decimal digits. Zero, and any value that is not
/// finite or whose scaling would overflow, is returned unchanged.
#[must_use]
pub fn round_sig(value: f64, digits: u32) -> f64 {
    if value == 0.0 || !value.is_finite() {
        return value;
    }
    // Position of the leading digit: 10^exponent <= |value| < 10^(exponent + 1).
    let exponent = value.abs().log10().floor();
    let Ok(scale_power) = i32::try_from(i64::from(digits) - 1 - exponent as i64) else {
        return value;
    };
    let scale = 10f64.powi(scale_power);
    let scaled = value * scale;
    if !scale.is_finite() || !scaled.is_finite() || scale == 0.0 {
        return value;
    }
    scaled.round() / scale
}

/// The data checks over `bars` for `symbols`. `bars` must be in time order, as
/// `BarReader::read` returns them, and cover the whole file (or at least the lookback).
///
/// The last session is the newest bar time among `bars`. The lookback window starts at the
/// [`DATA_LOOKBACK_SESSIONS`]th newest distinct bar time (or the oldest, when there are fewer).
/// Zero-volume bars must lie in the window. A move is one symbol's close against its previous
/// bar in `bars`, and it counts when the later bar lies in the window. Moves are
/// `(close - previous) / previous`, in exact decimal arithmetic.
#[must_use]
pub fn data_checks(bars: &[Bar], symbols: &[&str]) -> DataChecks {
    let session_times: BTreeSet<u64> = bars.iter().map(|bar| bar.ts_event.as_u64()).collect();
    let last = session_times.iter().next_back().copied();
    let window_start = session_times
        .iter()
        .rev()
        .nth(DATA_LOOKBACK_SESSIONS - 1)
        .or_else(|| session_times.iter().next())
        .copied();
    let in_window = |ts: u64| window_start.is_some_and(|start| ts >= start);

    let present_last: BTreeSet<String> = bars
        .iter()
        .filter(|bar| Some(bar.ts_event.as_u64()) == last)
        .map(symbol_of)
        .collect();
    let wanted: BTreeSet<&str> = symbols.iter().copied().collect();
    let missing_on_last_session = wanted
        .into_iter()
        .filter(|symbol| !present_last.contains(*symbol))
        .map(str::to_owned)
        .collect();

    let zero_volume = bars
        .iter()
        .filter(|bar| in_window(bar.ts_event.as_u64()) && bar.volume.as_decimal().is_zero())
        .map(|bar| ZeroVolume {
            symbol: symbol_of(bar),
            date: session_date(bar.ts_event.as_u64()),
        })
        .collect();

    let mut previous_close: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut large_moves = Vec::new();
    for bar in bars {
        let symbol = symbol_of(bar);
        let close = bar.close.as_decimal();
        let ts = bar.ts_event.as_u64();
        let Some(previous) = previous_close.insert(symbol.clone(), close) else {
            continue;
        };
        if !in_window(ts) {
            continue;
        }
        let Some(pct) = move_pct(previous, close) else {
            continue;
        };
        if pct.abs() > Decimal::from(LARGE_MOVE_PCT) {
            large_moves.push(LargeMove {
                symbol,
                date: session_date(ts),
                move_pct: format!("{pct:.1}"),
            });
        }
    }

    DataChecks {
        last_session: last.map(session_date),
        missing_on_last_session,
        zero_volume,
        large_moves,
    }
}

/// `(close - previous) / previous` as a percentage. `None` for a zero previous price or on
/// overflow.
fn move_pct(previous: Decimal, close: Decimal) -> Option<Decimal> {
    if previous.is_zero() {
        return None;
    }
    close
        .checked_sub(previous)?
        .checked_mul(Decimal::from(100))?
        .checked_div(previous)
}

/// The symbol of a bar as an owned string.
fn symbol_of(bar: &Bar) -> String {
    bar.bar_type.instrument_id().symbol.as_str().to_owned()
}

/// The UTC date of a bar time, `YYYY-MM-DD`.
fn session_date(ts: u64) -> String {
    i64::try_from(ts).map_or_else(
        |_| String::new(),
        |nanos| {
            DateTime::<Utc>::from_timestamp_nanos(nanos)
                .format("%Y-%m-%d")
                .to_string()
        },
    )
}

/// The UTC month of `ts`, `YYYY-MM`.
fn month_of(ts: UnixNanos) -> Result<String, StoreError> {
    let nanos = i64::try_from(ts.as_u64())
        .map_err(|_| StoreError::Data(format!("time {} out of range", ts.as_u64())))?;
    Ok(DateTime::<Utc>::from_timestamp_nanos(nanos)
        .format("%Y-%m")
        .to_string())
}

/// The bars of `symbol` as a `BarPoint` series, in the order they were given.
fn points(bars: &[Bar], symbol: &str) -> Vec<BarPoint> {
    bars.iter()
        .filter(|bar| bar.bar_type.instrument_id().symbol.as_str() == symbol)
        .map(|bar| BarPoint {
            ts: bar.ts_event,
            high: bar.high.as_f64(),
            low: bar.low.as_f64(),
            close: bar.close.as_f64(),
        })
        .collect()
}

/// The data section and the benchmark's regime, from one read of the newest bar file for the
/// config's symbols and benchmark. No bar file gives no data and an `Unknown` regime.
fn read_data(args: &ReportArgs<'_>) -> Result<(Option<DataReport>, Regime), EvalError> {
    let reader = BarReader::open(args.bars_dir)?;
    let Some(bar_file) = reader.snapshot_file_name()? else {
        return Ok((None, Regime::Unknown));
    };
    let mut wanted: BTreeSet<&str> = args.symbols.iter().map(String::as_str).collect();
    wanted.insert(args.benchmark);
    let list: Vec<&str> = wanted.into_iter().collect();
    let bars = reader.read(&list, UnixNanos::from(0), UnixNanos::from(i64::MAX as u64))?;
    let checks = data_checks(&bars, &list);
    let benchmark_bars = points(&bars, args.benchmark);
    let current = regime(&benchmark_bars, args.now);
    Ok((Some(DataReport { bar_file, checks }), current))
}

/// `filled / intended` as normalised decimal text (`"1"` for a full fill); `"0"` when the
/// division is impossible, such as a zero intended quantity.
fn filled_fraction(filled: Decimal, intended: Decimal) -> String {
    filled.checked_div(intended).map_or_else(
        || "0".to_owned(),
        |fraction| fraction.normalize().to_string(),
    )
}

/// A strategy version as stored.
struct VersionRow {
    id: i64,
    git_sha: String,
    config_hash: String,
}

/// Every strategy version, oldest first.
async fn read_versions<C: GenericClient>(client: &C) -> Result<Vec<VersionRow>, StoreError> {
    let rows = client
        .query(
            "SELECT id, git_sha, encode(config_hash, 'hex') AS config_hash \
             FROM strategy_version ORDER BY id",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(VersionRow {
            id: row.try_get("id")?,
            git_sha: row.try_get("git_sha")?,
            config_hash: row.try_get("config_hash")?,
        });
    }
    Ok(out)
}

/// `(strategy_version_id, action, count)` of the visible decisions.
async fn read_action_counts<C: GenericClient>(
    client: &C,
) -> Result<Vec<(i64, String, i64)>, StoreError> {
    let rows = client
        .query(
            "SELECT d.strategy_version_id, d.action, count(*)::bigint AS n \
             FROM decision d \
             JOIN decision_holdout h ON h.decision_id = d.id AND NOT h.hidden \
             GROUP BY d.strategy_version_id, d.action \
             ORDER BY d.strategy_version_id, d.action",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((
            row.try_get("strategy_version_id")?,
            row.try_get("action")?,
            row.try_get("n")?,
        ));
    }
    Ok(out)
}

/// `(strategy_version_id, metric, value)` for every visible decision's newest score row of
/// each metric (newest by `eval_score.id`, whatever its `eval_version`). Rows come in decision
/// id order within each version and metric, which [`aggregate`] relies on.
async fn read_decision_scores<C: GenericClient>(
    client: &C,
) -> Result<Vec<(i64, String, f64)>, StoreError> {
    let rows = client
        .query(
            "WITH visible AS ( \
                 SELECT d.id, d.strategy_version_id \
                 FROM decision d \
                 JOIN decision_holdout h ON h.decision_id = d.id AND NOT h.hidden \
             ), \
             newest AS ( \
                 SELECT DISTINCT ON (e.subject_id, e.metric) \
                     e.subject_id, e.metric, e.value::float8 AS value \
                 FROM eval_score e \
                 JOIN visible v ON v.id = e.subject_id \
                 WHERE e.subject_type = 'decision' \
                 ORDER BY e.subject_id, e.metric, e.id DESC \
             ) \
             SELECT v.strategy_version_id, n.metric, n.value \
             FROM newest n \
             JOIN visible v ON v.id = n.subject_id \
             ORDER BY v.strategy_version_id, n.metric, n.subject_id",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push((
            row.try_get("strategy_version_id")?,
            row.try_get("metric")?,
            row.try_get("value")?,
        ));
    }
    Ok(out)
}

/// A label queue row before its execution figures are judged.
struct LabelRow {
    decision_id: i64,
    symbol: String,
    action: String,
    reason_code: String,
    month: String,
    features: Value,
    intended: Decimal,
    orders: i64,
    filled: Decimal,
    shortfall_bps: Option<f64>,
}

/// The newest visible, non-hold decisions that have at least one order, newest first, at most
/// [`LABEL_QUEUE_MAX`]. Selects only how each decision was made and executed.
async fn read_label_queue<C: GenericClient>(client: &C) -> Result<Vec<LabelEntry>, EvalError> {
    let limit = i64::try_from(LABEL_QUEUE_MAX).unwrap_or(i64::MAX);
    let rows = client
        .query(
            "SELECT d.id, d.symbol, d.action, d.reason_code, \
                    to_char(d.event_ts AT TIME ZONE 'UTC', 'YYYY-MM') AS month, \
                    d.features, d.intended_qty::numeric AS intended, \
                    (SELECT count(DISTINCT o.client_order_id) FROM order_event o \
                      WHERE o.decision_id = d.id) AS orders, \
                    (SELECT COALESCE(sum(f.qty), 0)::numeric FROM fill f \
                      WHERE f.client_order_id IN ( \
                          SELECT o.client_order_id FROM order_event o \
                          WHERE o.decision_id = d.id)) AS filled, \
                    (SELECT e.value::float8 FROM eval_score e \
                      WHERE e.subject_type = 'decision' AND e.subject_id = d.id \
                        AND e.metric = $2 \
                      ORDER BY e.id DESC LIMIT 1) AS shortfall_bps \
             FROM decision d \
             JOIN decision_holdout h ON h.decision_id = d.id AND NOT h.hidden \
             WHERE d.action <> 'hold' \
               AND EXISTS (SELECT 1 FROM order_event o WHERE o.decision_id = d.id) \
             ORDER BY d.id DESC \
             LIMIT $1",
            &[&limit, &METRIC_SHORTFALL_BPS],
        )
        .await
        .map_err(StoreError::from)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let label = LabelRow {
            decision_id: row.try_get("id").map_err(StoreError::from)?,
            symbol: row.try_get("symbol").map_err(StoreError::from)?,
            action: row.try_get("action").map_err(StoreError::from)?,
            reason_code: row.try_get("reason_code").map_err(StoreError::from)?,
            month: row.try_get("month").map_err(StoreError::from)?,
            features: row.try_get("features").map_err(StoreError::from)?,
            intended: row.try_get("intended").map_err(StoreError::from)?,
            orders: row.try_get("orders").map_err(StoreError::from)?,
            filled: row.try_get("filled").map_err(StoreError::from)?,
            shortfall_bps: row.try_get("shortfall_bps").map_err(StoreError::from)?,
        };
        out.push(LabelEntry {
            decision_id: label.decision_id,
            symbol: label.symbol,
            action: label.action,
            reason_code: label.reason_code,
            month: label.month,
            features: label.features,
            execution: Execution {
                orders: label.orders,
                filled_fraction: filled_fraction(label.filled, label.intended),
                shortfall_bps: label.shortfall_bps,
            },
        });
    }
    Ok(out)
}

/// Groups `(version, action, count)` rows by version.
fn group_counts(rows: Vec<(i64, String, i64)>) -> BTreeMap<i64, BTreeMap<String, i64>> {
    let mut grouped: BTreeMap<i64, BTreeMap<String, i64>> = BTreeMap::new();
    for (version, action, count) in rows {
        grouped.entry(version).or_default().insert(action, count);
    }
    grouped
}

/// Groups `(version, metric, value)` rows by version and metric.
fn group_scores(rows: Vec<(i64, String, f64)>) -> BTreeMap<i64, BTreeMap<String, Vec<f64>>> {
    let mut grouped: BTreeMap<i64, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    for (version, metric, value) in rows {
        grouped
            .entry(version)
            .or_default()
            .entry(metric)
            .or_default()
            .push(value);
    }
    grouped
}

/// Each gate of a version as its status text, except the holdout gate ([`HOLDOUT_GATE`]). The
/// error is returned, not swallowed: the caller turns it into a note.
async fn gate_statuses<C: GenericClient>(
    client: &C,
    version: i64,
    gates: &Gates,
) -> Result<BTreeMap<String, GateStatus>, EvalError> {
    let inputs = load_inputs(client, StrategyVersionId(version), gates.clone()).await?;
    Ok(build_report(&inputs)
        .gates
        .iter()
        .filter(|gate| gate.name != HOLDOUT_GATE)
        .map(|gate| (gate.name.to_owned(), gate.status))
        .collect())
}
