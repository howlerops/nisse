//! The nightly scoring job, part one: the decision holdout and the outcome windows. Rules
//! in `docs/phases/phase-5.md`, "Trade-level scores".

use std::collections::{BTreeMap, BTreeSet};

use nisse_bars::BarReader;
use nisse_core::{
    DecisionId, EvalScore, SubjectType,
    nautilus::{Bar, UnixNanos},
};
use nisse_store::tables::{
    decision_holdout::{self, Assignment},
    eval_score,
    outcome_window::{self, OutcomeRow, PendingDecision},
};
use sha2::{Digest, Sha256};
use tokio_postgres::GenericClient;

use crate::{
    EvalError,
    gates::Gates,
    outcome::{BarPoint, OUTCOME_HORIZONS, base_too_old, decision_quality, outcome},
};

/// The `eval_version` written with every score this job produces.
pub const EVAL_VERSION: &str = "score-v1";
/// Shortest salt accepted, in bytes.
pub const MIN_SALT_BYTES: usize = 16;
/// Share of decisions hidden from agents, in percent.
pub const HIDDEN_PERCENT: u64 = 25;

/// Decisions read per holdout query.
const HOLDOUT_BATCH: i64 = 1_000;
/// A limit above any number of pending decisions a nightly run sees, so one query fetches all
/// of them (`pending` returns the same rows again when they cannot be scored, so it is not
/// paged).
const ALL_PENDING: i64 = 1_000_000;

/// Whether decision `id` is hidden from agents: the first 8 bytes of `SHA-256(salt ‖ id as decimal text)`, read as a big-endian integer, modulo 100, is below `HIDDEN_PERCENT`. Pure and stable: the same salt and id always give the same answer.
#[must_use]
pub fn is_hidden(salt: &[u8], id: DecisionId) -> bool {
    let mut digits = [0u8; 20];
    let text = decimal_text(id.0, &mut digits);
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(text);
    let digest: [u8; 32] = hasher.finalize().into();
    let head = u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ]);
    head % 100 < HIDDEN_PERCENT
}

/// The decimal text of `value` written into `buf` from the end, returned as the slice that
/// holds it. An `i64` takes at most 20 bytes with its sign, so `buf` always has room.
fn decimal_text(value: i64, buf: &mut [u8; 20]) -> &[u8] {
    let mut rest = value.unsigned_abs();
    let mut start = buf.len();
    loop {
        start = start.saturating_sub(1);
        buf[start] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if value < 0 {
        start = start.saturating_sub(1);
        buf[start] = b'-';
    }
    &buf[start..]
}

/// What one run of `assign_holdouts` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HoldoutReport {
    /// Decisions that got an assignment in this run.
    pub assigned: u64,
    /// Of those, how many were hidden.
    pub hidden: u64,
}

/// Gives every decision without a `decision_holdout` row its assignment, in batches of 1,000
/// (loop on `unassigned(client, 1000)` until it returns nothing). A salt shorter than
/// `MIN_SALT_BYTES` is `EvalError::InvalidJob` and nothing is written.
pub async fn assign_holdouts<C: GenericClient>(
    client: &C,
    salt: &[u8],
    now: UnixNanos,
) -> Result<HoldoutReport, EvalError> {
    if salt.len() < MIN_SALT_BYTES {
        return Err(EvalError::InvalidJob(format!(
            "holdout salt is {} bytes; at least {MIN_SALT_BYTES} are needed",
            salt.len()
        )));
    }
    let mut report = HoldoutReport::default();
    loop {
        let ids = decision_holdout::unassigned(client, HOLDOUT_BATCH).await?;
        if ids.is_empty() {
            break;
        }
        let rows: Vec<Assignment> = ids
            .iter()
            .map(|&decision_id| Assignment {
                decision_id,
                hidden: is_hidden(salt, decision_id),
            })
            .collect();
        let hidden = rows.iter().filter(|row| row.hidden).count();
        decision_holdout::insert_many(client, &rows, now).await?;
        report.assigned += rows.len() as u64;
        report.hidden += hidden as u64;
    }
    Ok(report)
}

/// What one run of `score_outcomes` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutcomeReport {
    /// Outcome rows written.
    pub written: u64,
    /// Decision-quality scores written.
    pub quality_scores: u64,
    /// Decision-horizon pairs that could not be scored yet (not enough bars, or no bar for the
    /// symbol or the benchmark).
    pub waiting: u64,
    /// Decision-horizon pairs left unscored because the decision was made 20 hours or more after
    /// its bar; see `outcome::MAX_BASE_AGE_NANOS`. Not counted in `waiting`.
    pub base_too_old: u64,
}

/// For each horizon in `OUTCOME_HORIZONS`: every decision still pending for it (all pages) is
/// scored with `outcome(..)` from the bars in `bars`; those that can be computed get an
/// `outcome_window` row and, when the decision has an `expected_vol > 0`, an `eval_score` row
/// (`subject_type` decision, metric `decision_quality_<horizon>`, `eval_version`
/// `EVAL_VERSION`); the rest are counted in `waiting`, or in `base_too_old` when their base bar
/// is too old to ever be scored from these bars.
pub async fn score_outcomes<C: GenericClient>(
    client: &C,
    bars: &BarReader,
    gates: &Gates,
    now: UnixNanos,
) -> Result<OutcomeReport, EvalError> {
    let mut pending: Vec<(u32, Vec<PendingDecision>)> = Vec::with_capacity(OUTCOME_HORIZONS.len());
    for horizon in OUTCOME_HORIZONS {
        let list = outcome_window::pending(client, horizon, ALL_PENDING).await?;
        pending.push((horizon, list));
    }

    let mut symbols: BTreeSet<&str> = BTreeSet::new();
    symbols.insert(gates.benchmark.as_str());
    for (_, list) in &pending {
        for decision in list {
            symbols.insert(decision.symbol.as_str());
        }
    }
    let symbol_list: Vec<&str> = symbols.into_iter().collect();
    let read = bars.read(
        &symbol_list,
        UnixNanos::from(0),
        UnixNanos::from(i64::MAX as u64),
    )?;
    let series = series_by_symbol(&read);

    let no_bars: &[BarPoint] = &[];
    let benchmark = series
        .get(gates.benchmark.as_str())
        .map_or(no_bars, Vec::as_slice);

    let mut report = OutcomeReport::default();
    for (horizon, list) in &pending {
        let horizon = *horizon;
        let metric = format!("decision_quality_{horizon}");
        let scored =
            eval_score::scored_subjects(client, SubjectType::Decision, &metric, EVAL_VERSION)
                .await?;

        let mut rows: Vec<OutcomeRow> = Vec::with_capacity(list.len());
        let mut scores: Vec<EvalScore> = Vec::with_capacity(list.len());
        for decision in list {
            let symbol_bars = series
                .get(decision.symbol.as_str())
                .map_or(no_bars, Vec::as_slice);
            let Some(result) = outcome(symbol_bars, benchmark, decision.ts, horizon) else {
                if base_too_old(symbol_bars, decision.ts) {
                    report.base_too_old += 1;
                } else {
                    report.waiting += 1;
                }
                continue;
            };
            rows.push(OutcomeRow {
                decision_id: decision.id,
                horizon_sessions: horizon,
                fwd_ret: result.fwd_ret,
                bench_ret: result.bench_ret,
                mfe: result.mfe,
                mae: result.mae,
                ts: now,
            });
            let already_scored = scored.binary_search(&decision.id.0).is_ok();
            let quality = match decision.expected_vol {
                Some(expected_vol) if !already_scored => {
                    decision_quality(&result, expected_vol, gates.periods_per_year)
                }
                _ => None,
            };
            if let Some(value) = quality {
                scores.push(EvalScore {
                    subject_id: decision.id.0,
                    subject_type: SubjectType::Decision,
                    metric: metric.clone(),
                    value,
                    eval_version: EVAL_VERSION.to_owned(),
                    ts: now,
                });
            }
        }

        // Scores first: a crash between the two writes leaves the outcome rows pending, and
        // the next run skips the scores that were already written.
        eval_score::insert_many(client, &scores).await?;
        outcome_window::insert_many(client, &rows).await?;
        report.quality_scores += scores.len() as u64;
        report.written += rows.len() as u64;
    }
    Ok(report)
}

/// The bars of each symbol as `BarPoint` series. The reader returns bars in time order, so
/// each series is too.
fn series_by_symbol(bars: &[Bar]) -> BTreeMap<String, Vec<BarPoint>> {
    let mut map: BTreeMap<String, Vec<BarPoint>> = BTreeMap::new();
    for bar in bars {
        let point = BarPoint {
            ts: bar.ts_event,
            high: bar.high.as_f64(),
            low: bar.low.as_f64(),
            close: bar.close.as_f64(),
        };
        let symbol = bar.bar_type.instrument_id().symbol;
        let key = symbol.as_str();
        if let Some(series) = map.get_mut(key) {
            series.push(point);
        } else {
            map.insert(key.to_owned(), vec![point]);
        }
    }
    map
}
