//! Making, recording and replaying decisions.

use nisse_core::{
    Decision, DecisionId, DecisionLog, Features, StrategyVersionId, feature_hash,
    nautilus::{Symbol, UnixNanos},
};

use crate::{EngineError, Strategy};

/// Everything about one decision point that does not come from the strategy.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionContext {
    pub strategy_version_id: StrategyVersionId,
    pub symbol: Symbol,
    pub features: Features,
    /// Names the market data the features were computed from.
    pub data_snapshot_id: String,
    pub rng_seed: u64,
    /// From the engine's injected clock.
    pub ts: UnixNanos,
}

/// The result of replaying a recorded decision.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayOutcome {
    /// The strategy made the same decision again.
    Match,
    /// The stored `feature_hash` is not the hash of the stored features: the row was
    /// corrupted or tampered with, so there is nothing sound to replay.
    FeatureHashMismatch,
    /// The strategy decided differently. `field` names the first difference found, checked
    /// in this order: `action`, `intended_qty`, `reason_code`, `expected_edge_bps`,
    /// `expected_vol`.
    Mismatch {
        field: &'static str,
        recorded: String,
        replayed: String,
    },
}

/// Builds the [`Decision`] a strategy makes in `context`, without recording it.
///
/// Calls `strategy.decide(&context.features, context.rng_seed)` exactly once, and computes
/// `feature_hash` with [`nisse_core::feature_hash`].
#[must_use]
pub fn decide<S: Strategy + ?Sized>(strategy: &S, context: DecisionContext) -> Decision {
    let verdict = strategy.decide(&context.features, context.rng_seed);
    let hash = feature_hash(&context.features);
    Decision {
        strategy_version_id: context.strategy_version_id,
        symbol: context.symbol,
        action: verdict.action,
        intended_qty: verdict.intended_qty,
        expected_edge_bps: verdict.expected_edge_bps,
        expected_vol: verdict.expected_vol,
        features: context.features,
        feature_hash: hash,
        data_snapshot_id: context.data_snapshot_id,
        rng_seed: context.rng_seed,
        reason_code: verdict.reason_code.to_owned(),
        ts: context.ts,
    }
}

/// Makes a decision and records it, returning its id. Every decision is recorded, holds and
/// skips included. If the log fails the decision is returned to nobody: the caller gets
/// `DecisionNotRecorded` and must not act on a decision that is not stored.
pub async fn decide_and_record<S: Strategy + ?Sized, L: DecisionLog>(
    strategy: &S,
    log: &L,
    context: DecisionContext,
) -> Result<(DecisionId, Decision), EngineError> {
    let decision = decide(strategy, context);
    let id = log
        .record_decision(&decision)
        .await
        .map_err(|e| EngineError::DecisionNotRecorded(Box::new(e)))?;
    Ok((id, decision))
}

/// Replays a recorded decision: checks the feature hash, runs the strategy on the recorded
/// features and seed, and compares the result with what was recorded. `f64` fields are
/// compared by bit pattern, so `NaN` equals `NaN` and `0.0` differs from `-0.0`: replay asks
/// for the same bits, not for closeness.
#[must_use]
pub fn replay<S: Strategy + ?Sized>(strategy: &S, recorded: &Decision) -> ReplayOutcome {
    if feature_hash(&recorded.features) != recorded.feature_hash {
        return ReplayOutcome::FeatureHashMismatch;
    }
    let verdict = strategy.decide(&recorded.features, recorded.rng_seed);

    if verdict.action != recorded.action {
        return mismatch(
            "action",
            recorded.action.to_string(),
            verdict.action.to_string(),
        );
    }
    if verdict.intended_qty != recorded.intended_qty {
        return mismatch(
            "intended_qty",
            recorded.intended_qty.to_string(),
            verdict.intended_qty.to_string(),
        );
    }
    if verdict.reason_code != recorded.reason_code.as_str() {
        return mismatch(
            "reason_code",
            recorded.reason_code.clone(),
            verdict.reason_code.to_owned(),
        );
    }
    if verdict.expected_edge_bps.map(f64::to_bits) != recorded.expected_edge_bps.map(f64::to_bits) {
        return mismatch(
            "expected_edge_bps",
            format!("{:?}", recorded.expected_edge_bps),
            format!("{:?}", verdict.expected_edge_bps),
        );
    }
    if verdict.expected_vol.map(f64::to_bits) != recorded.expected_vol.map(f64::to_bits) {
        return mismatch(
            "expected_vol",
            format!("{:?}", recorded.expected_vol),
            format!("{:?}", verdict.expected_vol),
        );
    }
    ReplayOutcome::Match
}

/// The one place a [`ReplayOutcome::Mismatch`] is built.
fn mismatch(field: &'static str, recorded: String, replayed: String) -> ReplayOutcome {
    ReplayOutcome::Mismatch {
        field,
        recorded,
        replayed,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        str::FromStr,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use nisse_core::{Action, nautilus::Symbol};
    use rust_decimal::Decimal;

    use super::*;
    use crate::Verdict;

    /// Decides from the `"signal"` feature: absent is `Skip`, above `level` is `Enter`, else
    /// `Hold`. Counts its calls.
    struct Threshold {
        level: Decimal,
        calls: AtomicUsize,
    }

    impl Threshold {
        fn new(level: &str) -> Self {
            Self {
                level: Decimal::from_str(level).unwrap(),
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Strategy for Threshold {
        fn name(&self) -> &'static str {
            "threshold"
        }

        fn decide(&self, features: &Features, _seed: u64) -> Verdict {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let (action, intended_qty, reason_code) = match features.get("signal") {
                None => (Action::Skip, Decimal::ZERO, "no_signal"),
                Some(v) if *v > self.level => (Action::Enter, Decimal::new(10, 0), "above"),
                Some(_) => (Action::Hold, Decimal::ZERO, "below"),
            };
            Verdict {
                action,
                intended_qty,
                expected_edge_bps: Some(12.5),
                expected_vol: Some(0.15),
                reason_code,
            }
        }
    }

    /// Enters on an even seed, exits on an odd one.
    struct Seeded;

    impl Strategy for Seeded {
        fn name(&self) -> &'static str {
            "seeded"
        }

        fn decide(&self, _features: &Features, seed: u64) -> Verdict {
            if seed.is_multiple_of(2) {
                Verdict {
                    action: Action::Enter,
                    intended_qty: Decimal::new(1, 0),
                    expected_edge_bps: None,
                    expected_vol: None,
                    reason_code: "even_seed",
                }
            } else {
                Verdict {
                    action: Action::Exit,
                    intended_qty: Decimal::new(-1, 0),
                    expected_edge_bps: None,
                    expected_vol: None,
                    reason_code: "odd_seed",
                }
            }
        }
    }

    /// Returns `NaN` edge statistics.
    struct NanStats;

    impl Strategy for NanStats {
        fn name(&self) -> &'static str {
            "nan_stats"
        }

        fn decide(&self, _features: &Features, _seed: u64) -> Verdict {
            Verdict {
                action: Action::Hold,
                intended_qty: Decimal::ZERO,
                expected_edge_bps: Some(f64::NAN),
                expected_vol: None,
                reason_code: "nan_stats",
            }
        }
    }

    /// Returns a `+0.0` edge, so replay can be shown to tell it from `-0.0`.
    struct ZeroEdge;

    impl Strategy for ZeroEdge {
        fn name(&self) -> &'static str {
            "zero_edge"
        }

        fn decide(&self, _features: &Features, _seed: u64) -> Verdict {
            Verdict {
                action: Action::Hold,
                intended_qty: Decimal::ZERO,
                expected_edge_bps: Some(0.0),
                expected_vol: None,
                reason_code: "zero_edge",
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("log is down")]
    struct LogError;

    /// Stores decisions in memory and numbers them 1, 2, 3...
    struct MemoryLog {
        rows: Mutex<Vec<Decision>>,
        fail: bool,
    }

    impl MemoryLog {
        fn new(fail: bool) -> Self {
            Self {
                rows: Mutex::new(Vec::new()),
                fail,
            }
        }

        fn rows(&self) -> Vec<Decision> {
            self.rows.lock().unwrap().clone()
        }
    }

    impl DecisionLog for MemoryLog {
        type Error = LogError;

        fn record_decision(
            &self,
            decision: &Decision,
        ) -> impl Future<Output = Result<DecisionId, Self::Error>> + Send {
            let result = if self.fail {
                Err(LogError)
            } else {
                let mut rows = self.rows.lock().unwrap();
                rows.push(decision.clone());
                Ok(DecisionId(i64::try_from(rows.len()).unwrap()))
            };
            std::future::ready(result)
        }
    }

    /// A context with a fixed id, symbol, snapshot and timestamp. `signal` is written into
    /// the features as `"signal"` when given, alongside an unrelated `"pos"` feature.
    fn context(signal: Option<&str>, seed: u64) -> DecisionContext {
        let mut features = Features::new();
        features.insert("pos".to_owned(), Decimal::ZERO);
        if let Some(s) = signal {
            features.insert("signal".to_owned(), Decimal::from_str(s).unwrap());
        }
        DecisionContext {
            strategy_version_id: StrategyVersionId(5),
            symbol: Symbol::new("SPY"),
            features,
            data_snapshot_id: "snap-1".to_owned(),
            rng_seed: seed,
            ts: UnixNanos::from(1_000_u64),
        }
    }

    /// A decision made by `Threshold(1.0)` on signal 2 with seed 9.
    fn enter_decision() -> Decision {
        decide(&Threshold::new("1.0"), context(Some("2"), 9))
    }

    #[test]
    fn decide_copies_every_context_field() {
        let strategy = Threshold::new("1.0");
        let ctx = context(Some("1.50"), 42);
        let expected_features = ctx.features.clone();
        let expected_hash = feature_hash(&expected_features);

        let decision = decide(&strategy, ctx);

        assert_eq!(strategy.calls(), 1);
        assert_eq!(
            decision,
            Decision {
                strategy_version_id: StrategyVersionId(5),
                symbol: Symbol::new("SPY"),
                action: Action::Enter,
                intended_qty: Decimal::new(10, 0),
                expected_edge_bps: Some(12.5),
                expected_vol: Some(0.15),
                features: expected_features,
                feature_hash: expected_hash,
                data_snapshot_id: "snap-1".to_owned(),
                rng_seed: 42,
                reason_code: "above".to_owned(),
                ts: UnixNanos::from(1_000_u64),
            }
        );
    }

    #[test]
    fn decide_accepts_a_boxed_strategy() {
        let strategy: Box<dyn Strategy> = Box::new(Threshold::new("1.0"));
        let decision = decide(&strategy, context(Some("0.5"), 0));
        assert_eq!(decision.action, Action::Hold);
        assert_eq!(decision.reason_code, "below");
    }

    #[test]
    fn decide_maps_hold_and_skip() {
        let strategy = Threshold::new("1.0");

        let hold = decide(&strategy, context(Some("0.5"), 0));
        assert_eq!(hold.action, Action::Hold);
        assert_eq!(hold.intended_qty, Decimal::ZERO);
        assert_eq!(hold.reason_code, "below");

        let skip = decide(&strategy, context(None, 0));
        assert_eq!(skip.action, Action::Skip);
        assert_eq!(skip.intended_qty, Decimal::ZERO);
        assert_eq!(skip.reason_code, "no_signal");
        assert_eq!(strategy.calls(), 2);
    }

    #[test]
    fn decide_uses_the_seed() {
        let even = decide(&Seeded, context(None, 2));
        assert_eq!(even.action, Action::Enter);
        assert_eq!(even.intended_qty, Decimal::new(1, 0));
        assert_eq!(even.rng_seed, 2);

        let odd = decide(&Seeded, context(None, 3));
        assert_eq!(odd.action, Action::Exit);
        assert_eq!(odd.intended_qty, Decimal::new(-1, 0));
        assert_eq!(odd.reason_code, "odd_seed");
    }

    #[tokio::test]
    async fn decide_and_record_stores_the_returned_decision() {
        let strategy = Threshold::new("1.0");
        let log = MemoryLog::new(false);

        let (id, decision) = decide_and_record(&strategy, &log, context(Some("2"), 1))
            .await
            .unwrap();

        assert_eq!(id, DecisionId(1));
        assert_eq!(strategy.calls(), 1);
        let rows = log.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], decision);
    }

    #[tokio::test]
    async fn decide_and_record_records_holds_and_skips() {
        let strategy = Threshold::new("1.0");
        let log = MemoryLog::new(false);

        let (hold_id, _) = decide_and_record(&strategy, &log, context(Some("0.5"), 0))
            .await
            .unwrap();
        let (skip_id, _) = decide_and_record(&strategy, &log, context(None, 0))
            .await
            .unwrap();

        assert_eq!(hold_id, DecisionId(1));
        assert_eq!(skip_id, DecisionId(2));
        let actions: Vec<Action> = log.rows().iter().map(|d| d.action).collect();
        assert_eq!(actions, vec![Action::Hold, Action::Skip]);
    }

    #[tokio::test]
    async fn decide_and_record_fails_when_the_log_fails() {
        let strategy = Threshold::new("1.0");
        let log = MemoryLog::new(true);

        let result = decide_and_record(&strategy, &log, context(Some("2"), 0)).await;

        match result {
            Err(EngineError::DecisionNotRecorded(source)) => {
                assert_eq!(source.to_string(), "log is down");
            }
            other => panic!("expected DecisionNotRecorded, got {other:?}"),
        }
        assert!(log.rows().is_empty());
    }

    #[test]
    fn replay_of_an_unmodified_decision_matches() {
        let strategy = Threshold::new("1.0");
        let recorded = decide(&strategy, context(Some("2"), 9));
        assert_eq!(replay(&strategy, &recorded), ReplayOutcome::Match);
    }

    #[test]
    fn replay_matches_nan_statistics() {
        let recorded = decide(&NanStats, context(None, 0));
        assert!(recorded.expected_edge_bps.is_some_and(f64::is_nan));
        assert_eq!(replay(&NanStats, &recorded), ReplayOutcome::Match);
    }

    #[test]
    fn replay_compares_quantity_by_value() {
        let strategy = Threshold::new("1.0");
        let mut recorded = enter_decision();
        recorded.intended_qty = Decimal::new(10000, 3);
        assert_eq!(replay(&strategy, &recorded), ReplayOutcome::Match);
    }

    #[test]
    fn replay_reports_tampered_features_as_hash_mismatch() {
        let strategy = Threshold::new("1.0");
        let mut recorded = enter_decision();
        recorded
            .features
            .insert("signal".to_owned(), Decimal::new(99, 0));
        assert_eq!(
            replay(&strategy, &recorded),
            ReplayOutcome::FeatureHashMismatch
        );
    }

    #[test]
    fn replay_reports_a_changed_action() {
        let strategy = Threshold::new("1.0");
        let mut recorded = enter_decision();
        recorded.action = Action::Exit;
        assert_eq!(
            replay(&strategy, &recorded),
            ReplayOutcome::Mismatch {
                field: "action",
                recorded: "exit".to_owned(),
                replayed: "enter".to_owned(),
            }
        );
    }

    #[test]
    fn replay_reports_a_changed_quantity() {
        let strategy = Threshold::new("1.0");
        let mut recorded = enter_decision();
        recorded.intended_qty = Decimal::new(11, 0);
        assert_eq!(
            replay(&strategy, &recorded),
            ReplayOutcome::Mismatch {
                field: "intended_qty",
                recorded: "11".to_owned(),
                replayed: "10".to_owned(),
            }
        );
    }

    #[test]
    fn replay_reports_a_changed_reason() {
        let strategy = Threshold::new("1.0");
        let mut recorded = enter_decision();
        recorded.reason_code = "other".to_owned();
        assert_eq!(
            replay(&strategy, &recorded),
            ReplayOutcome::Mismatch {
                field: "reason_code",
                recorded: "other".to_owned(),
                replayed: "above".to_owned(),
            }
        );
    }

    #[test]
    fn replay_reports_a_changed_edge() {
        let strategy = Threshold::new("1.0");
        let mut recorded = enter_decision();
        recorded.expected_edge_bps = Some(12.0);
        assert_eq!(
            replay(&strategy, &recorded),
            ReplayOutcome::Mismatch {
                field: "expected_edge_bps",
                recorded: "Some(12.0)".to_owned(),
                replayed: "Some(12.5)".to_owned(),
            }
        );
    }

    #[test]
    fn replay_reports_a_changed_vol() {
        let strategy = Threshold::new("1.0");
        let mut recorded = enter_decision();
        recorded.expected_vol = None;
        assert_eq!(
            replay(&strategy, &recorded),
            ReplayOutcome::Mismatch {
                field: "expected_vol",
                recorded: "None".to_owned(),
                replayed: "Some(0.15)".to_owned(),
            }
        );
    }

    #[test]
    fn replay_distinguishes_negative_zero_from_zero() {
        let mut recorded = decide(&ZeroEdge, context(None, 0));
        assert_eq!(replay(&ZeroEdge, &recorded), ReplayOutcome::Match);

        recorded.expected_edge_bps = Some(-0.0);
        assert_eq!(
            replay(&ZeroEdge, &recorded),
            ReplayOutcome::Mismatch {
                field: "expected_edge_bps",
                recorded: "Some(-0.0)".to_owned(),
                replayed: "Some(0.0)".to_owned(),
            }
        );
    }

    #[test]
    fn replay_reports_the_first_difference_in_documented_order() {
        let strategy = Threshold::new("1.0");

        let mut all = enter_decision();
        all.action = Action::Exit;
        all.intended_qty = Decimal::new(11, 0);
        all.reason_code = "other".to_owned();
        all.expected_edge_bps = Some(1.0);
        all.expected_vol = Some(1.0);
        assert!(matches!(
            replay(&strategy, &all),
            ReplayOutcome::Mismatch {
                field: "action",
                ..
            }
        ));

        let mut no_action = enter_decision();
        no_action.intended_qty = Decimal::new(11, 0);
        no_action.reason_code = "other".to_owned();
        no_action.expected_edge_bps = Some(1.0);
        no_action.expected_vol = Some(1.0);
        assert!(matches!(
            replay(&strategy, &no_action),
            ReplayOutcome::Mismatch {
                field: "intended_qty",
                ..
            }
        ));

        let mut reason_and_action = enter_decision();
        reason_and_action.action = Action::Hold;
        reason_and_action.reason_code = "other".to_owned();
        assert_eq!(
            replay(&strategy, &reason_and_action),
            ReplayOutcome::Mismatch {
                field: "action",
                recorded: "hold".to_owned(),
                replayed: "enter".to_owned(),
            }
        );
    }

    #[test]
    fn replay_depends_on_the_stored_seed() {
        let mut recorded = decide(&Seeded, context(None, 2));
        assert_eq!(recorded.action, Action::Enter);
        recorded.rng_seed = 3;
        assert_eq!(
            replay(&Seeded, &recorded),
            ReplayOutcome::Mismatch {
                field: "action",
                recorded: "enter".to_owned(),
                replayed: "exit".to_owned(),
            }
        );
    }

    #[test]
    fn replay_is_stable_over_a_thousand_runs() {
        let strategy = Threshold::new("1.0");
        let recorded = decide(&strategy, context(Some("2"), 9));
        for _ in 0..1_000 {
            assert_eq!(replay(&strategy, &recorded), ReplayOutcome::Match);
        }
        // One call for the decision, one per replay.
        assert_eq!(strategy.calls(), 1_001);
    }
}
