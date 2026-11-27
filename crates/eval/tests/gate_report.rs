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

/// A usable backtest trial over the snapshot "snap" and the range 0..1000: a clean build, the
/// file's holdout boundary, an ordinary run ending the day before it (or a holdout run with its
/// end recorded). A holdout run's sessions are all of its returns.
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
        costs: Value::Null,
        symbols: Value::Null,
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

#[test]
fn pbo_with_a_dominated_alternative_passes_at_zero() {
    let cand = drift(500, 0.01, 0.010);
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
    let cand = drift(500, 0.01, 0.010);
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
    let cand = drift(500, 0.01, 0.010);
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
    let cand = drift(500, 0.01, 0.010);
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

fn holdout_gate_of(trials: Vec<TrialSeries>, looks: u64) -> GateResult {
    let mut i = inputs(1, trials, 2);
    i.holdout_looks = looks;
    find(&build_report(&i), "holdout").clone()
}

/// The annualised Sharpe of `returns`, as the gate reports it.
fn annualised_sharpe(returns: &[f64]) -> f64 {
    moments::annualise(moments::sharpe(returns).unwrap(), gates().periods_per_year)
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
    let own = series(2, 1, true, cand.clone());
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
    let cand = drift(500, 0.01, 0.010);
    let base = vec![series(1, 1, false, cand.clone())];
    let haircut = (1.0 - gates().sharpe_haircut) * annualised_sharpe(&cand);
    let good = || holdout_trial(2, 1, cand.clone(), Some(60));

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

    // 4. Fewer than 20 sessions, none recorded, or more than the run has.
    for sessions in [Some(19), None, Some(501)] {
        let gate = holdout_gate_of(
            base.iter()
                .cloned()
                .chain([holdout_trial(2, 1, cand.clone(), sessions)])
                .collect(),
            0,
        );
        assert_eq!(gate.status, GateStatus::NotEvaluated, "{sessions:?}");
        assert_eq!(gate.note, "fewer than 20 holdout sessions", "{sessions:?}");
    }

    // 5. Ended flat after a drawdown: a fail, even with a good Sharpe.
    let flat = TrialSeries {
        final_drawdown_mode: Some("flat".to_owned()),
        ..good()
    };
    let gate = holdout_gate_of(base.iter().cloned().chain([flat]).collect(), 0);
    assert_eq!(gate.status, GateStatus::Fail);
    assert!(gate.note.contains("ended flat"), "{}", gate.note);

    // 6. The last 60 sessions below the haircut Sharpe: a fail.
    let weak_returns = drift(500, 0.0, 0.010);
    let weak = holdout_trial(2, 1, weak_returns.clone(), Some(60));
    let gate = holdout_gate_of(base.iter().cloned().chain([weak]).collect(), 0);
    assert_eq!(gate.status, GateStatus::Fail);
    assert_eq!(gate.value, Some(annualised_sharpe(&weak_returns[440..])));
    assert_eq!(gate.threshold, Some(haircut));

    // 7. At or above the haircut Sharpe: a pass, on the last 60 sessions (or the last 20).
    for sessions in [60, 20] {
        let gate = holdout_gate_of(
            base.iter()
                .cloned()
                .chain([holdout_trial(2, 1, cand.clone(), Some(sessions))])
                .collect(),
            0,
        );
        assert_eq!(gate.status, GateStatus::Pass, "{sessions}");
        let tail = &cand[cand.len() - sessions as usize..];
        assert_eq!(gate.value, Some(annualised_sharpe(tail)));
        assert_eq!(gate.threshold, Some(haircut));
    }
}

#[test]
fn the_holdout_is_judged_on_its_last_sessions_only() {
    let cand = drift(500, 0.01, 0.010);
    // The whole run is weak (about 0.1 a session against a candidate of 1.0); its last 60
    // sessions are as strong as the candidate.
    let mut returns = drift(440, 0.0, 0.010);
    returns.extend(drift(60, 0.01, 0.010));
    let trials = vec![
        series(1, 1, false, cand),
        holdout_trial(2, 1, returns.clone(), Some(60)),
    ];
    let gate = holdout_gate_of(trials, 0);
    assert_eq!(gate.status, GateStatus::Pass);
    assert_eq!(gate.value, Some(annualised_sharpe(&returns[440..])));
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
    let trials = vec![candidate, holdout_trial(2, 1, cand, Some(60))];
    let report = build_report(&inputs(1, trials, 2));

    let run = &report.info["candidate_run"];
    for key in [
        "dirty_build",
        "costs",
        "symbols",
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
    ] {
        assert!(holdout.get(key).is_some(), "holdout_run misses {key}");
    }
    assert_eq!(holdout["trial_id"], json!(2));
    assert_eq!(holdout["holdout_sessions"], json!(60));
    assert_eq!(holdout["final_drawdown_mode"], json!("normal"));
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
    let cand = drift(500, 0.01, 0.010);
    let alts = alternatives(9);
    let mut trials = configurations(&cand, &alts);
    trials.push(holdout_trial(11, 1, cand.clone(), Some(60)));
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

    // Each flip breaks exactly the gate named, and only that one must be non-passing.
    type Flip = fn(&mut ReportInputs);
    let flips: [(&str, Flip); 7] = [
        ("trial_budget", |i| i.lifetime_trials = 101),
        ("deflated_sharpe", |i| i.lifetime_trials = 30),
        ("pbo", |i| {
            for trial in &mut i.trials {
                if trial.trial_id < 10 {
                    trial.returns = Some(drift(500, 0.01, 0.010));
                }
            }
        }),
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
