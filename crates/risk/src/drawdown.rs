//! The drawdown step-down: normal, halved, flat. Rules in `docs/phases/phase-4.md`,
//! "Drawdown".

use nisse_core::RiskEvent;
use rust_decimal::Decimal;

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

#[allow(unused_variables, reason = "stub: Phase 4 task R2")]
impl DrawdownState {
    /// Feeds one equity figure. Steps are in the phase doc; in short: a non-positive equity
    /// is `Flat`; a new high moves the peak; `Flat` is latched; otherwise the thresholds in
    /// `limits` pick the mode, with `Halved` returning to `Normal` only below
    /// `drawdown_resume`.
    pub fn observe(&mut self, equity: Decimal, limits: &Limits) -> Observation {
        todo!("Phase 4 task R2")
    }

    /// `1 - equity / peak`, never negative; zero when there is no peak yet, and one when the
    /// equity is not positive.
    #[must_use]
    pub fn drawdown(&self, equity: Decimal) -> Decimal {
        todo!("Phase 4 task R2")
    }

    /// A person's decision to start again after `Flat`: the peak becomes `equity` and the
    /// mode `Normal`. Returns what changed, for the caller to store. `equity` must be
    /// positive (`InvalidEvent` otherwise, and nothing changes).
    pub fn reset(&mut self, equity: Decimal) -> Result<Observation, RiskError> {
        todo!("Phase 4 task R2")
    }

    /// The state after `events` (in id order): `equity_peak` sets the peak, `drawdown_mode`
    /// sets the mode (and, with `"reset": true`, the peak to its `"equity"`); other kinds are
    /// skipped. A peak or mode event whose detail cannot be read is `InvalidEvent`: the state
    /// is unknown and the caller must not trade.
    pub fn restore(events: &[RiskEvent]) -> Result<Self, RiskError> {
        todo!("Phase 4 task R2")
    }
}
