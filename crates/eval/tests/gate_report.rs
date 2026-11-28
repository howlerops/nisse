//! Acceptance tests for the gate report: `build_report` as pure values, `TrialSeries::from_stored`,
//! and `load_inputs` / `store_report` against a real Postgres (`TEST_DATABASE_URL`).

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "integration test")]

use std::path::Path;

use chrono::NaiveDate;
use nisse_core::nautilus::UnixNanos;
use nisse_core::{StrategyVersionId, Trial, TrialKind};
use nisse_eval::gate_report::{
    EVAL_VERSION, GateReport, GateResult, GateStatus, ReportInputs, TrialSeries, build_report,
    load_inputs, pair_tracking, store_report,
};
use nisse_eval::gates::Gates;
use nisse_eval::stats::{dsr, moments, pbo};
use nisse_store::tables::scoring::TrialWithResult;
use nisse_store::tables::{account_equity, trial_registry, trial_result};
use nisse_store::testing::ScratchDb;
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio_postgres::Client;

/// 2026-01-05T09:46:40.123456Z, in nanoseconds.
const T: u64 = 1_767_600_000_123_456_000;

/// 2023-01-01T00:00:00Z, the holdout boundary of `config/eval/gates.json`, in nanoseconds.
const BOUNDARY: u64 = 1_672_531_200_000_000_000;

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

/// `n` configurations with no edge (mean 0) and distinct wobbles.
fn alternatives(n: usize) -> Vec<Vec<f64>> {
    (0..n)
        .map(|k| drift(500, 0.0, 0.010 + k as f64 * 0.0001))
        .collect()
}

/// The cost model "A" every trial of the default helpers runs under.
fn costs_a() -> Value {
    json!({"slippage_bps": "2"})
}

/// A different cost model, "B": zero slippage.
fn costs_b() -> Value {
    json!({"slippage_bps": "0"})
}

/// A usable backtest trial over the snapshot "snap" and the range 0..1000: a clean build, the
/// file's holdout boundary, costs A and symbols SPY, the limits "aaa", starting cash 100000, seed
/// 7 and bar file "bars.parquet", an ordinary run ending the day before the boundary (or a holdout
/// run with its end recorded). A holdout run's sessions are all of its returns.
fn series(trial_id: i64, version: i64, holdout: bool, returns: Vec<f64>) -> TrialSeries {
    let sessions = returns.len() as u64;
    TrialSeries {
        trial_id,
        strategy_version_id: StrategyVersionId(version),
        holdout,
        returns: Some(returns),
        data_snapshot_id: "snap".to_owned(),
        requested_start: 0,
        requested_end: 1000,
        summary: json!({"sharpe": 1.0}),
        dirty_build: Some(false),
        holdout_start: Some("2023-01-01".to_owned()),
        data_end: Some(if holdout {
            BOUNDARY + 1_000
        } else {
            BOUNDARY - 1
        }),
        holdout_sessions: holdout.then_some(sessions),
        final_drawdown_mode: Some("normal".to_owned()),
        costs: costs_a(),
        symbols: json!(["SPY"]),
        limits_hash: Some("aaa".to_owned()),
        starting_cash: Some("100000".to_owned()),
        seed: Some(7),
        bars_file: Some("bars.parquet".to_owned()),
    }
}

/// The configurations of a PBO matrix: one per alternative (versions 2 onward, in trial id
/// order), then the candidate (version 1) as the newest trial.
fn configurations(cand: &[f64], alts: &[Vec<f64>]) -> Vec<TrialSeries> {
    let mut trials: Vec<TrialSeries> = alts
        .iter()
        .enumerate()
        .map(|(k, r)| series(k as i64 + 1, k as i64 + 2, false, r.clone()))
        .collect();
    trials.push(series(alts.len() as i64 + 1, 1, false, cand.to_vec()));
    trials
}

/// The PBO of the candidate and the alternatives in the order the report uses (versions
/// ascending, the candidate's version first).
fn expected_pbo(cand: &[f64], alts: &[Vec<f64>]) -> Option<f64> {
    let mut matrix: Vec<&[f64]> = vec![cand];
    matrix.extend(alts.iter().map(Vec::as_slice));
    pbo::pbo(&matrix)
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
        sent_orders: 5,
        live: None,
    }
}

fn find<'a>(report: &'a GateReport, name: &str) -> &'a GateResult {
    report.gates.iter().find(|gate| gate.name == name).unwrap()
}

fn status(report: &GateReport, name: &str) -> GateStatus {
    find(report, name).status
}

// ---- usable ---------------------------------------------------------------------------------

#[test]
fn a_clean_ordinary_run_ending_before_the_boundary_is_usable() {
    assert_eq!(
        series(1, 1, false, drift(10, 0.0, 0.01)).usable(&gates()),
        Ok(())
    );
}

#[test]
fn a_run_without_a_complete_return_series_is_not_usable() {
    let trial = TrialSeries {
        returns: None,
        ..series(1, 1, false, vec![])
    };
    assert_eq!(trial.usable(&gates()), Err("no complete return series"));
}

#[test]
fn a_dirty_build_is_not_usable() {
    let trial = TrialSeries {
        dirty_build: Some(true),
        ..series(1, 1, false, drift(10, 0.0, 0.01))
    };
    assert_eq!(
        trial.usable(&gates()),
        Err("dirty build, or the build state is not recorded")
    );
}

#[test]
fn a_run_that_does_not_record_its_build_state_is_not_usable() {
    let trial = TrialSeries {
        dirty_build: None,
        ..series(1, 1, false, drift(10, 0.0, 0.01))
    };
    assert_eq!(
        trial.usable(&gates()),
        Err("dirty build, or the build state is not recorded")
    );
}

#[test]
fn a_run_under_a_different_holdout_boundary_is_not_usable() {
    for other in ["2022-01-01", "2099-01-01"] {
        let trial = TrialSeries {
            holdout_start: Some(other.to_owned()),
            ..series(1, 1, false, drift(10, 0.0, 0.01))
        };
        assert_eq!(
            trial.usable(&gates()),
            Err("run under a different holdout boundary, or none recorded"),
            "{other}"
        );
    }
}

#[test]
fn a_run_without_a_recorded_holdout_boundary_is_not_usable() {
    let trial = TrialSeries {
        holdout_start: None,
        ..series(1, 1, false, drift(10, 0.0, 0.01))
    };
    assert_eq!(
        trial.usable(&gates()),
        Err("run under a different holdout boundary, or none recorded")
    );
}

#[test]
fn an_ordinary_run_whose_data_reaches_the_boundary_is_not_usable() {
    for end in [BOUNDARY, BOUNDARY + 1] {
        let trial = TrialSeries {
            data_end: Some(end),
            ..series(1, 1, false, drift(10, 0.0, 0.01))
        };
        assert_eq!(
            trial.usable(&gates()),
            Err("its data reaches into the holdout, or its end is not recorded"),
            "{end}"
        );
    }
}

#[test]
fn an_ordinary_run_without_a_recorded_end_is_not_usable() {
    let trial = TrialSeries {
        data_end: None,
        ..series(1, 1, false, drift(10, 0.0, 0.01))
    };
    assert_eq!(
        trial.usable(&gates()),
        Err("its data reaches into the holdout, or its end is not recorded")
    );
}

#[test]
fn a_holdout_run_is_usable_with_data_after_the_boundary() {
    let trial = TrialSeries {
        data_end: Some(BOUNDARY + 5_000),
        ..series(1, 1, true, drift(10, 0.0, 0.01))
    };
    assert_eq!(trial.usable(&gates()), Ok(()));
}

#[test]
fn a_holdout_run_without_a_recorded_end_is_not_usable() {
    let trial = TrialSeries {
        data_end: None,
        ..series(1, 1, true, drift(10, 0.0, 0.01))
    };
    assert_eq!(
        trial.usable(&gates()),
        Err("its data reaches into the holdout, or its end is not recorded")
    );
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
    // One configuration per version: reruns of one version count once.
    let mut trials: Vec<TrialSeries> = others
        .iter()
        .enumerate()
        .map(|(i, r)| series(i as i64 + 1, i as i64 + 2, false, r.clone()))
        .collect();
    trials.push(series(4, 1, false, r.clone()));

    // Eight lifetime trials, four usable Sharpes: exactly half.
    let report = build_report(&inputs(1, trials.clone(), 8));
    let sharpes: Vec<f64> = others
        .iter()
        .chain(std::iter::once(&r))
        .map(|s| moments::sharpe(s).unwrap())
        .collect();
    let value = find(&report, "deflated_sharpe").value;
    assert_eq!(value, dsr::deflated_sharpe(&r, &sharpes, 8));

    let single = dsr::deflated_sharpe(&r, &[moments::sharpe(&r).unwrap()], 1).unwrap();
    assert!(value.unwrap() < single);

    let weak = drift(500, 0.0001, 0.010);
    trials.pop();
    trials.push(series(4, 1, false, weak));
    let report = build_report(&inputs(1, trials, 8));
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
fn a_candidate_that_cannot_be_used_leaves_the_statistics_not_evaluated() {
    let candidate = TrialSeries {
        returns: None,
        ..series(1, 1, false, vec![])
    };
    let report = build_report(&inputs(1, vec![candidate], 1));
    let gate = find(&report, "deflated_sharpe");
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(
        gate.note,
        "the candidate trial cannot be used: no complete return series"
    );
    assert_eq!(status(&report, "pbo"), GateStatus::NotEvaluated);
    assert_eq!(
        find(&report, "pbo").note,
        "the candidate trial cannot be used: no complete return series"
    );
    assert_eq!(report.candidate_trial_id, Some(1));
    assert_eq!(report.info["candidate_run"]["usable"], json!(false));
}

#[test]
fn an_unusable_candidate_names_the_reason_in_both_statistical_gates() {
    let r = drift(500, 0.001, 0.010);
    let candidate = TrialSeries {
        dirty_build: Some(true),
        ..series(1, 1, false, r)
    };
    let report = build_report(&inputs(1, vec![candidate], 1));
    for name in ["deflated_sharpe", "pbo"] {
        let gate = find(&report, name);
        assert_eq!(gate.status, GateStatus::NotEvaluated, "{name}");
        assert!(
            gate.note
                .starts_with("the candidate trial cannot be used: "),
            "{name}: {}",
            gate.note
        );
        assert!(gate.note.contains("dirty build"), "{name}: {}", gate.note);
    }
    assert_eq!(report.info["candidate_run"]["usable"], json!(false));
    assert_eq!(
        report.info["candidate_run"]["not_usable_reason"],
        json!("dirty build, or the build state is not recorded")
    );
}

#[test]
fn a_run_recorded_under_another_boundary_or_into_the_holdout_is_never_used() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0, 0.010);
    let blocked = [
        (
            TrialSeries {
                holdout_start: Some("2099-01-01".to_owned()),
                ..series(3, 3, false, cand.clone())
            },
            "run under a different holdout boundary, or none recorded",
        ),
        (
            TrialSeries {
                data_end: Some(BOUNDARY + 1_000),
                ..series(3, 3, false, cand.clone())
            },
            "its data reaches into the holdout, or its end is not recorded",
        ),
    ];
    for (bad, reason) in blocked {
        // Not the candidate: left out of the trial Sharpes and the PBO matrix.
        let trials = vec![
            series(1, 2, false, alt.clone()),
            series(2, 1, false, cand.clone()),
            bad.clone(),
        ];
        let report = build_report(&inputs(1, trials, 3));
        assert_eq!(report.candidate_trial_id, Some(2));
        let sharpes = [
            moments::sharpe(&alt).unwrap(),
            moments::sharpe(&cand).unwrap(),
        ];
        assert_eq!(
            find(&report, "deflated_sharpe").value,
            dsr::deflated_sharpe(&cand, &sharpes, 3)
        );
        let pbo = find(&report, "pbo");
        assert_eq!(pbo.status, GateStatus::NotEvaluated);
        assert!(pbo.note.ends_with("found 2"), "{}", pbo.note);

        // As the newest run of the candidate's version: it is the candidate, and unusable.
        let as_candidate = TrialSeries {
            strategy_version_id: StrategyVersionId(1),
            ..bad
        };
        let trials = vec![series(1, 2, false, alt.clone()), as_candidate];
        let report = build_report(&inputs(1, trials, 2));
        assert_eq!(report.candidate_trial_id, Some(3));
        for name in ["deflated_sharpe", "pbo"] {
            let gate = find(&report, name);
            assert_eq!(gate.status, GateStatus::NotEvaluated, "{name}");
            assert_eq!(
                gate.note,
                format!("the candidate trial cannot be used: {reason}"),
                "{name}"
            );
        }
        assert_eq!(report.info["candidate_run"]["usable"], json!(false));
    }
}

#[test]
fn deflated_sharpe_needs_usable_sharpes_for_half_the_lifetime_trials() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0, 0.010);
    let unusable = TrialSeries {
        dirty_build: None,
        ..series(3, 3, false, alt.clone())
    };
    let trials = vec![
        series(1, 2, false, alt),
        series(2, 1, false, cand),
        unusable,
    ];
    let report = build_report(&inputs(1, trials, 100));
    let gate = find(&report, "deflated_sharpe");
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert!(
        gate.note
            .contains("fewer than half the lifetime trials have a usable Sharpe: 2 of 100"),
        "{}",
        gate.note
    );
}

#[test]
fn identical_trial_sharpes_with_coverage_leave_the_deflated_sharpe_not_evaluated() {
    let cand = drift(500, 0.001, 0.010);
    let trials = (1..=4).map(|i| series(i, i, false, cand.clone())).collect();
    let report = build_report(&inputs(1, trials, 4));
    let gate = find(&report, "deflated_sharpe");
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(
        gate.note,
        "the trial Sharpes do not vary, so there is nothing to deflate with"
    );
}

#[test]
fn reruns_of_the_same_version_snapshot_and_range_count_once() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0005, 0.010);
    let alt2 = drift(500, 0.002, 0.010);
    let older_rerun = drift(500, -0.001, 0.010);
    let trials = vec![
        series(1, 1, false, older_rerun),
        series(2, 1, false, cand.clone()),
        series(3, 2, false, alt.clone()),
        series(4, 3, false, alt2.clone()),
    ];
    let report = build_report(&inputs(1, trials, 4));
    let sharpes = [
        moments::sharpe(&cand).unwrap(),
        moments::sharpe(&alt).unwrap(),
        moments::sharpe(&alt2).unwrap(),
    ];
    assert_eq!(
        find(&report, "deflated_sharpe").value,
        dsr::deflated_sharpe(&cand, &sharpes, 4)
    );
}

#[test]
fn a_healthy_case_is_evaluated_on_the_deduplicated_sharpes() {
    let cand = drift(500, 0.001, 0.010);
    let alt1 = drift(500, 0.0005, 0.010);
    let alt2 = drift(500, 0.002, 0.010);
    let trials = vec![
        series(1, 2, false, alt1.clone()),
        series(2, 3, false, alt2.clone()),
        series(3, 1, false, cand.clone()),
    ];
    let report = build_report(&inputs(1, trials, 3));
    let gate = find(&report, "deflated_sharpe");
    assert_ne!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    let sharpes = [
        moments::sharpe(&alt1).unwrap(),
        moments::sharpe(&alt2).unwrap(),
        moments::sharpe(&cand).unwrap(),
    ];
    assert!(gate.value.is_some());
    assert_eq!(gate.value, dsr::deflated_sharpe(&cand, &sharpes, 3));
}

/// Two runs of one version over the same snapshot and range count as two Sharpes when their
/// costs differ (they are two experiments), and as one when every input is the same.
#[test]
fn deflated_sharpe_counts_runs_by_experiment_not_by_range() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0005, 0.010);
    let alt_sharpe = moments::sharpe(&alt).unwrap();
    let cand_sharpe = moments::sharpe(&cand).unwrap();

    // Trial 1 ran under costs B, trial 2 (the candidate) under costs A: two Sharpes.
    let other_costs = TrialSeries {
        costs: costs_b(),
        ..series(1, 1, false, alt.clone())
    };
    let trials = vec![other_costs, series(2, 1, false, cand.clone())];
    let report = build_report(&inputs(1, trials, 2));
    let gate = find(&report, "deflated_sharpe");
    assert_eq!(
        gate.value,
        dsr::deflated_sharpe(&cand, &[alt_sharpe, cand_sharpe], 2),
        "{}",
        gate.note
    );

    // Trial 1 and trial 2 are the same experiment: one Sharpe, the newer, and trial 3 is another
    // version's run. Two Sharpes in all for three lifetime trials.
    let trials = vec![
        series(1, 1, false, alt),
        series(2, 1, false, cand.clone()),
        series(3, 2, false, drift(500, 0.002, 0.010)),
    ];
    let report = build_report(&inputs(1, trials, 3));
    let other = moments::sharpe(&drift(500, 0.002, 0.010)).unwrap();
    assert_eq!(
        find(&report, "deflated_sharpe").value,
        dsr::deflated_sharpe(&cand, &[cand_sharpe, other], 3)
    );
}

#[test]
fn a_rerun_that_changes_only_the_seed_is_not_a_new_trial_sharpe() {
    let cand = drift(500, 0.001, 0.010);
    let alt = drift(500, 0.0005, 0.010);
    let alt_sharpe = moments::sharpe(&alt).unwrap();
    let cand_sharpe = moments::sharpe(&cand).unwrap();

    // Version 2 ran once, then three more times with other seeds. The strategy ignores the
    // seed, so the returns are identical: one Sharpe for version 2, not four.
    let mut trials = vec![series(1, 2, false, alt.clone())];
    for (id, seed) in [(2_i64, 11_u64), (3, 12), (4, 13)] {
        trials.push(TrialSeries {
            seed: Some(seed),
            ..series(id, 2, false, alt.clone())
        });
    }
    trials.push(series(5, 1, false, cand.clone()));
    // Four of five lifetime trials would be usable Sharpes if the re-runs counted; they do
    // not, so there are two.
    let report = build_report(&inputs(1, trials, 4));
    let gate = find(&report, "deflated_sharpe");
    assert_eq!(
        gate.value,
        dsr::deflated_sharpe(&cand, &[alt_sharpe, cand_sharpe], 4),
        "{}",
        gate.note
    );
}

#[test]
fn pbo_with_a_dominated_alternative_passes_at_zero() {
    let cand = drift(500, 0.011, 0.010);
    let alts = alternatives(9);
    let report = build_report(&inputs(1, configurations(&cand, &alts), 10));
    let gate = find(&report, "pbo");
    // Series in ascending strategy version id: the candidate (version 1), then versions 2 to 10.
    assert_eq!(gate.value, expected_pbo(&cand, &alts));
    assert_eq!(gate.status, GateStatus::Pass);
    assert_eq!(gate.value, Some(0.0));
}

#[test]
fn pbo_needs_at_least_ten_configurations() {
    let cand = drift(500, 0.011, 0.010);
    let alts = alternatives(9);

    let nine = build_report(&inputs(1, configurations(&cand, &alts[..8]), 9));
    let gate = find(&nine, "pbo");
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(
        gate.note,
        "needs at least 10 configurations over the same data, found 9"
    );

    let ten = build_report(&inputs(1, configurations(&cand, &alts), 10));
    assert_ne!(find(&ten, "pbo").status, GateStatus::NotEvaluated);
    assert_eq!(find(&ten, "pbo").value, expected_pbo(&cand, &alts));
}

#[test]
fn pbo_of_identical_series_is_one_and_fails() {
    let cand = drift(500, 0.001, 0.010);
    let copies = vec![cand.clone(); 9];
    let report = build_report(&inputs(1, configurations(&cand, &copies), 10));
    let gate = find(&report, "pbo");
    assert_eq!(gate.value, Some(1.0));
    assert_eq!(gate.status, GateStatus::Fail);
    assert_eq!(gate.value, expected_pbo(&cand, &copies));
}

#[test]
fn pbo_leaves_out_trials_over_other_data_or_another_length() {
    let cand = drift(500, 0.011, 0.010);
    let alts = alternatives(9);
    let alt = alts[0].clone();
    let other_snapshot = TrialSeries {
        data_snapshot_id: "other".to_owned(),
        ..series(11, 11, false, alt.clone())
    };
    let other_start = TrialSeries {
        requested_start: 1,
        ..series(11, 11, false, alt.clone())
    };
    let other_end = TrialSeries {
        requested_end: 2000,
        ..series(11, 11, false, alt.clone())
    };
    let other_length = series(11, 11, false, drift(499, 0.0, 0.010));
    let unusable = [
        TrialSeries {
            dirty_build: Some(true),
            ..series(11, 11, false, alt.clone())
        },
        TrialSeries {
            holdout_start: Some("2099-01-01".to_owned()),
            ..series(11, 11, false, alt.clone())
        },
        TrialSeries {
            data_end: Some(BOUNDARY + 1_000),
            ..series(11, 11, false, alt)
        },
    ];
    for other in [other_snapshot, other_start, other_end, other_length]
        .into_iter()
        .chain(unusable)
    {
        let mut trials = configurations(&cand, &alts);
        trials.push(other);
        let report = build_report(&inputs(1, trials, 11));
        let gate = find(&report, "pbo");
        assert!(
            gate.note.ends_with("over 10 configurations"),
            "{}",
            gate.note
        );
        assert_eq!(gate.value, expected_pbo(&cand, &alts));
    }
}

#[test]
fn pbo_counts_only_the_newest_trial_of_each_other_version() {
    let cand = drift(500, 0.011, 0.010);
    let alts = alternatives(9);
    // Version 2 has a dominated alternative (trial 1) and, newer, a copy of the candidate
    // (trial 20). Only the newer one is in the matrix.
    let mut trials = configurations(&cand, &alts);
    trials.push(series(20, 2, false, cand.clone()));
    let report = build_report(&inputs(1, trials, 11));
    let mut matrix: Vec<&[f64]> = vec![cand.as_slice(), cand.as_slice()];
    matrix.extend(alts[1..].iter().map(Vec::as_slice));
    assert_eq!(find(&report, "pbo").value, pbo::pbo(&matrix));
}

#[test]
fn pbo_leaves_out_another_versions_run_under_other_costs() {
    let cand = drift(500, 0.011, 0.010);
    let alts = alternatives(9);
    let mut trials = configurations(&cand, &alts);
    // Version 2's run (trial 1) was made under other costs than the candidate: it is left out.
    trials[0].costs = costs_b();
    let report = build_report(&inputs(1, trials.clone(), 10));
    let gate = find(&report, "pbo");
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(
        gate.note,
        "needs at least 10 configurations over the same data, found 9"
    );

    // The same run under the candidate's costs is in the matrix again.
    trials[0].costs = costs_a();
    let report = build_report(&inputs(1, trials, 10));
    assert_eq!(find(&report, "pbo").value, expected_pbo(&cand, &alts));
    assert_ne!(find(&report, "pbo").status, GateStatus::NotEvaluated);
}

// ---- holdout --------------------------------------------------------------------------------

/// A holdout run of `version` over `returns`, judged on its last `sessions` returns.
fn holdout_trial(
    trial_id: i64,
    version: i64,
    returns: Vec<f64>,
    sessions: Option<u64>,
) -> TrialSeries {
    TrialSeries {
        holdout_sessions: sessions,
        ..series(trial_id, version, true, returns)
    }
}

/// The returns of a holdout run made right after `base`: `base` followed by the holdout sessions
/// `tail`. A look is made on the run it tests, so the baseline's returns are a strict prefix of
/// the look's returns.
fn extended(base: &[f64], tail: &[f64]) -> Vec<f64> {
    [base, tail].concat()
}

fn holdout_gate_of(trials: Vec<TrialSeries>, looks: u64) -> GateResult {
    let mut i = inputs(1, trials, 2);
    i.holdout_looks = looks;
    find(&build_report(&i), "holdout").clone()
}

/// The annualised Sharpe of `returns`, as the gate reports it.
fn annualised_sharpe(returns: &[f64]) -> f64 {
    moments::annualise(moments::sharpe(returns).unwrap(), gates().periods_per_year)
}

/// `gates.json` with another `holdout_start`.
fn gates_starting(text: &str) -> Gates {
    let mut json: Value =
        serde_json::from_str(include_str!("../../../config/eval/gates.json")).unwrap();
    json["holdout_start"] = json!(text);
    Gates::from_json(&json).unwrap()
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
    // Two usable configurations: the holdout runs are not among them.
    let pbo = find(&report, "pbo");
    assert_eq!(pbo.status, GateStatus::NotEvaluated);
    assert!(pbo.note.ends_with("found 2"), "{}", pbo.note);
}

#[test]
fn holdout_gate_follows_the_looks_and_the_version_s_own_holdout_run() {
    let cand = drift(500, 0.001, 0.010);
    let own = holdout_trial(2, 1, extended(&cand, &drift(60, 0.001, 0.010)), Some(60));
    let other = series(2, 2, true, cand.clone());
    let base = vec![series(1, 1, false, cand)];

    assert_eq!(
        holdout_gate_of(base.clone(), 0).status,
        GateStatus::NotEvaluated
    );

    let mut with_own = base.clone();
    with_own.push(own);
    assert_eq!(
        holdout_gate_of(with_own.clone(), 3).status,
        GateStatus::Pass
    );
    assert_eq!(holdout_gate_of(with_own, 11).status, GateStatus::Fail);

    let mut with_other = base;
    with_other.push(other);
    assert_eq!(
        holdout_gate_of(with_other, 1).status,
        GateStatus::NotEvaluated
    );
}

#[test]
fn holdout_branches_are_judged_in_order() {
    let cand = drift(500, 0.011, 0.010);
    let base = vec![series(1, 1, false, cand.clone())];
    let haircut = (1.0 - gates().sharpe_haircut) * annualised_sharpe(&cand);
    let good = || holdout_trial(2, 1, extended(&cand, &drift(60, 0.011, 0.010)), Some(60));

    // 1. Looks beyond the limit: retired, whatever the run.
    let looks = gates().holdout_max_looks;
    let retired = holdout_gate_of(
        base.iter().cloned().chain([good()]).collect(),
        u64::from(looks) + 1,
    );
    assert_eq!(retired.status, GateStatus::Fail);
    assert_eq!(retired.value, Some(f64::from(looks) + 1.0));
    assert_eq!(retired.threshold, Some(f64::from(looks)));
    assert!(retired.note.contains("retired"), "{}", retired.note);

    // 2. No holdout run of this version.
    let none = holdout_gate_of(base.clone(), 0);
    assert_eq!(none.status, GateStatus::NotEvaluated);
    assert_eq!(none.note, "no holdout run for this version yet");

    // 3. A holdout run that cannot be used.
    let unusable = TrialSeries {
        holdout_start: Some("2099-01-01".to_owned()),
        ..good()
    };
    let gate = holdout_gate_of(base.iter().cloned().chain([unusable]).collect(), 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert!(
        gate.note
            .starts_with("the holdout run cannot be used: run under a different holdout boundary"),
        "{}",
        gate.note
    );

    // 4. Fewer than 60 sessions or none recorded; or more than the run has.
    for sessions in [Some(59), None] {
        let gate = holdout_gate_of(
            base.iter()
                .cloned()
                .chain([holdout_trial(2, 1, cand.clone(), sessions)])
                .collect(),
            0,
        );
        assert_eq!(gate.status, GateStatus::NotEvaluated, "{sessions:?}");
        assert_eq!(gate.note, "fewer than 60 holdout sessions", "{sessions:?}");
    }
    let too_many = holdout_gate_of(
        base.iter()
            .cloned()
            .chain([holdout_trial(2, 1, cand.clone(), Some(501))])
            .collect(),
        0,
    );
    assert_eq!(too_many.status, GateStatus::NotEvaluated);
    assert_eq!(
        too_many.note,
        "more holdout sessions than the holdout run has returns"
    );

    // 5. Ended flat after a drawdown: a fail, even with a good Sharpe.
    let flat = TrialSeries {
        final_drawdown_mode: Some("flat".to_owned()),
        ..good()
    };
    let gate = holdout_gate_of(base.iter().cloned().chain([flat]).collect(), 0);
    assert_eq!(gate.status, GateStatus::Fail);
    assert!(gate.note.contains("ended flat"), "{}", gate.note);

    // 6. The last 60 sessions below the haircut Sharpe: a fail.
    let weak_tail = drift(60, 0.0, 0.010);
    let weak = holdout_trial(2, 1, extended(&cand, &weak_tail), Some(60));
    let gate = holdout_gate_of(base.iter().cloned().chain([weak]).collect(), 0);
    assert_eq!(gate.status, GateStatus::Fail);
    assert_eq!(gate.value, Some(annualised_sharpe(&weak_tail)));
    assert_eq!(gate.threshold, Some(haircut));

    // 7. At or above the haircut Sharpe: a pass, on the last 60 sessions.
    for sessions in [60, 100] {
        let tail = &cand[cand.len() - sessions as usize..];
        let gate = holdout_gate_of(
            base.iter()
                .cloned()
                .chain([holdout_trial(2, 1, extended(&cand, tail), Some(sessions))])
                .collect(),
            0,
        );
        assert_eq!(gate.status, GateStatus::Pass, "{sessions}");
        assert_eq!(gate.value, Some(annualised_sharpe(tail)));
        assert_eq!(gate.threshold, Some(haircut));
    }
}

#[test]
fn the_holdout_is_judged_on_its_last_sessions_only() {
    let cand = drift(500, 0.011, 0.010);
    // The 440 sessions of the look before its last 60 are weak (about 0.1 a session against a
    // candidate of 1.0); its last 60 sessions are as strong as the candidate.
    let mut returns = drift(440, 0.0, 0.010);
    returns.extend(drift(60, 0.011, 0.010));
    let trials = vec![
        series(1, 1, false, cand.clone()),
        holdout_trial(2, 1, extended(&cand, &returns), Some(60)),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::Pass);
    assert_eq!(gate.value, Some(annualised_sharpe(&returns[440..])));
}

#[test]
fn the_first_matching_look_is_judged_and_a_later_one_cannot_rescue_it() {
    let cand = drift(500, 0.011, 0.010);
    let weak = drift(500, 0.0, 0.010);
    let base = series(1, 1, false, cand.clone());
    let first_a = holdout_trial(2, 1, extended(&cand, &weak[440..]), Some(60));

    // A later look under costs B (zero slippage) clears the bar; it is not the look judged.
    let later_b = TrialSeries {
        costs: costs_b(),
        ..holdout_trial(3, 1, cand.clone(), Some(60))
    };
    let gate = holdout_gate_of(vec![base.clone(), first_a.clone(), later_b], 0);
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);
    assert_eq!(gate.value, Some(annualised_sharpe(&weak[440..])));

    // A later look under costs A, which clears the bar, is not judged either.
    let later_a = holdout_trial(3, 1, cand, Some(60));
    let gate = holdout_gate_of(vec![base, first_a, later_a], 0);
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);
    assert_eq!(gate.value, Some(annualised_sharpe(&weak[440..])));
}

#[test]
fn a_first_look_under_other_symbols_is_judged_and_a_later_matching_one_cannot_rescue_it() {
    let cand = drift(500, 0.011, 0.010);
    let base = series(1, 1, false, cand.clone());
    let other_symbols = TrialSeries {
        symbols: json!(["QQQ"]),
        ..holdout_trial(2, 1, cand, Some(60))
    };
    let gate = holdout_gate_of(vec![base.clone(), other_symbols.clone()], 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, DIFFERENT_INPUTS);

    // The first look is the one judged, even though a later matching run is strong enough to
    // pass: the candidate is compared with the first look's inputs, which differ here.
    let weak = drift(500, 0.0, 0.010);
    let matching = holdout_trial(3, 1, weak, Some(60));
    let gate = holdout_gate_of(vec![base, other_symbols, matching], 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, DIFFERENT_INPUTS);
}

#[test]
fn a_candidate_registered_after_the_holdout_look_is_not_disturbed_by_it() {
    let cand = drift(500, 0.011, 0.010);
    // The look (trial 1) comes before the candidate (trial 2), which has the same inputs. The
    // holdout's baseline must be registered before the look, so there is none here.
    let trials = vec![
        holdout_trial(1, 1, cand.clone(), Some(60)),
        series(2, 1, false, cand),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, NO_BASELINE);
}

#[test]
fn a_flat_holdout_run_with_zero_returns_fails() {
    let cand = drift(500, 0.011, 0.010);
    let base = series(1, 1, false, cand);
    for sessions in [Some(60), None] {
        let flat = TrialSeries {
            final_drawdown_mode: Some("flat".to_owned()),
            ..holdout_trial(2, 1, vec![0.0; 500], sessions)
        };
        let gate = holdout_gate_of(vec![base.clone(), flat], 0);
        assert_eq!(gate.status, GateStatus::Fail, "{sessions:?}: {}", gate.note);
        assert!(gate.note.contains("ended flat"), "{}", gate.note);
    }
}

#[test]
fn the_holdout_needs_sixty_sessions_exactly() {
    let cand = drift(500, 0.011, 0.010);
    let base = series(1, 1, false, cand.clone());
    let run = |sessions: u64| {
        let tail = &cand[cand.len() - sessions as usize..];
        holdout_gate_of(
            vec![
                base.clone(),
                holdout_trial(2, 1, extended(&cand, tail), Some(sessions)),
            ],
            0,
        )
    };
    assert_eq!(run(59).status, GateStatus::NotEvaluated);
    assert_eq!(run(60).status, GateStatus::Pass);
}

#[test]
fn the_threshold_ignores_the_flat_warm_up() {
    let ppy = gates().periods_per_year;
    let strong = drift(300, 0.011, 0.010);
    // Two hundred and fifty flat sessions before the first trade, then the strong returns.
    let mut warm = vec![0.0; 250];
    warm.extend_from_slice(&strong);

    let active = moments::annualise(moments::sharpe(&strong).unwrap(), ppy);
    let full = moments::annualise(moments::sharpe(&warm).unwrap(), ppy);
    let expected = (1.0 - gates().sharpe_haircut) * active;
    assert!(
        (1.0 - gates().sharpe_haircut) * full < expected,
        "the warm-up must lower the full-series figure"
    );

    // A holdout tail between the two haircut figures: the warm-up version would pass it, the
    // active one must not.
    let tail = drift(60, 0.004, 0.010);
    let tail_sharpe = annualised_sharpe(&tail);
    assert!(
        (1.0 - gates().sharpe_haircut) * full < tail_sharpe && tail_sharpe < expected,
        "{tail_sharpe} not between {} and {expected}",
        (1.0 - gates().sharpe_haircut) * full
    );

    let trials = vec![
        series(1, 1, false, warm.clone()),
        holdout_trial(2, 1, extended(&warm, &tail), Some(60)),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);
    let threshold = gate.threshold.unwrap();
    assert!(
        (threshold - expected).abs() < 1e-12,
        "{threshold} vs {expected}"
    );
}

#[test]
fn a_baseline_with_no_active_return_is_not_evaluated() {
    // All flat: no non-zero return at all, so it is too short before its Sharpe is even asked.
    let flat = vec![0.0; 500];
    let base = series(1, 1, false, flat.clone());
    let holdout = holdout_trial(2, 1, extended(&flat, &drift(60, 0.011, 0.010)), Some(60));
    let gate = holdout_gate_of(vec![base, holdout], 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, SHORT_BASELINE);
}

#[test]
fn info_reports_the_active_sharpe_and_the_judged_holdout_run() {
    let ppy = gates().periods_per_year;
    let strong = drift(300, 0.011, 0.010);
    let mut warm = vec![0.0; 250];
    warm.extend_from_slice(&strong);
    let trials = vec![
        series(1, 1, false, warm.clone()),
        holdout_trial(2, 1, extended(&warm, &drift(60, 0.011, 0.010)), Some(60)),
        TrialSeries {
            costs: costs_b(),
            ..holdout_trial(3, 1, drift(60, 0.011, 0.010), Some(60))
        },
    ];
    let report = build_report(&inputs(1, trials, 3));
    let info = &report.info;
    let active = moments::annualise(moments::sharpe(&strong).unwrap(), ppy);
    let full = moments::annualise(moments::sharpe(&warm).unwrap(), ppy);
    assert!((info["active_sharpe"].as_f64().unwrap() - active).abs() < 1e-9);
    assert!((info["sharpe"].as_f64().unwrap() - full).abs() < 1e-9);
    // The haircut stays on the full series.
    assert!((info["haircut_sharpe"].as_f64().unwrap() - 0.5 * full).abs() < 1e-9);

    let run = &info["holdout_run"];
    assert_eq!(run["trial_id"], json!(2));
    assert_eq!(run["costs"], costs_a());
    assert_eq!(run["symbols"], json!(["SPY"]));
    assert_eq!(run["baseline_trial_id"], json!(1));
    assert_eq!(run["holdout_runs_for_version"], json!(2));
    // The baseline's active Sharpe is the one the threshold is built from.
    assert!((info["holdout_baseline_active_sharpe"].as_f64().unwrap() - active).abs() < 1e-9);

    let none = build_report(&inputs(1, vec![series(1, 1, false, vec![0.0; 500])], 1));
    assert!(none.info["active_sharpe"].is_null());
    assert!(none.info["holdout_baseline_active_sharpe"].is_null());
    assert!(none.info["holdout_run"].is_null());
}

#[test]
fn a_holdout_boundary_out_of_range_makes_the_trial_unusable() {
    let early = gates_starting("1000-01-01");
    let trial = TrialSeries {
        holdout_start: Some("1000-01-01".to_owned()),
        ..series(1, 1, false, drift(10, 0.0, 0.01))
    };
    assert_eq!(
        trial.usable(&early),
        Err("the holdout boundary is out of range")
    );
}

const DIFFERENT_INPUTS: &str = "the candidate's inputs (costs, symbols, limits, starting cash or seed) differ from those the holdout was looked at with";
const NO_BASELINE: &str = "no usable ordinary run before the first holdout look has its inputs, start date, bar file and the same returns up to its end";
const RUN_UNRECORDED: &str = "the holdout run's final drawdown mode is not recorded";
const BASELINE_UNRECORDED: &str = "the holdout baseline's final drawdown mode is not recorded";
const OTHER_EXPERIMENT: &str = "the holdout look tested another experiment than the candidate (the last ordinary run before the look)";
const SHORT_BASELINE: &str =
    "the holdout baseline has fewer than 252 sessions with a non-zero return";
const FLAT_BASELINE: &str = "the holdout baseline ended flat after a drawdown";
const UNDEFINED_BASELINE: &str = "the holdout baseline's active Sharpe ratio is undefined";
const NOT_POSITIVE: &str =
    "the holdout baseline's Sharpe is not positive, so there is nothing for the holdout to hold up";

/// The review's sequence: trial 1 ordinary and trial 2 holdout (a weak look) under costs written
/// "2" and symbols SPY and QQQ; then trial 3 ordinary and trial 4 holdout (a strong look) under
/// `costs_3_4` and `symbols_3_4`.
fn bypass_trials(costs_3_4: Value, symbols_3_4: Value) -> Vec<TrialSeries> {
    let cand = drift(500, 0.011, 0.010);
    let weak = drift(500, 0.0, 0.010);
    let first = |trial: TrialSeries| TrialSeries {
        costs: json!({"slippage_bps": "2"}),
        symbols: json!(["SPY", "QQQ"]),
        ..trial
    };
    let rerun = |trial: TrialSeries| TrialSeries {
        costs: costs_3_4.clone(),
        symbols: symbols_3_4.clone(),
        ..trial
    };
    vec![
        first(series(1, 1, false, cand.clone())),
        first(holdout_trial(2, 1, extended(&cand, &weak[440..]), Some(60))),
        rerun(series(3, 1, false, cand.clone())),
        rerun(holdout_trial(
            4,
            1,
            extended(&cand, &drift(60, 0.011, 0.010)),
            Some(60),
        )),
    ]
}

#[test]
fn a_reformatted_rerun_cannot_replace_the_first_look_by_reformatted_costs() {
    let trials = bypass_trials(json!({"slippage_bps": "2.0"}), json!(["SPY", "QQQ"]));
    let report = build_report(&inputs(1, trials, 4));
    assert_eq!(report.candidate_trial_id, Some(3));
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(2));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(1));
}

#[test]
fn a_reformatted_rerun_with_reordered_symbols_cannot_replace_the_first_look() {
    let trials = bypass_trials(json!({"slippage_bps": "2.0"}), json!(["QQQ", "SPY"]));
    let report = build_report(&inputs(1, trials, 4));
    assert_eq!(report.candidate_trial_id, Some(3));
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(2));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(1));
}

#[test]
fn a_rerun_with_genuinely_other_costs_is_not_evaluated_and_never_passes() {
    let trials = bypass_trials(json!({"slippage_bps": "0"}), json!(["SPY", "QQQ"]));
    let report = build_report(&inputs(1, trials, 4));
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, DIFFERENT_INPUTS);
    assert_ne!(gate.status, GateStatus::Pass);
}

#[test]
fn an_identical_ordinary_rerun_after_a_passed_holdout_does_not_disturb_it() {
    let cand = drift(500, 0.011, 0.010);
    let trials = vec![
        series(5, 1, false, cand.clone()),
        holdout_trial(6, 1, extended(&cand, &drift(60, 0.011, 0.010)), Some(60)),
        series(7, 1, false, cand),
    ];
    let mut i = inputs(1, trials, 7);
    i.holdout_looks = 1;
    let report = build_report(&i);
    assert_eq!(report.candidate_trial_id, Some(7));
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::Pass, "{}", gate.note);
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(6));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(5));
}

#[test]
fn the_baseline_is_the_newest_matching_ordinary_run_before_the_look() {
    let strong = drift(500, 0.011, 0.010);
    // Trial 1 (weak, negative) would set a zero threshold; trial 2 (strong) must set it.
    let trials = vec![
        series(1, 1, false, drift(500, -0.001, 0.010)),
        series(2, 1, false, strong.clone()),
        holdout_trial(3, 1, extended(&strong, &drift(60, 0.001, 0.010)), Some(60)),
    ];
    let report = build_report(&inputs(1, trials, 3));
    assert_eq!(report.candidate_trial_id, Some(2));
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(2));
    let haircut = (1.0 - gates().sharpe_haircut) * annualised_sharpe(&strong);
    assert_eq!(gate.threshold, Some(haircut));
}

#[test]
fn no_usable_ordinary_run_before_the_first_look_leaves_the_holdout_not_evaluated() {
    let cand = drift(500, 0.011, 0.010);
    // The only earlier ordinary run is a dirty build.
    let dirty = TrialSeries {
        dirty_build: Some(true),
        ..series(1, 1, false, cand.clone())
    };
    let gate = holdout_gate_of(vec![dirty, holdout_trial(2, 1, cand.clone(), Some(60))], 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, NO_BASELINE);

    // The only earlier ordinary run has other symbols; the candidate comes after the look.
    let other_symbols = TrialSeries {
        symbols: json!(["QQQ"]),
        ..series(1, 1, false, cand.clone())
    };
    let trials = vec![
        other_symbols,
        holdout_trial(2, 1, cand.clone(), Some(60)),
        series(3, 1, false, cand),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, NO_BASELINE);
}

#[test]
fn a_barely_positive_holdout_cannot_pass_against_a_losing_shorter_run() {
    // Trial 1: ordinary, full range, strong. Trial 2: ordinary, shorter range, losing. Trial 3:
    // the first holdout look, barely positive. Trial 4: an ordinary re-run of trial 1.
    let cand = drift(500, 0.011, 0.010);
    let shorter_returns = drift(500, -0.001, 0.010);
    let shorter = TrialSeries {
        requested_start: 100,
        requested_end: 600,
        ..series(2, 1, false, shorter_returns.clone())
    };
    // The look covers the shorter run's start, so it is made on that run.
    let look = TrialSeries {
        requested_start: 100,
        ..holdout_trial(
            3,
            1,
            extended(&shorter_returns, &drift(60, 0.0001, 0.010)),
            Some(60),
        )
    };
    let trials = vec![
        series(1, 1, false, cand.clone()),
        shorter,
        look,
        series(4, 1, false, cand),
    ];
    let mut i = inputs(1, trials, 4);
    i.holdout_looks = 1;
    let report = build_report(&i);
    assert_eq!(report.candidate_trial_id, Some(4));
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(3));
    // The look was made right after trial 2, so trial 2 is its baseline. Trial 4 tests another
    // experiment than the look did, so the look is not judged for it: never a pass.
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(2));
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, OTHER_EXPERIMENT);
    assert_eq!(gate.value, None);
    assert_eq!(gate.threshold, None);
}

#[test]
fn strong_then_weak_over_one_range_judges_the_weak_run_everywhere() {
    let strong = drift(500, 0.011, 0.010);
    let weak = drift(500, 0.002, 0.010);
    let trials = vec![
        series(1, 1, false, strong),
        series(2, 1, false, weak.clone()),
        holdout_trial(3, 1, extended(&weak, &drift(60, 0.004, 0.010)), Some(60)),
    ];
    let mut i = inputs(1, trials, 2);
    i.holdout_looks = 1;
    let report = build_report(&i);
    assert_eq!(report.candidate_trial_id, Some(2));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(2));
    assert_eq!(report.info["candidate_trial_id"], json!(2));
    assert!((report.info["sharpe"].as_f64().unwrap() - annualised_sharpe(&weak)).abs() < 1e-9);
    // The holdout clears the weak run's haircut, not the strong run's: it is judged against
    // the candidate's own threshold.
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::Pass, "{}", gate.note);
    let threshold = (1.0 - gates().sharpe_haircut) * annualised_sharpe(&weak);
    assert!(
        gate.threshold
            .is_some_and(|t| (t - threshold).abs() < 1e-12)
    );
}

#[test]
fn a_baseline_over_another_snapshot_or_range_is_not_the_candidates() {
    let cand = drift(500, 0.011, 0.010);
    let holdout = || holdout_trial(2, 1, extended(&cand, &drift(60, 0.011, 0.010)), Some(60));
    // Strong, but over another snapshot; the candidate is registered after the look.
    let other_snapshot = TrialSeries {
        data_snapshot_id: "other".to_owned(),
        ..series(1, 1, false, cand.clone())
    };
    // The look's baseline is the strong run over the other snapshot, which the candidate is not.
    let gate = holdout_gate_of(
        vec![other_snapshot, holdout(), series(3, 1, false, cand.clone())],
        0,
    );
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, OTHER_EXPERIMENT);

    // Strong, but over another requested end only.
    let other_end = TrialSeries {
        requested_end: 900,
        ..series(1, 1, false, cand.clone())
    };
    let gate = holdout_gate_of(
        vec![other_end, holdout(), series(3, 1, false, cand.clone())],
        0,
    );
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, OTHER_EXPERIMENT);

    // The only earlier ordinary run has other costs than the look: no baseline at all.
    let trials = vec![
        TrialSeries {
            costs: costs_b(),
            ..series(1, 1, false, cand.clone())
        },
        holdout(),
        series(3, 1, false, cand),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, NO_BASELINE);
}

#[test]
fn a_candidate_after_the_look_is_judged_on_an_earlier_identical_copy_only() {
    let cand = drift(500, 0.011, 0.010);
    let holdout = holdout_trial(2, 1, extended(&cand, &drift(60, 0.011, 0.010)), Some(60));

    // An identical earlier copy, registered before the look, is the baseline.
    let trials = vec![
        series(1, 1, false, cand.clone()),
        holdout.clone(),
        series(3, 1, false, cand.clone()),
    ];
    let mut i = inputs(1, trials, 3);
    i.holdout_looks = 1;
    let report = build_report(&i);
    assert_eq!(report.candidate_trial_id, Some(3));
    assert_eq!(status(&report, "holdout"), GateStatus::Pass);
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(1));

    // The only earlier run has another requested end: nothing matches the candidate.
    let other_end = TrialSeries {
        requested_end: 900,
        ..series(1, 1, false, cand.clone())
    };
    let gate = holdout_gate_of(vec![other_end, holdout, series(3, 1, false, cand)], 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, OTHER_EXPERIMENT);
}

#[test]
fn a_baseline_that_is_not_positive_is_never_a_pass() {
    for returns in [drift(500, -0.011, 0.010), drift(500, 0.0, 0.010)] {
        let look = holdout_trial(2, 1, extended(&returns, &drift(60, 0.011, 0.010)), Some(60));
        let trials = vec![series(1, 1, false, returns), look];
        let gate = holdout_gate_of(trials, 0);
        assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
        assert_eq!(gate.note, NOT_POSITIVE);
        assert_eq!(gate.value, None);
        assert_eq!(gate.threshold, None);
    }
}

/// The holdout gate when the candidate (trial 1, ordinary, strong) and the holdout run (trial 2)
/// carry the given costs and symbols.
fn gate_with_inputs(
    cand_costs: Value,
    cand_symbols: Value,
    run_costs: Value,
    run_symbols: Value,
) -> GateResult {
    let cand = drift(500, 0.011, 0.010);
    let base = TrialSeries {
        costs: cand_costs,
        symbols: cand_symbols,
        ..series(1, 1, false, cand.clone())
    };
    let run = TrialSeries {
        costs: run_costs,
        symbols: run_symbols,
        ..holdout_trial(2, 1, extended(&cand, &drift(60, 0.011, 0.010)), Some(60))
    };
    holdout_gate_of(vec![base, run], 0)
}

#[test]
fn same_inputs_compares_costs_as_decimals_and_symbols_as_a_set() {
    let spy = json!(["SPY"]);
    // Equal: decimal costs with other spellings, symbols in another order or repeated.
    let equal = [
        (
            json!({"slippage_bps": "2"}),
            spy.clone(),
            json!({"slippage_bps": "2.0"}),
            spy.clone(),
        ),
        (costs_a(), json!(["A", "B"]), costs_a(), json!(["B", "A"])),
        (
            costs_a(),
            json!(["A", "A", "B"]),
            costs_a(),
            json!(["B", "A"]),
        ),
    ];
    for (cc, cs, rc, rs) in equal {
        let gate = gate_with_inputs(cc.clone(), cs.clone(), rc.clone(), rs.clone());
        assert_eq!(
            gate.status,
            GateStatus::Pass,
            "{cc} {cs} vs {rc} {rs}: {}",
            gate.note
        );
    }

    // Not equal: each case leaves the gate not evaluated for the candidate's inputs.
    let different = [
        (costs_a(), spy.clone(), costs_b(), spy.clone()),
        (
            json!({"slippage_bps": "2"}),
            spy.clone(),
            json!({"slippage_bps": "3"}),
            spy.clone(),
        ),
        (Value::Null, spy.clone(), costs_a(), spy.clone()),
        (costs_a(), spy.clone(), Value::Null, spy.clone()),
        (Value::Null, spy.clone(), Value::Null, spy.clone()),
        (
            json!({"slippage_bps": 2}),
            spy.clone(),
            costs_a(),
            spy.clone(),
        ),
        (
            json!({"slippage_bps": "two"}),
            spy.clone(),
            json!({"slippage_bps": "two"}),
            spy.clone(),
        ),
        (
            costs_a(),
            spy.clone(),
            json!({"slippage_bps": "2", "fee_per_share": "0"}),
            spy.clone(),
        ),
        (costs_a(), json!(["A"]), costs_a(), json!(["A", "B"])),
        (costs_a(), Value::Null, costs_a(), spy.clone()),
        (costs_a(), spy, costs_a(), Value::Null),
        (costs_a(), json!(["SPY", 1]), costs_a(), json!(["SPY", 1])),
    ];
    for (cc, cs, rc, rs) in different {
        let gate = gate_with_inputs(cc.clone(), cs.clone(), rc.clone(), rs.clone());
        assert_eq!(
            gate.status,
            GateStatus::NotEvaluated,
            "{cc} {cs} vs {rc} {rs}"
        );
        assert_eq!(gate.note, DIFFERENT_INPUTS, "{cc} {cs} vs {rc} {rs}");
    }
}

#[test]
fn the_holdout_checks_run_in_the_documented_order() {
    let cand = drift(500, 0.011, 0.010);
    let ord = |id: i64, returns: Vec<f64>| series(id, 1, false, returns);
    let hold =
        |id: i64, returns: Vec<f64>, sessions: Option<u64>| holdout_trial(id, 1, returns, sessions);
    let unusable = |t: TrialSeries| TrialSeries {
        holdout_start: Some("2099-01-01".to_owned()),
        ..t
    };
    let flat = |t: TrialSeries| TrialSeries {
        final_drawdown_mode: Some("flat".to_owned()),
        ..t
    };
    let other_costs = |t: TrialSeries| TrialSeries {
        costs: costs_b(),
        ..t
    };
    let ok = Some(60);

    // (a) before (b): retired, with no candidate at all.
    let gate = holdout_gate_of(vec![hold(1, cand.clone(), ok)], 11);
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);

    // (b): no candidate.
    let gate = holdout_gate_of(vec![hold(1, cand.clone(), ok)], 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(
        gate.note,
        "no backtest trial of this version has a stored result yet"
    );

    // (c): no holdout run.
    let gate = holdout_gate_of(vec![ord(1, cand.clone())], 0);
    assert_eq!(gate.note, "no holdout run for this version yet");

    // (d) before (e): the candidate's costs differ and the look is unusable.
    let trials = vec![
        ord(1, cand.clone()),
        other_costs(unusable(hold(2, cand.clone(), ok))),
    ];
    assert_eq!(holdout_gate_of(trials, 0).note, DIFFERENT_INPUTS);

    // (e) before (f): unusable and flat.
    let trials = vec![
        ord(1, cand.clone()),
        flat(unusable(hold(2, cand.clone(), ok))),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert!(
        gate.note.starts_with("the holdout run cannot be used: "),
        "{}",
        gate.note
    );

    // (f) before (g): flat with too few sessions.
    let trials = vec![ord(1, cand.clone()), flat(hold(2, cand.clone(), Some(59)))];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::Fail, "{}", gate.note);

    // (g) before (h): too few sessions, and no baseline either.
    let trials = vec![hold(1, cand.clone(), Some(59)), ord(2, cand.clone())];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.note, "fewer than 60 holdout sessions");

    // (h): no baseline (the only ordinary run is registered after the look).
    let trials = vec![hold(1, cand.clone(), ok), ord(2, cand.clone())];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.note, NO_BASELINE);

    // (i) before (j): the baseline is too short and over another range than the candidate; the
    // experiment is checked first.
    let other_range = TrialSeries {
        requested_end: 900,
        ..ord(1, drift(251, 0.011, 0.010))
    };
    let trials = vec![other_range, hold(2, cand.clone(), ok), ord(3, cand.clone())];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.note, OTHER_EXPERIMENT);

    // (j1) before (j2): a flat baseline, which also has too few non-zero sessions.
    let mut flat_short = ord(1, drift(251, 0.011, 0.010));
    flat_short.final_drawdown_mode = Some("flat".to_owned());
    let gate = holdout_gate_of(vec![flat_short, hold(2, cand.clone(), ok)], 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, FLAT_BASELINE);

    // (j2): 251 non-zero sessions, the candidate being the baseline itself.
    let trials = vec![ord(1, drift(251, 0.011, 0.010)), hold(2, cand.clone(), ok)];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, SHORT_BASELINE);

    // (k) before (l): 300 active sessions whose Sharpe is undefined (constant, no variance).
    let trials = vec![
        ord(1, vec![0.5; 300]),
        hold(2, extended(&[0.5; 300], &drift(60, 0.011, 0.010)), ok),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.note, UNDEFINED_BASELINE);

    // (l): the baseline's Sharpe is negative, so the threshold would be zero.
    let trials = vec![
        ord(1, drift(500, -0.011, 0.010)),
        hold(
            2,
            extended(&drift(500, -0.011, 0.010), &drift(60, 0.011, 0.010)),
            ok,
        ),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, NOT_POSITIVE);

    // (m): the holdout sessions have no Sharpe ratio (all flat).
    let trials = vec![
        ord(1, cand.clone()),
        hold(2, extended(&cand, &[0.0; 60]), ok),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, "the holdout sessions have no Sharpe ratio");

    // (n): the comparison decides, Fail below the haircut Sharpe and Pass at or above it.
    let trials = vec![
        ord(1, cand.clone()),
        hold(2, extended(&cand, &drift(60, 0.0, 0.010)), ok),
    ];
    assert_eq!(holdout_gate_of(trials, 0).status, GateStatus::Fail);
    let trials = vec![
        ord(1, cand.clone()),
        hold(2, extended(&cand, &drift(60, 0.011, 0.010)), ok),
    ];
    assert_eq!(holdout_gate_of(trials, 0).status, GateStatus::Pass);
}

/// The review's scenario, one version. Trial 1: range A (the default), strong. Trial 2: range B
/// (requested 100 to 900), shorter, weakly positive: the last ordinary run before the look. Trial
/// 3: the first holdout look, mediocre: above half of trial 2's Sharpe, below half of trial 1's.
/// The baseline is trial 2 whatever comes later.
#[test]
fn the_look_is_judged_against_its_own_baseline_whatever_is_registered_later() {
    let strong = drift(500, 0.011, 0.010);
    let weak = drift(300, 0.002, 0.010);
    let range_b = |trial: TrialSeries| TrialSeries {
        requested_start: 100,
        requested_end: 900,
        ..trial
    };
    let first = series(1, 1, false, strong.clone());
    let second = range_b(series(2, 1, false, weak.clone()));
    // The look covers range B's start, so it is made on trial 2.
    let look = TrialSeries {
        requested_start: 100,
        ..holdout_trial(3, 1, extended(&weak, &drift(60, 0.0035, 0.010)), Some(60))
    };
    let report_for = |trials: Vec<TrialSeries>| {
        let count = trials.len() as u64;
        let mut i = inputs(1, trials, count);
        i.holdout_looks = 1;
        build_report(&i)
    };

    // Trial 4 re-runs range A: the candidate tests another experiment than the look did. The
    // holdout is not judged for it, whatever its own figures, and never passes.
    let rerun_a = series(4, 1, false, strong);
    let report = report_for(vec![first.clone(), second.clone(), look.clone(), rerun_a]);
    assert_eq!(report.candidate_trial_id, Some(4));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(2));
    assert!(
        (report.info["holdout_baseline_active_sharpe"]
            .as_f64()
            .unwrap()
            - annualised_sharpe(&weak))
        .abs()
            < 1e-9
    );
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, OTHER_EXPERIMENT);
    assert_ne!(gate.status, GateStatus::Pass);

    // Trial 5 re-runs range B, trial 2's experiment: judged against trial 2's threshold. The
    // weak experiment is then what every other gate judges too, as the candidate's own figures.
    let rerun_b = range_b(series(5, 1, false, weak.clone()));
    let report = report_for(vec![first.clone(), second.clone(), look.clone(), rerun_b]);
    assert_eq!(report.candidate_trial_id, Some(5));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(2));
    let gate = find(&report, "holdout");
    assert_eq!(gate.status, GateStatus::Pass, "{}", gate.note);
    let threshold = (1.0 - gates().sharpe_haircut) * annualised_sharpe(&weak);
    assert!(
        gate.threshold
            .is_some_and(|t| (t - threshold).abs() < 1e-12)
    );

    // No re-run: the candidate is trial 2 itself.
    let report = report_for(vec![first, second, look]);
    assert_eq!(report.candidate_trial_id, Some(2));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(2));
    assert_eq!(status(&report, "holdout"), GateStatus::Pass);
}

/// The intended use: the look is made right after the run it judges. A later identical re-run
/// does not change the judged look or its baseline.
#[test]
fn a_look_made_right_after_its_run_passes_and_an_identical_rerun_does_not_disturb_it() {
    let cand = drift(500, 0.011, 0.010);
    let base = series(1, 1, false, cand.clone());
    // Tail Sharpe 1.0 against a threshold of 0.5.
    let look = holdout_trial(2, 1, extended(&cand, &drift(60, 0.011, 0.010)), Some(60));

    let mut i = inputs(1, vec![base.clone(), look.clone()], 2);
    i.holdout_looks = 1;
    let report = build_report(&i);
    assert_eq!(report.candidate_trial_id, Some(1));
    assert_eq!(status(&report, "holdout"), GateStatus::Pass);
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(1));

    let mut i = inputs(1, vec![base, look, series(3, 1, false, cand)], 3);
    i.holdout_looks = 1;
    let report = build_report(&i);
    assert_eq!(report.candidate_trial_id, Some(3));
    assert_eq!(status(&report, "holdout"), GateStatus::Pass);
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(2));
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(1));
}

/// The baseline needs `MIN_BASELINE_SESSIONS` active sessions; the flat warm-up does not count.
#[test]
fn a_baseline_needs_252_active_sessions_and_the_warm_up_does_not_count() {
    let judged_with = |returns: Vec<f64>| {
        let look = holdout_trial(2, 1, extended(&returns, &drift(60, 0.011, 0.010)), Some(60));
        let trials = vec![series(1, 1, false, returns), look];
        holdout_gate_of(trials, 0)
    };

    let gate = judged_with(drift(251, 0.011, 0.010));
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, SHORT_BASELINE);

    // 501 returns, but only 251 with a non-zero return: the flat warm-up does not count.
    let mut warm = vec![0.0; 250];
    warm.extend(drift(251, 0.011, 0.010));
    let gate = judged_with(warm);
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert_eq!(gate.note, SHORT_BASELINE);

    let gate = judged_with(drift(252, 0.011, 0.010));
    assert_eq!(gate.status, GateStatus::Pass, "{}", gate.note);
    assert_eq!(
        gate.threshold,
        Some(0.5 * annualised_sharpe(&drift(252, 0.011, 0.010)))
    );

    // The same with a flat warm-up in front: 252 active sessions after 250 flat ones.
    let mut warm = vec![0.0; 250];
    warm.extend(drift(252, 0.011, 0.010));
    assert_eq!(judged_with(warm).status, GateStatus::Pass);
}

/// A baseline with an annualised Sharpe of 0.02 over 500 sessions is judged: its threshold is 0.01.
/// The minimum length, not a minimum Sharpe, is the guard. Such a candidate fails the deflated
/// Sharpe gate, which is what keeps a weak candidate from passing; this test checks that too.
#[test]
fn a_baseline_sharpe_of_two_hundredths_is_judged_because_the_length_is_the_guard() {
    let per_period = 0.02 / f64::from(gates().periods_per_year).sqrt();
    let weak = drift(500, per_period * 0.01, 0.010);
    assert!((annualised_sharpe(&weak) - 0.02).abs() < 1e-3);

    let mut trials = configurations(&weak, &alternatives(9));
    trials.push(holdout_trial(
        11,
        1,
        extended(&weak, &drift(60, 0.001, 0.010)),
        Some(60),
    ));
    let mut i = inputs(1, trials, 11);
    i.holdout_looks = 1;
    let report = build_report(&i);
    assert_eq!(report.candidate_trial_id, Some(10));
    let holdout = find(&report, "holdout");
    assert_eq!(holdout.status, GateStatus::Pass, "{}", holdout.note);
    let threshold = holdout.threshold.unwrap();
    assert!((threshold - 0.01).abs() < 1e-3, "{threshold}");
    assert_eq!(status(&report, "deflated_sharpe"), GateStatus::Fail);
}

#[test]
fn info_describes_the_holdout_run_without_a_verdict_and_a_null_baseline() {
    let cand = drift(500, 0.011, 0.010);
    // A holdout run and no candidate at all.
    let report = build_report(&inputs(
        1,
        vec![holdout_trial(1, 1, cand.clone(), Some(60))],
        1,
    ));
    assert_eq!(status(&report, "holdout"), GateStatus::NotEvaluated);
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(1));
    assert!(report.info["holdout_run"]["baseline_trial_id"].is_null());

    // A candidate whose costs differ from the run's: not evaluated, the run still described.
    let trials = vec![
        TrialSeries {
            costs: costs_b(),
            ..series(1, 1, false, cand.clone())
        },
        holdout_trial(2, 1, cand, Some(60)),
    ];
    let report = build_report(&inputs(1, trials, 2));
    assert_eq!(status(&report, "holdout"), GateStatus::NotEvaluated);
    assert_eq!(report.info["holdout_run"]["trial_id"], json!(2));
    assert!(report.info["holdout_run"]["baseline_trial_id"].is_null());
    assert!(report.info["holdout_baseline_active_sharpe"].is_null());
}

// ---- the same experiment: limits, starting cash, seed and bar file --------------------------

/// The review's sequence, one version: trial 1 an ordinary run (weak), trial 2 the look made right
/// after it (its tail as good as trial 1's, so it passes against trial 1's threshold), trial 3 an
/// ordinary re-run with the given returns. Every trial carries the default inputs unless a test
/// changes it.
fn look_scenario_with(rerun: Vec<f64>) -> [TrialSeries; 3] {
    let weak = drift(500, 0.002, 0.010);
    [
        series(1, 1, false, weak.clone()),
        holdout_trial(2, 1, extended(&weak, &drift(60, 0.002, 0.010)), Some(60)),
        series(3, 1, false, rerun),
    ]
}

/// The look scenario with an identical re-run of trial 1: the experiment the look tested.
fn look_scenario() -> [TrialSeries; 3] {
    look_scenario_with(drift(500, 0.002, 0.010))
}

/// The report of the look scenario's three trials, one look made.
fn judge_scenario(trials: [TrialSeries; 3]) -> GateReport {
    let mut i = inputs(1, Vec::from(trials), 3);
    i.holdout_looks = 1;
    build_report(&i)
}

/// The holdout gate of a report.
fn holdout_of(report: &GateReport) -> &GateResult {
    find(report, "holdout")
}

#[test]
fn a_rerun_under_other_limits_is_not_the_experiment_the_look_tested() {
    // Control: the same re-run under the same limits is judged, and passes.
    let report = judge_scenario(look_scenario());
    assert_eq!(report.candidate_trial_id, Some(3));
    assert_eq!(status(&report, "holdout"), GateStatus::Pass);

    // The review's scenario: the re-run is strong under other limits. It is the candidate, and
    // the look is not judged for it, so it never passes.
    let mut trials = look_scenario_with(drift(500, 0.011, 0.010));
    trials[2].limits_hash = Some("bbb".to_owned());
    let report = judge_scenario(trials);
    assert_eq!(report.candidate_trial_id, Some(3));
    let gate = holdout_of(&report);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, DIFFERENT_INPUTS);
    assert_ne!(gate.status, GateStatus::Pass);
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(1));
    assert_eq!(report.info["candidate_run"]["limits_hash"], json!("bbb"));
    assert_eq!(report.info["holdout_run"]["limits_hash"], json!("aaa"));
}

/// The old control case: an earlier weak run and a later strong candidate with identical recorded
/// inputs. The strong re-run is not the experiment the look tested, so it is never a pass.
#[test]
fn a_strong_rerun_with_identical_inputs_is_another_experiment() {
    let report = judge_scenario(look_scenario_with(drift(500, 0.011, 0.010)));
    assert_eq!(report.candidate_trial_id, Some(3));
    let gate = holdout_of(&report);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, OTHER_EXPERIMENT);
    assert_eq!(gate.value, None);
    assert_eq!(gate.threshold, None);
}

#[test]
fn a_rerun_with_other_starting_cash_or_seed_is_not_the_experiment_the_look_tested() {
    type Change = fn(&mut TrialSeries);
    let changes: [(&str, Change); 2] = [
        ("starting cash", |t| {
            t.starting_cash = Some("250000".to_owned())
        }),
        ("seed", |t| t.seed = Some(8)),
    ];
    for (what, change) in changes {
        let mut trials = look_scenario();
        change(&mut trials[2]);
        let report = judge_scenario(trials);
        let gate = holdout_of(&report);
        assert_eq!(
            gate.status,
            GateStatus::NotEvaluated,
            "{what}: {}",
            gate.note
        );
        assert_eq!(gate.note, DIFFERENT_INPUTS, "{what}");
        assert_ne!(gate.status, GateStatus::Pass, "{what}");
    }
}

#[test]
fn starting_cash_is_compared_as_a_decimal() {
    for equal in ["100000.0", "100000.00"] {
        let mut trials = look_scenario();
        trials[2].starting_cash = Some(equal.to_owned());
        let report = judge_scenario(trials);
        assert_eq!(
            status(&report, "holdout"),
            GateStatus::Pass,
            "{equal}: {}",
            holdout_of(&report).note
        );
    }

    let mut trials = look_scenario();
    trials[2].starting_cash = Some("100000.5".to_owned());
    assert_eq!(
        holdout_of(&judge_scenario(trials)).note,
        DIFFERENT_INPUTS,
        "a different amount"
    );

    // Not a decimal on either side: never equal, not even to itself.
    let mut trials = look_scenario();
    for trial in &mut trials {
        trial.starting_cash = Some("ten".to_owned());
    }
    assert_eq!(
        holdout_of(&judge_scenario(trials)).note,
        DIFFERENT_INPUTS,
        "not a decimal"
    );
}

#[test]
fn a_missing_limits_cash_or_seed_on_any_of_the_three_runs_is_never_equal() {
    type Blank = fn(&mut TrialSeries);
    let blanks: [(&str, Blank); 3] = [
        ("limits_hash", |t| t.limits_hash = None),
        ("starting_cash", |t| t.starting_cash = None),
        ("seed", |t| t.seed = None),
    ];
    // Trial 1 is the baseline, trial 2 the look, trial 3 the candidate.
    for (side, index) in [("baseline", 0), ("holdout run", 1), ("candidate", 2)] {
        for (field, blank) in blanks {
            let mut trials = look_scenario();
            blank(&mut trials[index]);
            let report = judge_scenario(trials);
            let gate = holdout_of(&report);
            assert_eq!(
                gate.status,
                GateStatus::NotEvaluated,
                "{side} without {field}: {}",
                gate.note
            );
            assert_eq!(gate.value, None, "{side} without {field}");
        }
    }
}

#[test]
fn a_look_that_read_another_bar_file_than_its_baseline_has_no_baseline() {
    let mut other = look_scenario();
    other[1].bars_file = Some("other.parquet".to_owned());
    let mut missing_run = look_scenario();
    missing_run[1].bars_file = None;
    let mut missing_baseline = look_scenario();
    missing_baseline[0].bars_file = None;
    for trials in [other, missing_run, missing_baseline] {
        let report = judge_scenario(trials);
        let gate = holdout_of(&report);
        assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
        assert_eq!(gate.note, NO_BASELINE);
    }

    // Control: the same bar file on both sides is judged.
    let report = judge_scenario(look_scenario());
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(1));
}

#[test]
fn a_baseline_that_ended_flat_is_not_evaluated() {
    let mut trials = look_scenario();
    trials[0].final_drawdown_mode = Some("flat".to_owned());
    let report = judge_scenario(trials);
    let gate = holdout_of(&report);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, FLAT_BASELINE);
    assert_eq!(gate.value, None);
    assert_eq!(gate.threshold, None);
}

#[test]
fn a_baseline_counts_only_its_non_zero_returns_against_252() {
    // The baseline, the look made right after it and an identical re-run of it, judged.
    let judged = |returns: Vec<f64>| {
        let look = holdout_trial(2, 1, extended(&returns, &drift(60, 0.002, 0.010)), Some(60));
        let trials = [
            series(1, 1, false, returns.clone()),
            look,
            series(3, 1, false, returns),
        ];
        holdout_of(&judge_scenario(trials)).clone()
    };

    // The review's idle baseline: 15 non-zero returns, then 485 zeros (500 sessions in all).
    let mut idle = drift(15, 0.011, 0.010);
    idle.extend(vec![0.0; 485]);
    let gate = judged(idle);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, SHORT_BASELINE);

    // Exactly 252 non-zero returns (among zeros) is judged.
    let mut enough = drift(252, 0.011, 0.010);
    enough.extend(vec![0.0; 248]);
    let gate = judged(enough);
    assert_ne!(gate.note, SHORT_BASELINE, "{}", gate.note);
    assert_ne!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);

    // 251 non-zero returns is not.
    let mut short = drift(251, 0.011, 0.010);
    short.extend(vec![0.0; 249]);
    let gate = judged(short);
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, SHORT_BASELINE);
}

/// The same bar file name is not the same bars: a look whose first return differs from the
/// baseline's has no baseline, and the gate is never a pass.
#[test]
fn a_look_on_other_history_under_the_same_bar_file_has_no_baseline() {
    let mut trials = look_scenario();
    let mut changed = trials[1].returns.clone().unwrap();
    changed[0] += 0.001;
    trials[1].returns = Some(changed);
    let gate = holdout_of(&judge_scenario(trials)).clone();
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, NO_BASELINE);
    assert_ne!(gate.status, GateStatus::Pass);
}

/// A look over other requested dates, or whose returns are not longer than the baseline's, has no
/// baseline either.
#[test]
fn a_look_with_another_start_or_no_longer_returns_has_no_baseline() {
    let mut other_start = look_scenario();
    other_start[1].requested_start = 100;
    let gate = holdout_of(&judge_scenario(other_start)).clone();
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, NO_BASELINE);

    let baseline_returns = look_scenario()[0].returns.clone().unwrap();
    for returns in [baseline_returns.clone(), baseline_returns[..400].to_vec()] {
        let mut trials = look_scenario();
        trials[1].returns = Some(returns);
        let gate = holdout_of(&judge_scenario(trials)).clone();
        assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
        assert_eq!(gate.note, NO_BASELINE);
    }
}

/// A run or a baseline whose final drawdown mode was not recorded is not evaluated, since it
/// cannot be told apart from one that went flat.
#[test]
fn an_unrecorded_final_drawdown_mode_is_not_evaluated_on_the_run_or_the_baseline() {
    let mut run_unrecorded = look_scenario();
    run_unrecorded[1].final_drawdown_mode = None;
    let gate = holdout_of(&judge_scenario(run_unrecorded)).clone();
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, RUN_UNRECORDED);
    assert_eq!(gate.value, None);

    let mut baseline_unrecorded = look_scenario();
    baseline_unrecorded[0].final_drawdown_mode = None;
    let gate = holdout_of(&judge_scenario(baseline_unrecorded)).clone();
    assert_eq!(gate.status, GateStatus::NotEvaluated, "{}", gate.note);
    assert_eq!(gate.note, BASELINE_UNRECORDED);
    assert_eq!(gate.value, None);
    assert_eq!(gate.threshold, None);
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
fn risk_breaches_must_be_zero_and_need_a_sent_order() {
    let risk = |breaches: u64, sent: u64| {
        let mut i = inputs(1, vec![], 0);
        i.risk_breaches = breaches;
        i.sent_orders = sent;
        build_report(&i)
    };
    let none_sent = risk(0, 0);
    assert_eq!(
        status(&none_sent, "risk_breaches"),
        GateStatus::NotEvaluated
    );
    assert_eq!(
        find(&none_sent, "risk_breaches").note,
        "this version has sent no order"
    );
    assert_eq!(status(&risk(0, 5), "risk_breaches"), GateStatus::Pass);
    assert_eq!(status(&risk(1, 5), "risk_breaches"), GateStatus::Fail);
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

// ---- pairing the live and tracking returns ----------------------------------------------------

/// January 2026, day `n`.
fn day(n: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 1, n).unwrap()
}

fn days(ns: &[u32]) -> Vec<NaiveDate> {
    ns.iter().map(|&n| day(n)).collect()
}

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

/// Equity rows as `(day, equity)`.
fn equity_rows(rows: &[(u32, &str)]) -> Vec<(NaiveDate, Decimal)> {
    rows.iter()
        .map(|&(n, equity)| (day(n), dec(equity)))
        .collect()
}

/// Asserts two series have the same length and values to within 1e-12.
fn assert_close(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len(), "{actual:?} vs {expected:?}");
    for (a, e) in actual.iter().zip(expected) {
        assert!((a - e).abs() < 1e-12, "{actual:?} vs {expected:?}");
    }
}

fn assert_paired(pair: Option<(Vec<f64>, Vec<f64>)>, live: &[f64], backtest: &[f64]) {
    let (got_live, got_backtest) = pair.expect("the series pair");
    assert_close(&got_live, live);
    assert_close(&got_backtest, backtest);
}

/// The two series cannot be compared: both empty, which the gate reports as not computable.
fn assert_uncomparable(pair: Option<(Vec<f64>, Vec<f64>)>) {
    assert_eq!(pair, Some((Vec::new(), Vec::new())));
}

#[test]
fn pair_tracking_needs_two_equity_rows() {
    let returns = [0.0, 0.1];
    assert_eq!(
        pair_tracking(&equity_rows(&[]), &days(&[5, 6]), &returns),
        None
    );
    assert_eq!(
        pair_tracking(&equity_rows(&[(5, "100")]), &days(&[5, 6]), &returns),
        None
    );
}

#[test]
fn pair_tracking_is_none_without_a_tracking_date_on_or_after_the_second_equity_row() {
    let eq = equity_rows(&[(5, "100"), (6, "110")]);
    assert_eq!(pair_tracking(&eq, &[], &[]), None);
    // A tracking date only before the second equity row: nothing to compare yet.
    assert_eq!(pair_tracking(&eq, &days(&[4, 5]), &[0.0, 0.0]), None);
}

#[test]
fn pair_tracking_pairs_the_live_and_backtest_returns_of_the_same_sessions() {
    let eq = equity_rows(&[(5, "100"), (6, "110"), (7, "99")]);
    let pair = pair_tracking(&eq, &days(&[5, 6, 7]), &[0.0, 0.05, -0.02]);
    assert_paired(pair, &[0.10, -0.10], &[0.05, -0.02]);
}

#[test]
fn pair_tracking_uses_only_the_tail_of_a_long_tracking_run() {
    let eq = equity_rows(&[(7, "100"), (8, "101"), (9, "103")]);
    let dates = days(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    let returns = [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.004, 0.02];
    let pair = pair_tracking(&eq, &dates, &returns);
    assert_paired(pair, &[0.01, 2.0 / 101.0], &[0.004, 0.02]);
}

#[test]
fn pair_tracking_pairs_from_the_second_equity_row_when_the_run_starts_later() {
    let eq = equity_rows(&[(5, "100"), (6, "110"), (7, "99")]);
    let pair = pair_tracking(&eq, &days(&[6, 7]), &[0.05, -0.02]);
    assert_paired(pair, &[0.10, -0.10], &[0.05, -0.02]);
}

#[test]
fn pair_tracking_compares_only_up_to_the_last_tracking_date() {
    let eq = equity_rows(&[(5, "100"), (6, "110"), (7, "99"), (8, "105")]);
    let pair = pair_tracking(&eq, &days(&[5, 6]), &[0.0, 0.05]);
    assert_paired(pair, &[0.10], &[0.05]);
}

#[test]
fn pair_tracking_with_an_equity_row_missing_inside_the_span_is_uncomparable() {
    // Day 7 is in the tracking run, but the equity series has no row for it.
    let eq = equity_rows(&[(5, "100"), (6, "110"), (8, "99")]);
    let pair = pair_tracking(&eq, &days(&[5, 6, 7, 8]), &[0.0, 0.05, 0.01, -0.02]);
    assert_uncomparable(pair);
}

#[test]
fn pair_tracking_with_an_equity_row_the_tracking_run_lacks_is_uncomparable() {
    let eq = equity_rows(&[(5, "100"), (6, "110"), (7, "99")]);
    assert_uncomparable(pair_tracking(&eq, &days(&[5, 7]), &[0.0, 0.01]));
}

#[test]
fn pair_tracking_with_a_zero_or_negative_previous_equity_is_uncomparable() {
    let zero = equity_rows(&[(5, "0"), (6, "110")]);
    assert_uncomparable(pair_tracking(&zero, &days(&[5, 6]), &[0.0, 0.05]));
    let negative = equity_rows(&[(5, "-5"), (6, "110")]);
    assert_uncomparable(pair_tracking(&negative, &days(&[5, 6]), &[0.0, 0.05]));
}

#[test]
fn pair_tracking_with_different_lengths_is_uncomparable() {
    let eq = equity_rows(&[(5, "100"), (6, "110"), (7, "99")]);
    assert_uncomparable(pair_tracking(&eq, &days(&[5, 6, 7]), &[0.0, 0.05]));
    assert_uncomparable(pair_tracking(&eq, &days(&[5, 6]), &[0.0, 0.05, -0.02]));
}

#[test]
fn pair_tracking_with_unsorted_or_repeated_tracking_dates_is_uncomparable() {
    let eq = equity_rows(&[(5, "100"), (6, "110"), (7, "99")]);
    assert_uncomparable(pair_tracking(&eq, &days(&[5, 7, 6]), &[0.0, -0.02, 0.05]));
    assert_uncomparable(pair_tracking(&eq, &days(&[5, 6, 6]), &[0.0, 0.05, 0.05]));
}

#[test]
fn pair_tracking_with_equity_out_of_date_order_is_uncomparable() {
    let eq = equity_rows(&[(6, "110"), (5, "100")]);
    assert_uncomparable(pair_tracking(&eq, &days(&[5, 6]), &[0.0, 0.05]));
}

// ---- the tracking gate from the store -------------------------------------------------------

/// Records the equity of `version` in `environment` for each `(day, equity)` row of January 2026.
async fn record_equity(
    client: &Client,
    version: StrategyVersionId,
    environment: &str,
    rows: &[(u32, &str)],
) {
    for &(n, equity) in rows {
        account_equity::insert(
            client,
            version,
            environment,
            day(n),
            dec(equity),
            UnixNanos::from(T),
        )
        .await
        .unwrap();
    }
}

/// Registers a tracking run of `version` with its returns over `dates` and returns its trial id.
async fn tracking_run(
    client: &Client,
    version: StrategyVersionId,
    dates: &[NaiveDate],
    returns: &[f64],
) -> i64 {
    let id = register(
        client,
        version,
        TrialKind::Backtest,
        Some(json!({"tracking": true})),
    )
    .await;
    let mut result = stored_result(json!(returns), false);
    result["session_dates"] = json!(dates.iter().map(ToString::to_string).collect::<Vec<_>>());
    trial_result::insert(client, id, &result, UnixNanos::from(T))
        .await
        .unwrap();
    id
}

#[tokio::test]
async fn the_tracking_gate_is_not_evaluated_without_equity_rows() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    tracking_run(&client, v, &days(&[5, 6, 7]), &[0.0, 0.1, -0.1]).await;

    let inputs = load_inputs(&client, v, gates()).await.unwrap();
    assert_eq!(inputs.live, None);
    let report = build_report(&inputs);
    let gate = find(&report, "tracking");
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert!(gate.note.contains("no live returns yet"), "{}", gate.note);

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn the_tracking_gate_is_judged_from_equity_rows_and_a_tracking_run() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    // Equity 100, 110, 99, 108.9: daily returns 0.10, -0.10 and 0.10.
    record_equity(
        &client,
        v,
        "paper",
        &[(5, "100"), (6, "110"), (7, "99"), (8, "108.9")],
    )
    .await;
    tracking_run(&client, v, &days(&[5, 6, 7, 8]), &[0.0, 0.1, -0.1, 0.1]).await;

    let inputs = load_inputs(&client, v, gates()).await.unwrap();
    let (live, backtest) = inputs.live.as_ref().expect("a paired series");
    assert_close(live, &[0.1, -0.1, 0.1]);
    assert_close(backtest, &[0.1, -0.1, 0.1]);
    let report = build_report(&inputs);
    let gate = find(&report, "tracking");
    assert_eq!(gate.status, GateStatus::Pass);
    assert!(gate.value.expect("a tracking error").abs() < 1e-12);

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn live_equity_is_preferred_over_paper() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    record_equity(&client, v, "paper", &[(5, "100"), (6, "110"), (7, "99")]).await;
    record_equity(&client, v, "live", &[(5, "100"), (6, "120"), (7, "90")]).await;
    tracking_run(&client, v, &days(&[5, 6, 7]), &[0.0, 0.2, -0.25]).await;

    let inputs = load_inputs(&client, v, gates()).await.unwrap();
    let (live, backtest) = inputs.live.as_ref().expect("a paired series");
    // The live rows give 0.2 and -0.25; paper's would give 0.1 and -0.1.
    assert_close(live, &[0.2, -0.25]);
    assert_close(backtest, &[0.2, -0.25]);

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_missed_session_makes_the_tracking_gate_not_evaluated() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    // The equity has no row for day 7, which the tracking run has.
    record_equity(&client, v, "paper", &[(5, "100"), (6, "110"), (8, "108.9")]).await;
    tracking_run(&client, v, &days(&[5, 6, 7, 8]), &[0.0, 0.1, -0.1, 0.1]).await;

    let inputs = load_inputs(&client, v, gates()).await.unwrap();
    assert_eq!(inputs.live, Some((Vec::new(), Vec::new())));
    let report = build_report(&inputs);
    let gate = find(&report, "tracking");
    assert_eq!(gate.status, GateStatus::NotEvaluated);
    assert!(gate.note.contains("could not be computed"), "{}", gate.note);

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_malformed_tracking_result_leaves_the_gate_uncomputable() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    record_equity(&client, v, "paper", &[(5, "100"), (6, "110"), (7, "99")]).await;
    let id = register(
        &client,
        v,
        TrialKind::Backtest,
        Some(json!({"tracking": true})),
    )
    .await;
    let mut result = stored_result(json!([0.0, 0.1, -0.1]), false);
    result["session_dates"] = json!(["2026-01-05", "not a date", "2026-01-07"]);
    trial_result::insert(&client, id, &result, UnixNanos::from(T))
        .await
        .unwrap();

    let inputs = load_inputs(&client, v, gates()).await.unwrap();
    assert_eq!(inputs.live, Some((Vec::new(), Vec::new())));
    assert_eq!(
        status(&build_report(&inputs), "tracking"),
        GateStatus::NotEvaluated
    );

    drop(client);
    db.drop_db().await.unwrap();
}

#[tokio::test]
async fn a_tracking_run_never_becomes_the_candidate() {
    let db = ScratchDb::create().await;
    let client = db.client().await.unwrap();
    let v = version(&client, 1).await;
    let ordinary = register(&client, v, TrialKind::Backtest, None).await;
    trial_result::insert(
        &client,
        ordinary,
        &stored_result(json!([0.01, -0.02]), false),
        UnixNanos::from(T),
    )
    .await
    .unwrap();
    // The tracking run is newer, so it would be the candidate if it were a trial.
    tracking_run(&client, v, &days(&[5, 6]), &[0.0, 0.1]).await;

    let inputs = load_inputs(&client, v, gates()).await.unwrap();
    let report = build_report(&inputs);
    assert_eq!(report.candidate_trial_id, Some(ordinary));

    drop(client);
    db.drop_db().await.unwrap();
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
        "candidate_run",
        "holdout_run",
    ] {
        assert!(report.info[key].is_null(), "{key} is not null");
    }
}

#[test]
fn candidate_run_and_holdout_run_carry_the_documented_keys() {
    let cand = drift(500, 0.001, 0.010);
    let candidate = TrialSeries {
        costs: json!({"slippage_bps": "2", "fee_per_share": "0.0001"}),
        symbols: json!(["SPY"]),
        ..series(1, 1, false, cand.clone())
    };
    let holdout = TrialSeries {
        costs: json!({"slippage_bps": "2", "fee_per_share": "0.0001"}),
        symbols: json!(["SPY"]),
        ..holdout_trial(2, 1, extended(&cand, &drift(60, 0.001, 0.010)), Some(60))
    };
    let trials = vec![candidate, holdout];
    let report = build_report(&inputs(1, trials, 2));

    let run = &report.info["candidate_run"];
    for key in [
        "dirty_build",
        "costs",
        "symbols",
        "limits_hash",
        "starting_cash",
        "seed",
        "bars_file",
        "requested_start",
        "requested_end",
        "data_end",
        "holdout_start",
        "usable",
        "not_usable_reason",
    ] {
        assert!(run.get(key).is_some(), "candidate_run misses {key}");
    }
    assert_eq!(run["dirty_build"], json!(false));
    assert_eq!(
        run["costs"],
        json!({"slippage_bps": "2", "fee_per_share": "0.0001"})
    );
    assert_eq!(run["symbols"], json!(["SPY"]));
    assert_eq!(run["limits_hash"], json!("aaa"));
    assert_eq!(run["starting_cash"], json!("100000"));
    assert_eq!(run["seed"], json!(7));
    assert_eq!(run["bars_file"], json!("bars.parquet"));
    assert_eq!(run["requested_end"], json!(1000));
    assert_eq!(run["data_end"], json!(BOUNDARY - 1));
    assert_eq!(run["holdout_start"], json!("2023-01-01"));
    assert_eq!(run["usable"], json!(true));
    assert!(run["not_usable_reason"].is_null());

    let holdout = &report.info["holdout_run"];
    for key in [
        "trial_id",
        "holdout_sessions",
        "final_drawdown_mode",
        "sharpe",
        "costs",
        "symbols",
        "limits_hash",
        "starting_cash",
        "seed",
        "bars_file",
        "baseline_trial_id",
        "holdout_runs_for_version",
    ] {
        assert!(holdout.get(key).is_some(), "holdout_run misses {key}");
    }
    assert_eq!(holdout["limits_hash"], json!("aaa"));
    assert_eq!(holdout["starting_cash"], json!("100000"));
    assert_eq!(holdout["seed"], json!(7));
    assert_eq!(holdout["bars_file"], json!("bars.parquet"));
    assert_eq!(holdout["baseline_trial_id"], json!(1));
    assert_eq!(holdout["trial_id"], json!(2));
    assert_eq!(holdout["holdout_sessions"], json!(60));
    assert_eq!(holdout["final_drawdown_mode"], json!("normal"));
    assert_eq!(
        holdout["costs"],
        json!({"slippage_bps": "2", "fee_per_share": "0.0001"})
    );
    assert_eq!(holdout["symbols"], json!(["SPY"]));
    assert_eq!(holdout["holdout_runs_for_version"], json!(1));
}

#[test]
fn an_unusable_candidate_run_gives_its_reason() {
    let candidate = TrialSeries {
        data_end: None,
        ..series(1, 1, false, drift(500, 0.001, 0.010))
    };
    let report = build_report(&inputs(1, vec![candidate], 1));
    assert_eq!(report.info["candidate_run"]["usable"], json!(false));
    assert_eq!(
        report.info["candidate_run"]["not_usable_reason"],
        json!("its data reaches into the holdout, or its end is not recorded")
    );
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

/// Every gate passes: a strong candidate, ten usable configurations with the candidate
/// dominant, a usable holdout run of 60 sessions whose tail clears the haircut, replay (0, 3),
/// five orders sent with no breach, identical live and backtest returns.
fn passing_inputs() -> ReportInputs {
    let cand = drift(500, 0.011, 0.010);
    let alts = alternatives(9);
    let mut trials = configurations(&cand, &alts);
    trials.push(holdout_trial(
        11,
        1,
        extended(&cand, &drift(60, 0.011, 0.010)),
        Some(60),
    ));
    let mut i = inputs(1, trials, 11);
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
    // The candidate (trial 10) was registered before the look, so it is its own baseline.
    assert_eq!(report.info["holdout_run"]["baseline_trial_id"], json!(10));

    // Each flip breaks exactly the gate named, and only that one must be non-passing.
    type Flip = fn(&mut ReportInputs);
    let flips: [(&str, Flip); 7] = [
        ("trial_budget", |i| i.lifetime_trials = 101),
        ("deflated_sharpe", |i| i.lifetime_trials = 30),
        ("pbo", |i| {
            for trial in &mut i.trials {
                if trial.trial_id < 10 {
                    trial.returns = Some(drift(500, 0.011, 0.010));
                }
            }
        }),
        ("holdout", |i| i.holdout_looks = 11),
        ("replay", |i| i.replay = (1, 3)),
        ("risk_breaches", |i| i.risk_breaches = 1),
        ("tracking", |i| {
            i.live = Some((drift(500, 0.01, -0.010), drift(500, 0.011, 0.010)));
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
fn the_run_fields_are_read_when_present() {
    let series = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01], "returns_complete": true,
        "dirty_build": false,
        "holdout_start": "2023-01-01",
        "data_end": 42,
        "holdout_sessions": 7,
        "final_drawdown_mode": "halved",
        "costs": {"slippage_bps": "2"},
        "symbols": ["SPY", "QQQ"],
    })));
    assert_eq!(series.dirty_build, Some(false));
    assert_eq!(series.holdout_start.as_deref(), Some("2023-01-01"));
    assert_eq!(series.data_end, Some(42));
    assert_eq!(series.holdout_sessions, Some(7));
    assert_eq!(series.final_drawdown_mode.as_deref(), Some("halved"));
    assert_eq!(series.costs, json!({"slippage_bps": "2"}));
    assert_eq!(series.symbols, json!(["SPY", "QQQ"]));
}

#[test]
fn mistyped_or_absent_run_fields_are_unknown() {
    let series = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01], "returns_complete": true,
        "dirty_build": "no",
        "holdout_start": 2023,
        "data_end": -1,
        "holdout_sessions": "7",
        "final_drawdown_mode": 1,
    })));
    assert_eq!(series.dirty_build, None);
    assert_eq!(series.holdout_start, None);
    assert_eq!(series.data_end, None);
    assert_eq!(series.holdout_sessions, None);
    assert_eq!(series.final_drawdown_mode, None);
    assert_eq!(series.costs, Value::Null);
    assert_eq!(series.symbols, Value::Null);
    assert!(series.usable(&gates()).is_err());
}

#[test]
fn a_complete_stored_result_of_a_clean_ordinary_run_is_usable() {
    let series = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01, -0.02], "returns_complete": true,
        "dirty_build": false,
        "holdout_start": "2023-01-01",
        "data_end": BOUNDARY - 1,
    })));
    assert_eq!(series.usable(&gates()), Ok(()));
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
    assert_eq!(older.dirty_build, None);
    assert_eq!(older.holdout_start, None);
    assert_eq!(older.data_end, None);
    assert_eq!(older.costs, Value::Null);
}

#[test]
fn the_comparison_fields_are_read_when_present() {
    let series = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01], "returns_complete": true,
        "limits_hash": "abc",
        "starting_cash": "100000.0",
        "seed": 42,
        "bars_file": "bars.parquet",
    })));
    assert_eq!(series.limits_hash.as_deref(), Some("abc"));
    assert_eq!(series.starting_cash.as_deref(), Some("100000.0"));
    assert_eq!(series.seed, Some(42));
    assert_eq!(series.bars_file.as_deref(), Some("bars.parquet"));
}

#[test]
fn absent_or_mistyped_comparison_fields_are_unknown() {
    let absent = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01], "returns_complete": true,
    })));
    assert_eq!(absent.limits_hash, None);
    assert_eq!(absent.starting_cash, None);
    assert_eq!(absent.seed, None);
    assert_eq!(absent.bars_file, None);

    let mistyped = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01], "returns_complete": true,
        "limits_hash": 7,
        "starting_cash": 100000,
        "seed": "42",
        "bars_file": null,
    })));
    assert_eq!(mistyped.limits_hash, None);
    assert_eq!(mistyped.starting_cash, None);
    assert_eq!(mistyped.seed, None);
    assert_eq!(mistyped.bars_file, None);

    let negative_seed = TrialSeries::from_stored(&stored(json!({
        "returns": [0.01], "returns_complete": true,
        "seed": -1,
    })));
    assert_eq!(negative_seed.seed, None);
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

/// A backtest result as `backtest_job` stores it, with the given returns. Results stored
/// before the run fields existed have no build state, so they are not usable.
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
    assert!(inputs.trials[0].usable(&gates()).is_err());
    assert_eq!(inputs.replay, (0, 0));
    assert_eq!(inputs.risk_breaches, 0);
    assert_eq!(inputs.sent_orders, 0);
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
