//! Purged k-fold cross-validation with an embargo (López de Prado).

use std::ops::Range;

use crate::EvalError;

/// One fold: the range tested and the ranges that may be trained on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    pub test: Range<usize>,
    /// Every index outside `[test.start - purge, test.end + embargo)`, as at most two
    /// ranges in ascending order; empty ranges are left out.
    pub train: Vec<Range<usize>>,
}

/// The `k` folds of `n` observations: fold `i` tests `[i·n/k, (i+1)·n/k)` (integer
/// division, so the folds cover `0..n` exactly). Errors with `InvalidJob` when `k < 2` or
/// `k > n`.
#[allow(unused_variables, reason = "stub: Phase 5 task ST4")]
pub fn folds(n: usize, k: usize, purge: usize, embargo: usize) -> Result<Vec<Fold>, EvalError> {
    todo!("Phase 5 task ST4")
}
