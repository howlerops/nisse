//! The drawdown step-down: normal, halved, flat. Rules in `docs/phases/phase-4.md`,
//! "Drawdown".

use std::str::FromStr;

use nisse_core::{RiskEvent, RiskEventKind};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::{Limits, RiskError};

/// How much of its normal exposure the strategy may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrawdownMode {
    #[default]
    Normal,
    Halved,
    /// No exposure. Latched: only [`DrawdownState::reset`] leaves it.
    Flat,
}

impl DrawdownMode {
    /// The name stored in `risk_event.detail["mode"]`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Halved => "halved",
            Self::Flat => "flat",
        }
    }

    /// The mode stored under `name`, if it is one.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "halved" => Some(Self::Halved),
            "flat" => Some(Self::Flat),
            _ => None,
        }
    }

    /// The factor on the strategy's target weights and on the exposure cap: 1, 0.5 or 0.
    #[must_use]
    pub fn scale(self) -> Decimal {
        match self {
            Self::Normal => Decimal::ONE,
            Self::Halved => Decimal::new(5, 1),
            Self::Flat => Decimal::ZERO,
        }
    }
}

/// The equity high-water mark and the mode it has led to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DrawdownState {
    /// `None` until the first equity is observed.
    pub peak: Option<Decimal>,
    pub mode: DrawdownMode,
}

/// What one equity observation changed. The caller stores a risk event for each `Some`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Observation {
    /// The new high-water mark, when this equity set one.
    pub new_peak: Option<Decimal>,
    /// `(from, to)` when the mode changed.
    pub mode_change: Option<(DrawdownMode, DrawdownMode)>,
}

impl DrawdownState {
    /// Feeds one equity figure. Steps are in the phase doc; in short: a non-positive equity
    /// is `Flat`; a new high moves the peak; `Flat` is latched; otherwise the thresholds in
    /// `limits` pick the mode, with `Halved` returning to `Normal` only below
    /// `drawdown_resume`.
    pub fn observe(&mut self, equity: Decimal, limits: &Limits) -> Observation {
        let before = self.mode;
        let mut new_peak = None;
        if equity <= Decimal::ZERO {
            self.mode = DrawdownMode::Flat;
        } else {
            if self.peak.is_none_or(|peak| equity > peak) {
                self.peak = Some(equity);
                new_peak = Some(equity);
            }
            let drawdown = self.drawdown(equity);
            self.mode = next_mode(self.mode, drawdown, limits);
        }
        Observation {
            new_peak,
            mode_change: change(before, self.mode),
        }
    }

    /// `1 - equity / peak`, never negative; zero when there is no peak yet, and one when the
    /// equity is not positive.
    #[must_use]
    pub fn drawdown(&self, equity: Decimal) -> Decimal {
        let Some(peak) = self.peak else {
            return Decimal::ZERO;
        };
        if equity <= Decimal::ZERO {
            return Decimal::ONE;
        }
        // A failed division or subtraction fails closed: the drawdown is the whole account.
        equity
            .checked_div(peak)
            .and_then(|ratio| Decimal::ONE.checked_sub(ratio))
            .map_or(Decimal::ONE, |drawdown| drawdown.max(Decimal::ZERO))
    }

    /// A person's decision to start again after `Flat`: the peak becomes `equity` and the
    /// mode `Normal`. Returns what changed, for the caller to store. `equity` must be
    /// positive (`InvalidEvent` otherwise, and nothing changes).
    pub fn reset(&mut self, equity: Decimal) -> Result<Observation, RiskError> {
        if equity <= Decimal::ZERO {
            return Err(RiskError::InvalidEvent(
                "reset needs a positive equity".into(),
            ));
        }
        let before = self.mode;
        self.peak = Some(equity);
        self.mode = DrawdownMode::Normal;
        Ok(Observation {
            new_peak: Some(equity),
            mode_change: change(before, self.mode),
        })
    }

    /// The state after `events` (in id order): `equity_peak` sets the peak, `drawdown_mode`
    /// sets the mode (and, with `"reset": true`, the peak to its `"equity"`); other kinds are
    /// skipped. A peak or mode event whose detail cannot be read is `InvalidEvent`: the state
    /// is unknown and the caller must not trade.
    pub fn restore(events: &[RiskEvent]) -> Result<Self, RiskError> {
        let mut state = Self::default();
        for event in events {
            let kind = event.kind;
            match kind {
                RiskEventKind::EquityPeak => {
                    state.peak = Some(positive_decimal(&event.detail, "peak", kind)?);
                }
                RiskEventKind::DrawdownMode => {
                    let name = event
                        .detail
                        .get("mode")
                        .and_then(Value::as_str)
                        .ok_or_else(|| event_error(kind, "mode must be a string"))?;
                    state.mode = DrawdownMode::from_name(name)
                        .ok_or_else(|| event_error(kind, "mode must be normal, halved or flat"))?;
                    if reset_flag(&event.detail, kind)? {
                        state.peak = Some(positive_decimal(&event.detail, "equity", kind)?);
                    }
                }
                _ => {}
            }
        }
        Ok(state)
    }
}

/// The mode after one observation with `drawdown` (the peak has already been updated).
fn next_mode(mode: DrawdownMode, drawdown: Decimal, limits: &Limits) -> DrawdownMode {
    if mode == DrawdownMode::Flat || drawdown >= limits.drawdown_flat {
        DrawdownMode::Flat
    } else if drawdown >= limits.drawdown_halve {
        DrawdownMode::Halved
    } else if mode == DrawdownMode::Halved && drawdown < limits.drawdown_resume {
        DrawdownMode::Normal
    } else {
        mode
    }
}

/// `Some((from, to))` when the mode differs.
fn change(from: DrawdownMode, to: DrawdownMode) -> Option<(DrawdownMode, DrawdownMode)> {
    (from != to).then_some((from, to))
}

fn event_error(kind: RiskEventKind, problem: &str) -> RiskError {
    RiskError::InvalidEvent(format!("{kind}: {problem}"))
}

/// A decimal stored as a JSON string under `key`, and greater than zero.
fn positive_decimal(detail: &Value, key: &str, kind: RiskEventKind) -> Result<Decimal, RiskError> {
    let text = detail
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| event_error(kind, &format!("{key} must be a decimal string")))?;
    let value = Decimal::from_str(text)
        .map_err(|_| event_error(kind, &format!("{key} is not a decimal")))?;
    if value > Decimal::ZERO {
        Ok(value)
    } else {
        Err(event_error(kind, &format!("{key} must be positive")))
    }
}

/// The `"reset"` flag: absent is false; present must be a boolean.
fn reset_flag(detail: &Value, kind: RiskEventKind) -> Result<bool, RiskError> {
    match detail.get("reset") {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(event_error(kind, "reset must be a boolean")),
    }
}

#[cfg(test)]
mod tests {
    use nisse_core::nautilus::UnixNanos;
    use serde_json::json;

    use super::*;

    const DEFAULT: &str = include_str!("../../../config/limits/limits.json");

    fn limits() -> Limits {
        let value: Value = serde_json::from_str(DEFAULT).unwrap();
        Limits::from_json(&value).unwrap()
    }

    fn d(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    fn event(kind: RiskEventKind, detail: Value) -> RiskEvent {
        RiskEvent {
            kind,
            detail,
            ts: UnixNanos::from(1_u64),
        }
    }

    fn observed(equity: &str, state: &mut DrawdownState, limits: &Limits) -> Observation {
        state.observe(d(equity), limits)
    }

    fn is_invalid_event(result: Result<DrawdownState, RiskError>) -> bool {
        matches!(result, Err(RiskError::InvalidEvent(_)))
    }

    #[test]
    fn first_observation_sets_the_peak_and_stays_normal() {
        let limits = limits();
        let mut state = DrawdownState::default();
        assert_eq!(
            observed("100", &mut state, &limits),
            Observation {
                new_peak: Some(d("100")),
                mode_change: None,
            }
        );
        assert_eq!(state.peak, Some(d("100")));
        assert_eq!(state.mode, DrawdownMode::Normal);

        let higher = observed("105", &mut state, &limits);
        assert_eq!(higher.new_peak, Some(d("105")));
        assert_eq!(higher.mode_change, None);

        assert_eq!(observed("105", &mut state, &limits), Observation::default());
        assert_eq!(observed("104", &mut state, &limits), Observation::default());
        assert_eq!(state.peak, Some(d("105")));
    }

    #[test]
    fn halve_and_flat_thresholds_are_inclusive() {
        let limits = limits();
        let mut state = DrawdownState::default();
        observed("100", &mut state, &limits);

        assert_eq!(observed("90.01", &mut state, &limits).mode_change, None);
        assert_eq!(state.mode, DrawdownMode::Normal);

        let halved = observed("90", &mut state, &limits);
        assert_eq!(
            halved.mode_change,
            Some((DrawdownMode::Normal, DrawdownMode::Halved))
        );
        assert_eq!(state.mode, DrawdownMode::Halved);

        assert_eq!(observed("85.01", &mut state, &limits).mode_change, None);
        assert_eq!(state.mode, DrawdownMode::Halved);

        let flat = observed("85", &mut state, &limits);
        assert_eq!(
            flat.mode_change,
            Some((DrawdownMode::Halved, DrawdownMode::Flat))
        );
        assert_eq!(state.mode, DrawdownMode::Flat);
    }

    #[test]
    fn a_large_fall_goes_straight_from_normal_to_flat() {
        let limits = limits();
        let mut state = DrawdownState::default();
        observed("100", &mut state, &limits);
        let obs = observed("80", &mut state, &limits);
        assert_eq!(
            obs.mode_change,
            Some((DrawdownMode::Normal, DrawdownMode::Flat))
        );
        assert_eq!(state.mode, DrawdownMode::Flat);
    }

    #[test]
    fn halved_returns_to_normal_only_below_the_resume_threshold() {
        let limits = limits();
        let mut state = DrawdownState::default();
        observed("100", &mut state, &limits);
        observed("90", &mut state, &limits);
        assert_eq!(state.mode, DrawdownMode::Halved);

        // Drawdown 0.06 is at or above resume: stays halved, no change reported.
        assert_eq!(observed("94", &mut state, &limits).mode_change, None);
        assert_eq!(state.mode, DrawdownMode::Halved);

        // Drawdown exactly 0.05 is not below resume.
        assert_eq!(observed("95", &mut state, &limits).mode_change, None);
        assert_eq!(state.mode, DrawdownMode::Halved);

        let back = observed("95.01", &mut state, &limits);
        assert_eq!(
            back.mode_change,
            Some((DrawdownMode::Halved, DrawdownMode::Normal))
        );
        assert_eq!(state.mode, DrawdownMode::Normal);
    }

    #[test]
    fn flat_is_latched_even_when_a_new_peak_is_recorded() {
        let limits = limits();
        let mut state = DrawdownState::default();
        observed("100", &mut state, &limits);
        observed("80", &mut state, &limits);
        assert_eq!(state.mode, DrawdownMode::Flat);

        assert_eq!(observed("100", &mut state, &limits), Observation::default());
        assert_eq!(state.mode, DrawdownMode::Flat);

        let new_high = observed("200", &mut state, &limits);
        assert_eq!(new_high.new_peak, Some(d("200")));
        assert_eq!(new_high.mode_change, None);
        assert_eq!(state.mode, DrawdownMode::Flat);
        assert_eq!(state.peak, Some(d("200")));
    }

    #[test]
    fn a_non_positive_equity_is_flat_and_keeps_the_peak() {
        let limits = limits();
        let mut state = DrawdownState::default();
        observed("100", &mut state, &limits);

        let zero = observed("0", &mut state, &limits);
        assert_eq!(
            zero,
            Observation {
                new_peak: None,
                mode_change: Some((DrawdownMode::Normal, DrawdownMode::Flat)),
            }
        );
        assert_eq!(state.peak, Some(d("100")));

        assert_eq!(observed("-5", &mut state, &limits), Observation::default());
        assert_eq!(state.mode, DrawdownMode::Flat);
        assert_eq!(state.peak, Some(d("100")));
    }

    #[test]
    fn a_non_positive_first_equity_is_flat_without_a_peak() {
        let limits = limits();
        let mut state = DrawdownState::default();
        let obs = observed("0", &mut state, &limits);
        assert_eq!(obs.new_peak, None);
        assert_eq!(
            obs.mode_change,
            Some((DrawdownMode::Normal, DrawdownMode::Flat))
        );
        assert_eq!(state.peak, None);
        assert_eq!(state.mode, DrawdownMode::Flat);
    }

    #[test]
    fn drawdown_is_measured_from_the_peak() {
        let mut state = DrawdownState::default();
        assert_eq!(state.drawdown(d("50")), Decimal::ZERO);

        let limits = limits();
        observed("100", &mut state, &limits);
        assert_eq!(state.drawdown(d("120")), Decimal::ZERO);
        assert_eq!(state.drawdown(d("100")), Decimal::ZERO);
        assert_eq!(state.drawdown(d("90")), d("0.1"));
        assert_eq!(state.drawdown(Decimal::ZERO), Decimal::ONE);
        assert_eq!(state.drawdown(d("-3")), Decimal::ONE);
    }

    #[test]
    fn drawdown_fails_closed_on_a_zero_peak() {
        let state = DrawdownState {
            peak: Some(Decimal::ZERO),
            mode: DrawdownMode::Normal,
        };
        assert_eq!(state.drawdown(d("5")), Decimal::ONE);
    }

    #[test]
    fn reset_restarts_from_flat_with_a_new_peak() {
        let limits = limits();
        let mut state = DrawdownState::default();
        observed("100", &mut state, &limits);
        observed("80", &mut state, &limits);
        assert_eq!(state.mode, DrawdownMode::Flat);

        let obs = state.reset(d("70")).unwrap();
        assert_eq!(
            obs,
            Observation {
                new_peak: Some(d("70")),
                mode_change: Some((DrawdownMode::Flat, DrawdownMode::Normal)),
            }
        );
        assert_eq!(state.peak, Some(d("70")));
        assert_eq!(state.mode, DrawdownMode::Normal);

        // 63 is 10% below 70: halved.
        assert_eq!(
            observed("63", &mut state, &limits).mode_change,
            Some((DrawdownMode::Normal, DrawdownMode::Halved))
        );
    }

    #[test]
    fn reset_from_normal_reports_no_mode_change() {
        let mut state = DrawdownState::default();
        let obs = state.reset(d("10")).unwrap();
        assert_eq!(obs.new_peak, Some(d("10")));
        assert_eq!(obs.mode_change, None);
    }

    #[test]
    fn reset_with_a_non_positive_equity_changes_nothing() {
        let limits = limits();
        let mut state = DrawdownState::default();
        observed("100", &mut state, &limits);
        observed("80", &mut state, &limits);
        let before = state;

        assert!(matches!(
            state.reset(Decimal::ZERO),
            Err(RiskError::InvalidEvent(_))
        ));
        assert!(matches!(
            state.reset(d("-1")),
            Err(RiskError::InvalidEvent(_))
        ));
        assert_eq!(state, before);
    }

    #[test]
    fn restore_of_no_events_is_the_default_state() {
        assert_eq!(
            DrawdownState::restore(&[]).unwrap(),
            DrawdownState::default()
        );
    }

    #[test]
    fn restore_folds_peaks_modes_and_resets_in_order() {
        let events = [
            event(RiskEventKind::EquityPeak, json!({"peak": "100"})),
            event(RiskEventKind::DrawdownMode, json!({"mode": "halved"})),
            event(RiskEventKind::EquityPeak, json!({"peak": "120"})),
            event(RiskEventKind::DrawdownMode, json!({"mode": "flat"})),
            event(
                RiskEventKind::DrawdownMode,
                json!({"mode": "normal", "equity": "80", "reset": true}),
            ),
        ];
        assert_eq!(
            DrawdownState::restore(&events).unwrap(),
            DrawdownState {
                peak: Some(d("80")),
                mode: DrawdownMode::Normal,
            }
        );
    }

    #[test]
    fn restore_skips_other_kinds() {
        let events = [
            event(RiskEventKind::EquityPeak, json!({"peak": "100"})),
            event(
                RiskEventKind::KillSwitch,
                json!({"engaged": true, "by": "owner"}),
            ),
            event(RiskEventKind::DrawdownMode, json!({"mode": "halved"})),
            event(
                RiskEventKind::LimitsLoaded,
                json!({"hash": "ab", "limits": {}}),
            ),
        ];
        assert_eq!(
            DrawdownState::restore(&events).unwrap(),
            DrawdownState {
                peak: Some(d("100")),
                mode: DrawdownMode::Halved,
            }
        );
    }

    #[test]
    fn restore_rejects_malformed_events() {
        let cases = [
            event(RiskEventKind::EquityPeak, json!({"peak": 100})),
            event(RiskEventKind::EquityPeak, json!({"peak": "abc"})),
            event(RiskEventKind::EquityPeak, json!({"peak": "0"})),
            event(RiskEventKind::EquityPeak, json!({"peak": "-1"})),
            event(RiskEventKind::DrawdownMode, json!({})),
            event(RiskEventKind::DrawdownMode, json!({"mode": "sideways"})),
            event(
                RiskEventKind::DrawdownMode,
                json!({"mode": "normal", "reset": true}),
            ),
            event(
                RiskEventKind::DrawdownMode,
                json!({"mode": "normal", "reset": true, "equity": "0"}),
            ),
            event(
                RiskEventKind::DrawdownMode,
                json!({"mode": "normal", "reset": "yes"}),
            ),
        ];
        for case in cases {
            let kind = case.kind.to_string();
            match DrawdownState::restore(&[case]) {
                Err(RiskError::InvalidEvent(message)) => {
                    assert!(message.starts_with(&kind), "{message} does not name {kind}");
                }
                other => panic!("expected InvalidEvent for {kind}, got {other:?}"),
            }
        }
    }

    #[test]
    fn restore_rejects_a_malformed_event_among_good_ones() {
        let events = [
            event(RiskEventKind::EquityPeak, json!({"peak": "100"})),
            event(RiskEventKind::DrawdownMode, json!({"mode": "sideways"})),
        ];
        assert!(is_invalid_event(DrawdownState::restore(&events)));
    }

    #[test]
    fn stored_events_restore_the_live_state() {
        let limits = limits();
        let mut live = DrawdownState::default();
        let mut events = Vec::new();
        let mut record = |obs: Observation| {
            if let Some(peak) = obs.new_peak {
                events.push(event(
                    RiskEventKind::EquityPeak,
                    json!({"peak": peak.to_string()}),
                ));
            }
            if let Some((_, to)) = obs.mode_change {
                events.push(event(
                    RiskEventKind::DrawdownMode,
                    json!({"mode": to.as_str()}),
                ));
            }
        };

        for equity in [
            "100", "105", "100", "92", "99", "110", "104", "103", "105", "80",
        ] {
            record(live.observe(d(equity), &limits));
        }
        record(live.reset(d("90")).unwrap());
        for equity in ["85", "75", "0"] {
            record(live.observe(d(equity), &limits));
        }

        assert!(events.len() > 5);
        assert_eq!(DrawdownState::restore(&events).unwrap(), live);
    }

    #[test]
    fn mode_scale_and_names() {
        assert_eq!(DrawdownMode::Normal.scale(), Decimal::ONE);
        assert_eq!(DrawdownMode::Halved.scale(), d("0.5"));
        assert_eq!(DrawdownMode::Flat.scale(), Decimal::ZERO);

        for mode in [
            DrawdownMode::Normal,
            DrawdownMode::Halved,
            DrawdownMode::Flat,
        ] {
            assert_eq!(DrawdownMode::from_name(mode.as_str()), Some(mode));
        }
        assert_eq!(DrawdownMode::from_name("x"), None);
    }
}
