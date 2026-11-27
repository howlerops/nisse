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
pub fn folds(n: usize, k: usize, purge: usize, embargo: usize) -> Result<Vec<Fold>, EvalError> {
    if k < 2 {
        return Err(EvalError::InvalidJob("cv: k must be at least 2".into()));
    }
    if k > n {
        return Err(EvalError::InvalidJob(
            "cv: more folds than observations".into(),
        ));
    }
    let mut out = Vec::with_capacity(k);
    for i in 0..k {
        let start = boundary(i, n, k)?;
        let end = boundary(i + 1, n, k)?;
        let lo = start.saturating_sub(purge);
        let hi = n.min(end.saturating_add(embargo));
        let mut train = Vec::with_capacity(2);
        if lo > 0 {
            train.push(0..lo);
        }
        if hi < n {
            train.push(hi..n);
        }
        out.push(Fold {
            test: start..end,
            train,
        });
    }
    Ok(out)
}

/// `i·n/k` computed in `u128` so the product cannot overflow, then narrowed back to `usize`.
fn boundary(i: usize, n: usize, k: usize) -> Result<usize, EvalError> {
    let product = (i as u128 * n as u128) / k as u128;
    usize::try_from(product)
        .map_err(|_| EvalError::InvalidJob("cv: fold boundary overflows usize".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid_job(result: &Result<Vec<Fold>, EvalError>) -> bool {
        matches!(result, Err(EvalError::InvalidJob(_)))
    }

    /// Checks the structural contract of `folds` for one parameter set.
    fn assert_valid(n: usize, k: usize, purge: usize, embargo: usize) {
        let folds = folds(n, k, purge, embargo).expect("valid parameters");
        assert_eq!(folds.len(), k);
        let mut next = 0;
        for fold in &folds {
            // Tests partition 0..n in order.
            assert_eq!(fold.test.start, next, "tests must be contiguous");
            assert!(fold.test.end > fold.test.start, "test must be non-empty");
            next = fold.test.end;

            let excluded_lo = fold.test.start.saturating_sub(purge);
            let excluded_hi = fold.test.end.saturating_add(embargo);
            let mut prev_end = 0;
            for range in &fold.train {
                assert!(range.start < range.end, "train range must be non-empty");
                assert!(range.start >= prev_end, "train ranges must ascend");
                assert!(range.end <= n);
                assert!(
                    range.end <= excluded_lo || range.start >= excluded_hi,
                    "train range {range:?} overlaps the excluded zone"
                );
                prev_end = range.end;
            }
            // Every index outside the excluded zone is trainable.
            for idx in 0..n {
                let excluded = idx >= excluded_lo && idx < excluded_hi;
                let trainable = fold.train.iter().any(|r| r.contains(&idx));
                assert_eq!(trainable, !excluded, "index {idx} in fold {:?}", fold.test);
            }
        }
        assert_eq!(next, n, "tests must cover 0..n");
    }

    #[test]
    fn five_folds_without_purge_or_embargo() {
        let f = folds(10, 5, 0, 0).expect("valid");
        let tests: Vec<_> = f.iter().map(|x| x.test.clone()).collect();
        assert_eq!(tests, vec![0..2, 2..4, 4..6, 6..8, 8..10]);
        assert_eq!(f[0].train, vec![2..10]);
        assert_eq!(f[2].train, vec![0..4, 6..10]);
        assert_eq!(f[4].train, vec![0..8]);
    }

    #[test]
    fn three_folds_use_integer_division() {
        let f = folds(10, 3, 0, 0).expect("valid");
        let tests: Vec<_> = f.iter().map(|x| x.test.clone()).collect();
        assert_eq!(tests, vec![0..3, 3..6, 6..10]);
    }

    #[test]
    fn purge_and_embargo_shrink_the_train_set() {
        let f = folds(100, 4, 5, 10).expect("valid");
        assert_eq!(f[1].test, 25..50);
        assert_eq!(f[1].train, vec![0..20, 60..100]);
        assert_eq!(f[0].test, 0..25);
        assert_eq!(f[0].train, vec![35..100]);
        assert_eq!(f[3].test, 75..100);
        assert_eq!(f[3].train, vec![0..70]);
    }

    #[test]
    fn purge_and_embargo_larger_than_data_leave_no_train() {
        let f = folds(10, 2, 100, 100).expect("valid");
        assert!(f.iter().all(|fold| fold.train.is_empty()));
    }

    #[test]
    fn property_holds_over_parameter_grid() {
        for n in 1..=24 {
            for k in 2..=n {
                for purge in 0..4 {
                    for embargo in 0..4 {
                        assert_valid(n, k, purge, embargo);
                    }
                }
            }
        }
    }

    #[test]
    fn invalid_fold_counts_are_rejected() {
        assert!(invalid_job(&folds(10, 0, 0, 0)));
        assert!(invalid_job(&folds(10, 1, 0, 0)));
        assert!(invalid_job(&folds(5, 6, 0, 0)));
        assert!(invalid_job(&folds(0, 2, 0, 0)));
    }

    #[test]
    fn one_observation_per_fold_when_k_equals_n() {
        let f = folds(5, 5, 0, 0).expect("valid");
        assert_eq!(f.len(), 5);
        for (i, fold) in f.iter().enumerate() {
            assert_eq!(fold.test, i..i + 1);
        }
        assert_eq!(f[0].train, vec![1..5]);
        assert_eq!(f[2].train, vec![0..2, 3..5]);
    }

    #[test]
    fn huge_n_does_not_overflow() {
        let result = folds(usize::MAX / 2, 3, 0, 0);
        assert!(result.is_ok() || invalid_job(&result));
    }
}
