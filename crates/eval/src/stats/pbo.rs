//! Probability of backtest overfitting by combinatorially symmetric cross-validation.

/// Blocks the sessions are cut into; half are in-sample in each combination.
pub const SPLITS: usize = 16;

/// In-sample blocks in each combination (`SPLITS / 2`).
const IN_SAMPLE: u32 = 8;

/// `C(16, 8)`: the number of combinations, the denominator of the result.
const COMBINATIONS: usize = 12_870;

/// Sums over the rows of one configuration's blocks: `count`, `Σd` and `Σd²` with
/// `d = r - shift`. The shift is the configuration's first kept row. It changes no statistic
/// (the mean is `Σd / n + shift`, the variance is shift-invariant), but it keeps a constant
/// series at exactly zero variance instead of rounding noise.
#[derive(Clone, Copy, Default)]
struct Sums {
    count: f64,
    sum: f64,
    sum_sq: f64,
}

impl Sums {
    fn absorb(&mut self, other: &Sums) {
        self.count += other.count;
        self.sum += other.sum;
        self.sum_sq += other.sum_sq;
    }
}

/// Every 16-bit mask with exactly `IN_SAMPLE` bits set. Bit `b` set means block `b` is
/// in-sample.
fn combinations() -> impl Iterator<Item = u32> {
    (0..(1u32 << SPLITS)).filter(|mask| mask.count_ones() == IN_SAMPLE)
}

/// `Sum`s of every configuration's blocks, row-major (`N × SPLITS`), and the shift used for each
/// configuration. `skip` rows are dropped from the start, then each `len` rows form a block.
/// `skip + SPLITS * len` must not exceed the series length (the caller guarantees this).
fn block_sums(series: &[&[f64]], skip: usize, len: usize) -> (Vec<Sums>, Vec<f64>) {
    let mut table = Vec::with_capacity(series.len() * SPLITS);
    let mut shifts = Vec::with_capacity(series.len());
    for s in series {
        let rows = s.get(skip..).unwrap_or(&[]);
        let shift = rows.first().copied().unwrap_or(0.0);
        shifts.push(shift);
        for block in rows.chunks_exact(len.max(1)) {
            let mut sums = Sums::default();
            for &r in block {
                let d = r - shift;
                sums.count += 1.0;
                sums.sum += d;
                sums.sum_sq += d * d;
            }
            table.push(sums);
        }
    }
    (table, shifts)
}

/// The per-period Sharpe of the rows summed in `sums`: `mean / sqrt(var)` with
/// `var = (Σr² − n·mean²) / (n − 1)`. `NEG_INFINITY` when it is `None` (fewer than two rows,
/// variance zero or not finite, or a non-finite ratio).
fn sharpe_of(sums: &Sums, shift: f64) -> f64 {
    if sums.count < 2.0 {
        return f64::NEG_INFINITY;
    }
    let centred_mean = sums.sum / sums.count;
    let var = (sums.sum_sq - sums.sum * centred_mean) / (sums.count - 1.0);
    if var.is_finite() && var > 0.0 {
        let sharpe = (centred_mean + shift) / var.sqrt();
        if sharpe.is_finite() {
            return sharpe;
        }
    }
    f64::NEG_INFINITY
}

/// The in-sample and out-of-sample Sharpes of one configuration for the combination `mask`.
/// Bit `b` of `mask` selects block `b` as in-sample; the other blocks are out-of-sample.
fn split_sharpes(blocks: &[Sums], shift: f64, mask: u32) -> (f64, f64) {
    let mut in_sample = Sums::default();
    let mut out_of_sample = Sums::default();
    for (b, block) in blocks.iter().enumerate() {
        if (mask >> b) & 1 == 1 {
            in_sample.absorb(block);
        } else {
            out_of_sample.absorb(block);
        }
    }
    (
        sharpe_of(&in_sample, shift),
        sharpe_of(&out_of_sample, shift),
    )
}

/// The index of the largest in-sample Sharpe; ties go to the lowest index. Index 0 when every
/// value is `NEG_INFINITY`.
fn best_index(in_sample: &[f64]) -> usize {
    let mut best = 0;
    let mut best_value = f64::NEG_INFINITY;
    for (i, &v) in in_sample.iter().enumerate() {
        if v > best_value {
            best = i;
            best_value = v;
        }
    }
    best
}

/// `ω = rank / (N + 1)` of the configuration `best`, where `rank` is `1 + (configurations with a
/// strictly smaller out-of-sample Sharpe) + ½ × (other configurations with an equal one)`.
fn omega_of(out_of_sample: &[f64], best: usize) -> f64 {
    let target = out_of_sample
        .get(best)
        .copied()
        .unwrap_or(f64::NEG_INFINITY);
    let mut below = 0_usize;
    let mut tied = 0_usize;
    for (i, &v) in out_of_sample.iter().enumerate() {
        if v < target {
            below += 1;
        } else if v == target && i != best {
            tied += 1;
        }
    }
    let rank = 1.0 + below as f64 + 0.5 * tied as f64;
    rank / (out_of_sample.len() as f64 + 1.0)
}

/// The probability of backtest overfitting of `series` (one return series per
/// configuration, all over the same sessions). Rules in `docs/phases/phase-5.md`. `None`
/// when there are fewer than two series, their lengths differ, or there are fewer than
/// `2 × SPLITS` sessions.
#[must_use]
pub fn pbo(series: &[&[f64]]) -> Option<f64> {
    let configs = series.len();
    if configs < 2 {
        return None;
    }
    let total = series.first()?.len();
    if total < 2 * SPLITS || series.iter().any(|s| s.len() != total) {
        return None;
    }
    if series.iter().any(|s| s.iter().any(|r| !r.is_finite())) {
        return None;
    }
    let skip = total % SPLITS;
    let len = (total - skip) / SPLITS;
    let (table, shifts) = block_sums(series, skip, len);

    let mut is_sharpes = vec![0.0_f64; configs];
    let mut oos_sharpes = vec![0.0_f64; configs];
    let mut counted = 0_usize;
    for mask in combinations() {
        for ((blocks, &shift), (is_slot, oos_slot)) in table
            .as_chunks::<SPLITS>()
            .0
            .iter()
            .zip(&shifts)
            .zip(is_sharpes.iter_mut().zip(oos_sharpes.iter_mut()))
        {
            (*is_slot, *oos_slot) = split_sharpes(blocks, shift, mask);
        }
        let best = best_index(&is_sharpes);
        let omega = omega_of(&oos_sharpes, best);
        if (omega / (1.0 - omega)).ln() <= 0.0 {
            counted += 1;
        }
    }
    Some(counted as f64 / COMBINATIONS as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Alternating ±0.001 by global row index. Every block of even length sums to zero.
    fn noise(i: usize) -> f64 {
        if i.is_multiple_of(2) { 0.001 } else { -0.001 }
    }

    /// `len` values `scale × sin(0.37 i + phase)`.
    fn sine(len: usize, phase: f64, scale: f64) -> Vec<f64> {
        (0..len)
            .map(|i| scale * (i as f64 * 0.37 + phase).sin())
            .collect()
    }

    /// Borrows each series as a slice, in order.
    fn refs(series: &[Vec<f64>]) -> Vec<&[f64]> {
        series.iter().map(Vec::as_slice).collect()
    }

    /// 320 rows (16 blocks of 20). `A` has drift +0.01 in blocks 0..8 and -0.01 in 8..16; `B` is
    /// the mirror image. Both carry the same alternating noise.
    fn mirror_pair() -> (Vec<f64>, Vec<f64>) {
        let mut a = Vec::with_capacity(320);
        let mut b = Vec::with_capacity(320);
        for i in 0..320 {
            let drift = if i / 20 < 8 { 0.01 } else { -0.01 };
            a.push(drift + noise(i));
            b.push(-drift + noise(i));
        }
        (a, b)
    }

    #[test]
    fn enumerates_exactly_12870_masks() {
        assert_eq!(combinations().count(), 12_870);
    }

    #[test]
    fn every_mask_has_eight_in_sample_and_eight_out_of_sample_blocks() {
        for mask in combinations() {
            assert!(mask <= 0xFFFF);
            assert_eq!(mask.count_ones(), 8);
            assert_eq!(((!mask) & 0xFFFF).count_ones(), 8);
        }
    }

    #[test]
    fn fewer_than_two_series_is_none() {
        let a = sine(320, 0.0, 0.01);
        assert_eq!(pbo(&[]), None);
        assert_eq!(pbo(&[a.as_slice()]), None);
    }

    #[test]
    fn series_of_different_lengths_is_none() {
        let a = sine(320, 0.0, 0.01);
        let b = sine(319, 1.0, 0.01);
        assert_eq!(pbo(&[a.as_slice(), b.as_slice()]), None);
    }

    #[test]
    fn thirty_one_sessions_is_none_and_thirty_two_is_some() {
        let a = sine(31, 0.0, 0.01);
        let b = sine(31, 1.0, 0.01);
        assert_eq!(pbo(&[a.as_slice(), b.as_slice()]), None);
        let a = sine(32, 0.0, 0.01);
        let b = sine(32, 1.0, 0.01);
        assert!(pbo(&[a.as_slice(), b.as_slice()]).is_some());
    }

    #[test]
    fn nan_or_infinity_in_a_series_is_none() {
        let mut a = sine(320, 0.0, 0.01);
        let b = sine(320, 1.0, 0.01);
        a[100] = f64::NAN;
        assert_eq!(pbo(&[a.as_slice(), b.as_slice()]), None);
        a[100] = f64::INFINITY;
        assert_eq!(pbo(&[a.as_slice(), b.as_slice()]), None);
    }

    #[test]
    fn identical_series_give_pbo_one_with_average_rank_one_and_a_half() {
        let a: Vec<f64> = (0..320).map(|i| 0.001 + noise(i)).collect();
        assert_eq!(pbo(&[a.as_slice(), a.as_slice()]), Some(1.0));
        // Two tied configurations: rank 1 + ½ = 1.5 of 2, so ω = 1.5 / 3 = 0.5.
        assert_eq!(omega_of(&[0.3, 0.3], 0), 0.5);
    }

    #[test]
    fn configuration_dominant_in_every_block_gives_pbo_zero() {
        let a: Vec<f64> = (0..320).map(|i| 0.002 + noise(i)).collect();
        let b: Vec<f64> = (0..320).map(noise).collect();
        assert_eq!(pbo(&[a.as_slice(), b.as_slice()]), Some(0.0));
    }

    #[test]
    fn perfectly_overfit_pair_has_intermediate_pbo_symmetric_under_swap() {
        let (a, b) = mirror_pair();
        let forward = pbo(&[a.as_slice(), b.as_slice()]).unwrap_or(f64::NAN);
        let swapped = pbo(&[b.as_slice(), a.as_slice()]).unwrap_or(f64::NAN);
        assert!(forward > 0.4 && forward < 1.0, "pbo = {forward}");
        assert!((forward - swapped).abs() <= 1e-12);
    }

    #[test]
    fn mirror_pair_in_sample_best_is_worst_out_of_sample_for_first_half() {
        let (a, b) = mirror_pair();
        let (table, shifts) = block_sums(&[a.as_slice(), b.as_slice()], 0, 20);
        let (a_blocks, b_blocks) = table.split_at(SPLITS);
        let (a_is, a_oos) = split_sharpes(a_blocks, shifts[0], 0xFF);
        let (b_is, b_oos) = split_sharpes(b_blocks, shifts[1], 0xFF);
        assert!(a_is > b_is);
        assert!(a_oos < b_oos);
        assert_eq!(best_index(&[a_is, b_is]), 0);
        assert_eq!(omega_of(&[a_oos, b_oos], 0), 1.0 / 3.0);
    }

    #[test]
    fn permuting_configurations_does_not_change_pbo() {
        let series = [
            sine(320, 0.0, 0.010),
            sine(320, 1.0, 0.007),
            sine(320, 2.5, 0.012),
        ];
        let original = pbo(&refs(&series)).unwrap_or(f64::NAN);
        let permuted = [series[2].clone(), series[0].clone(), series[1].clone()];
        let reordered = pbo(&refs(&permuted)).unwrap_or(f64::NAN);
        assert!((original - reordered).abs() <= 1e-12);
    }

    #[test]
    fn rows_dropped_from_the_start_do_not_change_pbo() {
        let (a, b) = mirror_pair();
        let base = pbo(&[a.as_slice(), b.as_slice()]).unwrap_or(f64::NAN);
        // T = 330 has remainder 10, so the first 10 rows are dropped.
        let prefix_a: Vec<f64> = (0..10).map(|i| 0.5 * i as f64 - 2.0).collect();
        let prefix_b: Vec<f64> = (0..10).map(|i| 0.3 * (i as f64).cos()).collect();
        let a_long: Vec<f64> = prefix_a.iter().chain(a.iter()).copied().collect();
        let b_long: Vec<f64> = prefix_b.iter().chain(b.iter()).copied().collect();
        let longer = pbo(&[a_long.as_slice(), b_long.as_slice()]).unwrap_or(f64::NAN);
        assert!((base - longer).abs() <= 1e-12);
    }

    #[test]
    fn constant_in_sample_configuration_is_never_selected_over_a_positive_one() {
        let constant = vec![0.01; 320];
        let positive: Vec<f64> = (0..320).map(|i| 0.002 + noise(i)).collect();
        let (table, shifts) = block_sums(&[constant.as_slice(), positive.as_slice()], 0, 20);
        let (constant_blocks, _) = table.split_at(SPLITS);
        for mask in combinations() {
            let (is, oos) = split_sharpes(constant_blocks, shifts[0], mask);
            assert_eq!(is, f64::NEG_INFINITY);
            assert_eq!(oos, f64::NEG_INFINITY);
        }
        // The constant configuration is never the in-sample best, so it always ranks last.
        assert_eq!(best_index(&[f64::NEG_INFINITY, 2.0]), 1);
        let result = pbo(&[constant.as_slice(), positive.as_slice()]);
        assert_eq!(result, Some(0.0));
    }

    #[test]
    fn result_is_a_multiple_of_one_over_12870_within_unit_interval() {
        let (a, b) = mirror_pair();
        let identical: Vec<f64> = (0..320).map(|i| 0.001 + noise(i)).collect();
        let dominant: Vec<f64> = (0..320).map(|i| 0.002 + noise(i)).collect();
        let noisy: Vec<Vec<f64>> = (0..3).map(|j| sine(320, j as f64, 0.01)).collect();
        let inputs: Vec<Vec<&[f64]>> = vec![
            vec![a.as_slice(), b.as_slice()],
            vec![identical.as_slice(), identical.as_slice()],
            vec![dominant.as_slice(), a.as_slice()],
            refs(&noisy),
        ];
        for input in &inputs {
            let p = pbo(input).unwrap_or(f64::NAN);
            assert!((0.0..=1.0).contains(&p), "pbo = {p}");
            let count = p * 12_870.0;
            assert!((count.round() - count).abs() <= 1e-9, "pbo = {p}");
        }
    }

    #[test]
    fn ten_configurations_of_640_rows_finish_quickly() {
        let series: Vec<Vec<f64>> = (0..10)
            .map(|j| sine(640, j as f64 * 0.7, 0.01 + 0.001 * j as f64))
            .collect();
        let started = std::time::Instant::now();
        let result = pbo(&refs(&series));
        eprintln!("pbo N=10 T=640 took {:?}", started.elapsed());
        assert!(result.is_some_and(f64::is_finite));
    }
}
