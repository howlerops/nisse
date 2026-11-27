//! The standard normal distribution: `std` has neither function.

use std::f64::consts::TAU;

/// Abramowitz and Stegun 26.2.17 polynomial coefficients b1..b5.
const CDF_B: [f64; 5] = [
    0.319_381_530,
    -0.356_563_782,
    1.781_477_937,
    -1.821_255_978,
    1.330_274_429,
];

/// Acklam inverse-cdf numerator coefficients for the central region.
const INV_A: [f64; 6] = [
    -3.969_683_028_665_376e1,
    2.209_460_984_245_205e2,
    -2.759_285_104_469_687e2,
    1.383_577_518_672_69e2,
    -3.066_479_806_614_716e1,
    2.506_628_277_459_239,
];

/// Acklam inverse-cdf denominator coefficients for the central region.
const INV_B: [f64; 5] = [
    -5.447_609_879_822_406e1,
    1.615_858_368_580_409e2,
    -1.556_989_798_598_866e2,
    6.680_131_188_771_972e1,
    -1.328_068_155_288_572e1,
];

/// Acklam inverse-cdf coefficients tail numerator.
const INV_C: [f64; 6] = [
    -7.784_894_002_430_293e-3,
    -3.223_964_580_411_365e-1,
    -2.400_758_277_161_838,
    -2.549_732_539_343_734,
    4.374_664_141_464_968,
    2.938_163_982_698_783,
];

/// Acklam inverse-cdf coefficients tail denominator.
const INV_D: [f64; 4] = [
    7.784_695_709_041_462e-3,
    3.224_671_290_700_398e-1,
    2.445_134_137_142_996,
    3.754_408_661_907_416,
];

/// Lower tail boundary of Acklam's approximation.
const P_LOW: f64 = 0.02425;

/// Φ(x), the standard normal cumulative distribution (Abramowitz and Stegun 26.2.17,
/// absolute error below 7.5e-8). Total on finite input, with `cdf(-inf) = 0` and
/// `cdf(inf) = 1`; a NaN input gives NaN, so callers check their inputs first.
#[must_use]
pub fn cdf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x == f64::INFINITY {
        return 1.0;
    }
    if x == f64::NEG_INFINITY {
        return 0.0;
    }
    if x < 0.0 {
        return 1.0 - cdf(-x);
    }
    let t = 1.0 / (1.0 + 0.2316419 * x);
    let poly = ((((CDF_B[4] * t + CDF_B[3]) * t + CDF_B[2]) * t + CDF_B[1]) * t + CDF_B[0]) * t;
    let phi = (-x * x / 2.0).exp() / TAU.sqrt();
    (1.0 - phi * poly).clamp(0.0, 1.0)
}

/// Φ⁻¹(p) for `0 < p < 1` (Acklam's rational approximation, relative error about 1e-9);
/// `None` otherwise, including for NaN.
#[must_use]
pub fn inv_cdf(p: f64) -> Option<f64> {
    if !(p > 0.0 && p < 1.0) {
        return None;
    }
    let x = if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        tail(q)
    } else if p > 1.0 - P_LOW {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -tail(q)
    } else {
        let q = p - 0.5;
        let r = q * q;
        let num = ((((INV_A[0] * r + INV_A[1]) * r + INV_A[2]) * r + INV_A[3]) * r + INV_A[4]) * r
            + INV_A[5];
        let den =
            ((((INV_B[0] * r + INV_B[1]) * r + INV_B[2]) * r + INV_B[3]) * r + INV_B[4]) * r + 1.0;
        num * q / den
    };
    Some(x)
}

/// Acklam tail rational function in `q = sqrt(-2 ln p)`.
fn tail(q: f64) -> f64 {
    let num =
        ((((INV_C[0] * q + INV_C[1]) * q + INV_C[2]) * q + INV_C[3]) * q + INV_C[4]) * q + INV_C[5];
    let den = (((INV_D[0] * q + INV_D[1]) * q + INV_D[2]) * q + INV_D[3]) * q + 1.0;
    num / den
}

#[cfg(test)]
mod tests {
    use super::{cdf, inv_cdf};

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    /// Grid -5.0 to 5.0 step 0.25, built from integers so it is exact.
    fn grid() -> impl Iterator<Item = f64> {
        (0..=40).map(|i| -5.0 + f64::from(i) * 0.25)
    }

    #[test]
    fn cdf_reference_values() {
        let tol = 1e-7;
        assert!(close(cdf(0.0), 0.5, tol));
        assert!(close(cdf(1.0), 0.841_344_746_1, tol));
        assert!(close(cdf(2.0), 0.977_249_868_1, tol));
        assert!(close(cdf(1.96), 0.975_002_104_9, tol));
        assert!(close(cdf(-1.0), 0.158_655_253_9, tol));
        assert!(close(cdf(-3.0), 0.001_349_898_0, tol));
        assert!(close(cdf(3.0), 0.998_650_102_0, tol));
        assert!(close(cdf(0.5), 0.691_462_461_3, tol));
        assert!(close(cdf(6.0), 1.0, tol));
    }

    #[test]
    fn cdf_symmetry() {
        // x = 0 is excluded: for x < 0 the function is defined as 1 - cdf(-x), but at 0 both
        // sides take the x >= 0 branch, so the approximation error (about 5e-10) doubles.
        for x in grid().filter(|&x| x != 0.0) {
            assert!(close(cdf(x) + cdf(-x), 1.0, 1e-12), "x = {x}");
        }
    }

    #[test]
    fn cdf_monotone_and_bounded() {
        let mut prev = f64::NEG_INFINITY;
        for x in grid() {
            let v = cdf(x);
            assert!((0.0..=1.0).contains(&v), "x = {x}, v = {v}");
            assert!(v >= prev, "not monotone at x = {x}");
            prev = v;
        }
    }

    #[test]
    fn cdf_infinities_and_nan() {
        assert_eq!(cdf(f64::INFINITY), 1.0);
        assert_eq!(cdf(f64::NEG_INFINITY), 0.0);
        assert!(cdf(f64::NAN).is_nan());
    }

    #[test]
    fn inv_cdf_reference_values() {
        let tol = 1e-6;
        assert!(close(inv_cdf(0.5).expect("in range"), 0.0, 1e-9));
        assert!(close(inv_cdf(0.975).expect("in range"), 1.959_963_985, tol));
        assert!(close(
            inv_cdf(0.025).expect("in range"),
            -1.959_963_985,
            tol
        ));
        assert!(close(inv_cdf(0.841_344_746_1).expect("in range"), 1.0, tol));
        assert!(close(
            inv_cdf(0.001).expect("in range"),
            -3.090_232_306,
            tol
        ));
        assert!(close(inv_cdf(0.999).expect("in range"), 3.090_232_306, tol));
        assert!(close(inv_cdf(0.01).expect("in range"), -2.326_347_874, tol));
        assert!(close(
            inv_cdf(1e-10).expect("in range"),
            -6.361_340_902,
            1e-5
        ));
    }

    #[test]
    fn inv_cdf_branch_boundary() {
        let lo = inv_cdf(0.02425).expect("in range");
        let hi = inv_cdf(1.0 - 0.02425).expect("in range");
        assert!(lo.is_finite() && hi.is_finite());
        assert!(close(lo, -hi, 1e-9), "lo = {lo}, hi = {hi}");
    }

    #[test]
    fn inv_cdf_none_outside_open_interval() {
        for p in [0.0, 1.0, -0.1, 1.1, f64::NAN] {
            assert!(inv_cdf(p).is_none(), "p = {p}");
        }
    }

    #[test]
    fn inv_cdf_antisymmetry() {
        for p in [0.001, 0.01, 0.1, 0.3, 0.45] {
            let s = inv_cdf(p).expect("in range") + inv_cdf(1.0 - p).expect("in range");
            assert!(close(s, 0.0, 1e-8), "p = {p}, sum = {s}");
        }
    }

    #[test]
    fn inv_cdf_round_trip() {
        for i in 1..=999 {
            let p = f64::from(i) * 0.001;
            let x = inv_cdf(p).expect("in range");
            assert!(close(cdf(x), p, 2e-7), "p = {p}, x = {x}, cdf = {}", cdf(x));
        }
    }

    #[test]
    fn inv_cdf_strictly_increasing() {
        let mut prev = f64::NEG_INFINITY;
        for i in 1..=999 {
            let x = inv_cdf(f64::from(i) * 0.001).expect("in range");
            assert!(x > prev, "not increasing at i = {i}");
            prev = x;
        }
    }
}
