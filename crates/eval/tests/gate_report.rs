//! Acceptance tests for the gate report: `build_report` as pure values, `TrialSeries::from_stored`,
//! and `load_inputs` / `store_report` against a real Postgres (`TEST_DATABASE_URL`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::path::Path;

use nisse_core::nautilus::UnixNanos;
use nisse_core::{StrategyVersionId, Trial, TrialKind};
use nisse_eval::gate_report::{
    EVAL_VERSION, GateReport, GateResult, GateStatus, ReportInputs, TrialSeries, build_report,
    load_inputs, store_report,
};
use nisse_eval::gates::Gates;
use nisse_eval::stats::{dsr, moments, pbo};
use nisse_store::tables::scoring::TrialWithResult;
use nisse_store::tables::{trial_registry, trial_result};
use nisse_store::testing::ScratchDb;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z, in nanoseconds.
const T: u64 = 1_767_600_000_123_456_000;

const NAMES: [&str; 7] = [
    "trial_budget",
    "deflated_sharpe",
    "pbo",
    "holdout",
    "replay",
    "risk_breaches",
    "tracking",
];

fn gates() -> Gates {
    Gates::load(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config/eval/gates.json"
    )))
    .unwrap()
}

/// `n` returns alternating `mean + wobble` and `mean - wobble`.
fn drift(n: usize, mean: f64, wobble: f64) -> Vec<f64> {
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                mean + wobble
            } else {
                mean - wobble
            }
        })
        .collect()
}

/// A backtest trial over the snapshot "snap" and the range 0..1000.
fn series(trial_id: i64, version: i64, holdout: bool, returns: Vec<f64>) -> TrialSeries {
    TrialSeries {
        trial_id,
        strategy_version_id: StrategyVersionId(version),
        holdout,
        returns: Some(returns),
        data_snapshot_id: "snap".to_owned(),
        requested_start: 0,
        requested_end: 1000,
        summary: json!({"sharpe": 1.0}),
    }
}

fn inputs(version: i64, trials: Vec<TrialSeries>, lifetime: u64) -> ReportInputs {
    ReportInputs {
        version: StrategyVersionId(version),
        gates: gates(),
        lifetime_trials: lifetime,
        holdout_looks: 0,
        trials,
        replay: (0, 0),
        risk_breaches: 0,
        live: None,
    }
}

fn find<'a>(report: &'a GateReport, name: &str) -> &'a GateResult {
    report.gates.iter().find(|gate| gate.name == name).unwrap()
}

fn status(report: &GateReport, name: &str) -> GateStatus {
    find(report, name).status
}

// ---- build_report ---------------------------------------------------------------------------

#[test]
fn no_trials_leaves_the_statistical_gates_not_evaluated() {
    let report = build_report(&inputs(1, vec![], 0));
    let names: Vec<&str> = report.gates.iter().map(|g| g.name).collect();
    assert_eq!(names, NAMES);
    assert_eq!(status(&report, "trial_budget"), GateStatus::Pass);
    for name in ["deflated_sharpe", "pbo", "holdout", "replay", "tracking"] {
        assert_eq!(status(&report, name), GateStatus::NotEvaluated, "{name}");
    }
    assert_eq!(status(&report, "risk_breaches"), GateStatus::Pass);
    assert!(!report.all_pass());
    assert!(report.none_failed());
    assert_eq!(report.candidate_trial_id, None);
}

#[test]
fn one_strong_candidate_passes_the_deflated_sharpe() {
    let r = drift(500, 0.001, 0.010);
    let sr = moments::sharpe(&r).unwrap();
    let report = build_report(&inputs(1, vec![series(1, 1, false, r.clone())], 1));
    let gate = find(&report, "deflated_sharpe");
    assert_eq!(gate.status, GateStatus::Pass);
    assert_eq!(gate.value, dsr::deflated_sharpe(&r, &[sr], 1));
    assert_eq!(report.candidate_trial_id, Some(1));
    assert_eq!(status(&report, "pbo"), GateStatus::NotEvaluated);
}

#[test]
fn many_lifetime_trials_lower_the_deflated_sharpe_and_a_weak_candidate_fails() {
    let r = drift(500, 0.001, 0.010);
    let others = [
        drift(500, 0.0005, 0.010),
        drift(500, 0.002, 0.010),
        drift(500, 0.0, 0.010),
    ];
    let mut trials: Vec<TrialSeries> = others
        .iter()
        .enumerate()
        .map(|(i, r)| series(i as i64 + 1, 2, false, r.clone()))
        .collect();
    trials.push(series(4, 1, false, r.clone()));

    let report = build_report(&inputs(1, trials.clone(), 60));
    let sharpes: Vec<f64> = others
        .iter()
        .chain(std::iter::once(&r))
        .map(|s| moments::sharpe(s).unwrap())
        .collect();
    let value = find(&report, "deflated_sharpe").value;
    assert_eq!(value, dsr::deflated_sharpe(&r, &sharpes, 60));

    let single = dsr::deflated_sharpe(&r, &[moments::sharpe(&r).unwrap()], 1).unwrap();
    assert!(value.unwrap() < single);

    let weak = drift(500, 0.0001, 0.010);
    trials.pop();
    trials.push(series(4, 1, false, weak));
    let report = build_report(&inputs(1, trials, 60));
    assert_eq!(
        status(&report, "deflated_sharpe"),
        GateStatus::Fail,
        "{:?}",
        find(&report, "deflated_sharpe")
    );
}

#[test]
fn more_than_the_trial_budget_fails_the_budget_gate() {
    let report = build_report(&inputs(1, vec![], 101));
    let gate = find(&report, "trial_budget");
    assert_eq!(gate.status, GateStatus::Fail);
    assert_eq!(gate.value, Some(101.0));
    assert_eq!(gate.threshold, Some(100.0));
}

#[test]
fn a_candidate_without_returns_is_not_evaluated_for_the_deflated_sharpe() {
    let candidate = TrialSeries {
        returns: None,
        ..series(1, 1, false, vec![])
    };
    let report = build_report(&inputs(1, vec![candidate], 1));
    let gate = find(&report, "deflated_sharpe");
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert!(gate.note.contains("returns"), "{}", gate.note);
    assert_eq!(report.candidate_trial_id, Some(1));
}

#[test]
fn pbo_with_a_dominated_alternative_passes_at_zero() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0, 0.010);
    // Alternative (version 2) has the lower trial id; the candidate is version 1's only trial.
    let trials = vec![
        series(1, 2, false, alt.clone()),
        series(2, 1, false, cand.clone()),
    ];
    let report = build_report(&inputs(1, trials, 2));
    let gate = find(&report, "pbo");
    // Series in ascending strategy version id: the candidate (version 1), then version 2.
    assert_eq!(gate.value, pbo::pbo(&[cand.as_slice(), alt.as_slice()]));
    assert_eq!(gate.status, GateStatus::Pass);
    assert_eq!(gate.value, Some(0.0));
}

#[test]
fn pbo_of_two_identical_series_is_one_and_fails() {
    let cand = drift(500, 0.001, 0.010);
    let trials = vec![
        series(1, 2, false, cand.clone()),
        series(2, 1, false, cand.clone()),
    ];
    let report = build_report(&inputs(1, trials, 2));
    let gate = find(&report, "pbo");
    assert_eq!(gate.value, Some(1.0));
    assert_eq!(gate.status, GateStatus::Fail);
    assert_eq!(gate.value, pbo::pbo(&[cand.as_slice(), cand.as_slice()]));
}

#[test]
fn pbo_leaves_out_trials_over_other_data_or_another_length() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0, 0.010);
    let other_snapshot = TrialSeries {
        data_snapshot_id: "other".to_owned(),
        ..series(1, 2, false, alt.clone())
    };
    let other_start = TrialSeries {
        requested_start: 1,
        ..series(1, 2, false, alt.clone())
    };
    let other_end = TrialSeries {
        requested_end: 2000,
        ..series(1, 2, false, alt)
    };
    let other_length = series(1, 2, false, drift(499, 0.0, 0.010));
    for other in [other_snapshot, other_start, other_end, other_length] {
        let trials = vec![other, series(2, 1, false, cand.clone())];
        let report = build_report(&inputs(1, trials, 2));
        assert_eq!(status(&report, "pbo"), GateStatus::NotEvaluated);
    }
}

#[test]
fn pbo_counts_only_the_newest_trial_of_each_other_version() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0, 0.010);
    // Version 2 has a dominated alternative (trial 1) and, newer, a copy of the candidate
    // (trial 3). Only the newer one is in the matrix, so the PBO is 1.
    let trials = vec![
        series(1, 2, false, alt),
        series(2, 1, false, cand.clone()),
        series(3, 2, false, cand.clone()),
    ];
    let report = build_report(&inputs(1, trials, 3));
    assert_eq!(
        find(&report, "pbo").value,
        pbo::pbo(&[cand.as_slice(), cand.as_slice()])
    );
    assert_eq!(status(&report, "pbo"), GateStatus::Fail);
}

// ---- holdout --------------------------------------------------------------------------------

fn holdout_status(trials: Vec<TrialSeries>, looks: u64) -> GateStatus {
    let mut i = inputs(1, trials, 2);
    i.holdout_looks = looks;
    status(&build_report(&i), "holdout")
}

#[test]
fn holdout_runs_are_never_the_candidate_nor_in_the_statistics() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0, 0.010);
    let trials = vec![
        series(1, 1, false, cand.clone()),
        series(2, 2, false, alt.clone()),
        series(3, 1, true, cand.clone()),
        series(4, 2, true, cand.clone()),
    ];
    let report = build_report(&inputs(1, trials, 4));
    assert_eq!(report.candidate_trial_id, Some(1));

    let sharpes = [
        moments::sharpe(&cand).unwrap(),
        moments::sharpe(&alt).unwrap(),
    ];
    assert_eq!(
        find(&report, "deflated_sharpe").value,
        dsr::deflated_sharpe(&cand, &sharpes, 4)
    );
    assert_eq!(
        find(&report, "pbo").value,
        pbo::pbo(&[cand.as_slice(), alt.as_slice()])
    );
}

#[test]
fn holdout_gate_follows_the_looks_and_the_version_s_own_holdout_run() {
    let cand = drift(500, 0.001, 0.010);
    let own = series(2, 1, true, cand.clone());
    let other = series(2, 2, true, cand.clone());
    let base = vec![series(1, 1, false, cand)];

    assert_eq!(holdout_status(base.clone(), 0), GateStatus::NotEvaluated);

    let mut with_own = base.clone();
    with_own.push(own);
    assert_eq!(holdout_status(with_own.clone(), 3), GateStatus::Pass);
    assert_eq!(holdout_status(with_own, 11), GateStatus::Fail);

    let mut with_other = base;
    with_other.push(other);
    assert_eq!(holdout_status(with_other, 1), GateStatus::NotEvaluated);
}

// ---- replay and risk ------------------------------------------------------------------------

#[test]
fn replay_gate_judges_failures_among_scored_decisions() {
    let replay = |replay: (u64, u64)| {
        let mut i = inputs(1, vec![], 0);
        i.replay = replay;
        status(&build_report(&i), "replay")
    };
    assert_eq!(replay((0, 0)), GateStatus::NotEvaluated);
    assert_eq!(replay((0, 5)), GateStatus::Pass);
    assert_eq!(replay((2, 5)), GateStatus::Fail);
}

#[test]
fn risk_breaches_must_be_zero() {
    let breaches = |n: u64| {
        let mut i = inputs(1, vec![], 0);
        i.risk_breaches = n;
        status(&build_report(&i), "risk_breaches")
    };
    assert_eq!(breaches(0), GateStatus::Pass);
    assert_eq!(breaches(1), GateStatus::Fail);
}

// ---- tracking -------------------------------------------------------------------------------

#[test]
fn tracking_is_not_evaluated_without_live_returns_and_judged_with_them() {
    let r = drift(500, 0.001, 0.010);
    let mut i = inputs(1, vec![series(1, 1, false, r.clone())], 1);
    assert_eq!(
        status(&build_report(&i), "tracking"),
        GateStatus::NotEvaluated
    );

    i.live = Some((r.clone(), r.clone()));
    let report = build_report(&i);
    let gate = find(&report, "tracking");
    assert_eq!(gate.status, GateStatus::Pass);
    assert!(gate.value.unwrap().abs() < 1e-12);

    // Live returns alternate out of phase with the backtest: a large, steady gap.
    i.live = Some((drift(500, 0.01, -0.010), r.clone()));
    assert_eq!(status(&build_report(&i), "tracking"), GateStatus::Fail);

    i.live = Some((r[..499].to_vec(), r));
    assert_eq!(
        status(&build_report(&i), "tracking"),
        GateStatus::NotEvaluated
    );
}

// ---- info -----------------------------------------------------------------------------------

#[test]
fn info_carries_the_haircut_intervals_and_folds() {
    let r = drift(500, 0.001, 0.010);
    let report = build_report(&inputs(1, vec![series(1, 1, false, r.clone())], 1));
    let info = &report.info;
    let sharpe = moments::sharpe(&r).unwrap() * 252f64.sqrt();

    let haircut = info["haircut_sharpe"].as_f64().unwrap();
    assert!(
        (haircut - 0.5 * sharpe).abs() < 1e-9,
        "{haircut} vs {sharpe}"
    );
    assert!((info["sharpe"].as_f64().unwrap() - sharpe).abs() < 1e-9);

    let interval = info["sharpe_interval"].as_array().unwrap();
    let (lo, hi) = (interval[0].as_f64().unwrap(), interval[1].as_f64().unwrap());
    assert!(
        lo <= sharpe && sharpe <= hi,
        "[{lo}, {hi}] does not hold {sharpe}"
    );

    assert_eq!(info["fold_sharpes"].as_array().unwrap().len(), 8);
    assert_eq!(info["lifetime_trials"], json!(1));
    assert_eq!(info["candidate_trial_id"], json!(1));
    assert_eq!(info["summary"], json!({"sharpe": 1.0}));
}

#[test]
fn a_negative_sharpe_candidate_has_a_zero_haircut() {
    let r = drift(500, -0.001, 0.010);
    let report = build_report(&inputs(1, vec![series(1, 1, false, r)], 1));
    assert_eq!(report.info["haircut_sharpe"].as_f64(), Some(0.0));
}

#[test]
fn info_is_null_without_a_candidate() {
    let report = build_report(&inputs(1, vec![], 0));
    for key in [
        "summary",
        "sharpe",
        "sortino",
        "expectancy",
        "max_drawdown",
        "sharpe_interval",
        "sortino_interval",
        "expectancy_interval",
        "fold_sharpes",
        "haircut_sharpe",
        "candidate_trial_id",
    ] {
        assert!(report.info[key].is_null(), "{key} is not null");
    }
}

// ---- determinism and JSON -------------------------------------------------------------------

#[test]
fn build_report_is_deterministic_and_the_json_has_the_documented_keys() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0, 0.010);
    let i = inputs(
        1,
        vec![series(1, 2, false, alt), series(2, 1, false, cand)],
        2,
    );
    let report = build_report(&i);
    assert_eq!(report, build_report(&i));

    let json = report.to_json();
    let text = serde_json::to_string(&json).unwrap();
    assert!(!text.contains("NaN"));
    for key in [
        "strategy_version_id",
        "candidate_trial_id",
        "all_pass",
        "gates",
        "info",
    ] {
        assert!(json.get(key).is_some(), "missing {key}");
    }
    let gates = json["gates"].as_array().unwrap();
    assert_eq!(gates.len(), 7);
    for gate in gates {
        for key in ["name", "status", "value", "threshold", "note"] {
            assert!(gate.get(key).is_some(), "gate misses {key}");
        }
    }
}

/// Every gate passes: a strong candidate, a dominated alternative, a holdout run of the
/// version, replay (0, 3), identical live and backtest returns.
fn passing_inputs() -> ReportInputs {
    let cand = drift(500, 0.01, 0.010);
    let alt = drift(500, 0.0, 0.010);
    let mut i = inputs(
        1,
        vec![
            series(1, 2, false, alt),
            series(2, 1, false, cand.clone()),
            series(3, 1, true, cand.clone()),
        ],
        3,
    );
    i.holdout_looks = 1;
    i.replay = (0, 3);
    i.live = Some((cand.clone(), cand));
    i
}

#[test]
fn all_pass_holds_only_when_every_gate_passes() {
    let base = passing_inputs();
    let report = build_report(&base);
    for gate in &report.gates {
        assert_eq!(
            gate.status,
            GateStatus::Pass,
            "{}: {}",
            gate.name,
            gate.note
        );
    }
    assert!(report.all_pass());
    assert!(report.none_failed());

    // Each flip breaks exactly the gate named, and only that one must be non-passing.
    type Flip = fn(&mut ReportInputs);
    let flips: [(&str, Flip); 7] = [
        ("trial_budget", |i| i.lifetime_trials = 101),
        ("deflated_sharpe", |i| i.lifetime_trials = 10),
        ("pbo", |i| i.trials[0].returns = i.trials[1].returns.clone()),
        ("holdout", |i| i.holdout_looks = 11),
        ("replay", |i| i.replay = (1, 3)),
        ("risk_breaches", |i| i.risk_breaches = 1),
        ("tracking", |i| {
            i.live = Some((drift(500, 0.01, -0.010), drift(500, 0.01, 0.010)));
        }),
    ];
    for (name, flip) in flips {
        let mut flipped = passing_inputs();
        flip(&mut flipped);
        let report = build_report(&flipped);
        assert!(!report.all_pass(), "{name} flip still all pass");
        assert_ne!(
            status(&report, name),
            GateStatus::Pass,
            "{name} still passes"
        );
    }
}

// ---- TrialSeries::from_stored ---------------------------------------------------------------

fn stored(result: Value) -> TrialWithResult {
    TrialWithResult {
        trial_id: 7,
        strategy_version_id: StrategyVersionId(3),
        holdout: false,
        result,
        ts: UnixNanos::from(T),
    }
}

#[test]
fn a_complete_stored_result_gives_its_returns_and_fields() {
    let series = TrialSeries::from_stored(&stored(json!({
        "summary": {"sharpe": 1.0},
        "returns": [0.01, -0.02],
        "returns_complete": true,
        "data_snapshot_id": "abc",
        "requested_start": 5,
        "requested_end": 9,
    })));
    assert_eq!(series.trial_id, 7);
    assert_eq!(series.strategy_version_id, StrategyVersionId(3));
    assert!(!series.holdout);
    assert_eq!(series.returns, Some(vec![0.01, -0.02]));
    assert_eq!(series.data_snapshot_id, "abc");
    assert_eq!((series.requested_start, series.requested_end), (5, 9));
    assert_eq!(series.summary, json!({"sharpe": 1.0}));
}

#[test]
fn an_incomplete_or_null_returns_array_gives_none() {
    let with_null = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01, null], "returns_complete": true,
    })));
    assert_eq!(with_null.returns, None);

    let incomplete = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01, 0.02], "returns_complete": false,
    })));
    assert_eq!(incomplete.returns, None);
}

#[test]
fn an_older_result_without_returns_is_read_with_defaults() {
    let older = TrialSeries::from_stored(&stored(json!({"summary": {}})));
    assert_eq!(older.returns, None);
    assert_eq!(older.data_snapshot_id, "");
    assert_eq!((older.requested_start, older.requested_end), (0, 0));
    assert_eq!(older.summary, json!({}));
}

// ---- store ----------------------------------------------------------------------------------

/// Inserts a strategy version whose git SHA and config hash are unique to `n` (1-9).
async fn version(client: &Client, n: u8) -> StrategyVersionId {
    let id: i64 = client
        .query_one(
            "INSERT INTO strategy_version (git_sha, config_hash, params, event_ts) \
             VALUES ($1, $2, '{}', '2026-01-02T00:00:00Z') RETURNING id",
            &[&n.to_string().repeat(40), &vec![n; 32]],
        )
        .await
        .unwrap()
        .get(0);
    StrategyVersionId(id)
}

/// Registers a trial and returns its id.
async fn register(
    client: &Client,
    version: StrategyVersionId,
    kind: TrialKind,
    registry_result: Option<Value>,
) -> i64 {
    trial_registry::insert(
        client,
        &Trial {
            strategy_version_id: version,
            kind,
            data_start: UnixNanos::from(T),
            data_end: UnixNanos::from(T + 1_000),
            result: registry_result,
            ts: UnixNanos::from(T),
        },
    )
    .await
    .unwrap()
}

/// A backtest result as `backtest_job` stores it, with the given returns.
fn stored_result(returns: Value, holdout: bool) -> Value {
    json!({
        "summary": {},
        "returns": returns,
        "returns_complete": true,
        "holdout": holdout,
        "data_snapshot_id": "snap",
        "requested_start": 0,
        "requested_end": 1000,
    })
}

#[tokio::test]
async fn load_inputs_counts_every_trial_and_reads_backtest_results_only() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v1 = version(&client, 1).await;
    let v2 = version(&client, 2).await;

    let a = register(&client, v1, TrialKind::Backtest, None).await;
    trial_result::insert(
        &client,
        a,
        &stored_result(json!([0.01, -0.02]), false),
        UnixNanos::from(T),
    )
    .await
    .unwrap();
    let b = register(&client, v2, TrialKind::Backtest, None).await;
    trial_result::insert(
        &client,
        b,
        &stored_result(json!([0.0, 0.01]), false),
        UnixNanos::from(T),
    )
    .await
    .unwrap();
    let h = register(
        &client,
        v1,
        TrialKind::Backtest,
        Some(json!({"holdout": true})),
    )
    .await;
    trial_result::insert(
        &client,
        h,
        &stored_result(json!([0.01, 0.01]), true),
        UnixNanos::from(T),
    )
    .await
    .unwrap();
    // A shadow run with a result, and a backtest still running (no result): counted, not read.
    let s = register(&client, v1, TrialKind::Shadow, None).await;
    trial_result::insert(
        &client,
        s,
        &stored_result(json!([0.01, 0.01]), false),
        UnixNanos::from(T),
    )
    .await
    .unwrap();
    register(&client, v1, TrialKind::Backtest, None).await;

    let inputs = load_inputs(&client, v1, gates()).await.unwrap();
    assert_eq!(inputs.version, v1);
    assert_eq!(inputs.lifetime_trials, 5);
    assert_eq!(inputs.holdout_looks, 1);
    let ids: Vec<i64> = inputs.trials.iter().map(|t| t.trial_id).collect();
    assert_eq!(ids, vec![a, b, h]);
    let holdouts: Vec<bool> = inputs.trials.iter().map(|t| t.holdout).collect();
    assert_eq!(holdouts, vec![false, false, true]);
    assert_eq!(inputs.trials[0].returns, Some(vec![0.01, -0.02]));
    assert_eq!(inputs.replay, (0, 0));
    assert_eq!(inputs.risk_breaches, 0);
    assert_eq!(inputs.live, None);

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn store_report_writes_status_and_value_rows_for_the_version() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;

    let cand = drift(500, 0.001, 0.010);
    let report = build_report(&inputs(v.0, vec![series(1, v.0, false, cand)], 1));
    store_report(&client, &report, UnixNanos::from(T))
        .await
        .unwrap();

    let rows = client
        .query(
            "SELECT subject_type, subject_id, metric, value, eval_version \
             FROM eval_score ORDER BY id",
            &[],
        )
        .await
        .unwrap();
    let got: Vec<(String, i64, String, f64, String)> = rows
        .iter()
        .map(|row| {
            (
                row.get("subject_type"),
                row.get("subject_id"),
                row.get("metric"),
                row.get("value"),
                row.get("eval_version"),
            )
        })
        .collect();

    let mut expected: Vec<(String, f64)> = Vec::new();
    for gate in &report.gates {
        let score = match gate.status {
            GateStatus::Pass => 1.0,
            GateStatus::Fail => 0.0,
            GateStatus::NotEvaluated => -1.0,
        };
        expected.push((format!("gate_{}_status", gate.name), score));
        if let Some(value) = gate.value {
            expected.push((format!("gate_{}", gate.name), value));
        }
    }
    assert_eq!(got.len(), expected.len());
    assert_eq!(got.iter().filter(|r| r.2.ends_with("_status")).count(), 7);
    for ((subject_type, subject_id, metric, value, eval_version), (name, score)) in
        got.iter().zip(&expected)
    {
        assert_eq!(subject_type, "strategy_version");
        assert_eq!(*subject_id, v.0);
        assert_eq!(eval_version, EVAL_VERSION);
        assert_eq!(metric, name);
        assert_eq!(value, score, "{metric}");
    }

    drop(client);
    db.drop_db().await.unwrap();
}
