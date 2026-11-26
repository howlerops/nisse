//! Typed row identifiers for the store tables other rows point at.

/// Primary key of a `strategy_version` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StrategyVersionId(pub i64);

/// Primary key of a `decision` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DecisionId(pub i64);
