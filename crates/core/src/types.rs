//! Enums stored as text in the decision store.
//!
//! The snake_case string of each variant is the value stored in Postgres and is fixed by a
//! `CHECK` constraint in `migrations/`. Renaming or adding a variant needs a new migration.
//! `<&'static str>::from(variant)` gives the stored string without allocating, `FromStr` parses
//! it back, and `VARIANTS` (from [`strum::VariantArray`]) lists every variant.

use strum::{Display, EnumString, IntoStaticStr, VariantArray};

/// What the strategy decided for one symbol at one decision point (`decision.action`).
/// Skips and holds are stored and scored too, not just entries and exits.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum Action {
    Enter,
    Exit,
    Hold,
    Skip,
}

/// How a trial was run (`trial_registry.kind`). Every kind counts toward the lifetime trial
/// budget.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum TrialKind {
    Backtest,
    Shadow,
    Live,
}

/// What a `risk_event` row records (`risk_event.kind`). The risk state after a restart is the
/// fold of these rows in id order.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum RiskEventKind {
    /// A new equity high-water mark. Detail: `{"peak": "<decimal>"}`.
    EquityPeak,
    /// The drawdown mode changed, or was reset by a person. Detail:
    /// `{"mode": "normal"|"halved"|"flat", "equity": "<decimal>", "reset": bool}`.
    DrawdownMode,
    /// The kill switch was engaged or released. Detail: `{"engaged": bool, "by": "..."}`.
    KillSwitch,
    /// A process loaded a limits file. Detail: `{"hash": "<hex>", "limits": {...}}`.
    LimitsLoaded,
}

/// What an `eval_score` row scores (`eval_score.subject_type`); `subject_id` is the id of a
/// row in the table of the same name.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(serialize_all = "snake_case")]
pub enum SubjectType {
    Decision,
    StrategyVersion,
}
