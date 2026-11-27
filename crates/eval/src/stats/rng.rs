//! A small seeded generator, so a bootstrap reproduces exactly from its stored seed.

/// xoshiro256** seeded through SplitMix64. Not for cryptography.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    state: [u64; 4],
}

/// One SplitMix64 step: advances `state` by the golden-ratio increment and returns the
/// mixed output.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut x = *state;
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl Rng {
    /// The generator for `seed`: the four state words are four successive SplitMix64 outputs
    /// starting from `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        let mut z = seed;
        let state = [
            splitmix(&mut z),
            splitmix(&mut z),
            splitmix(&mut z),
            splitmix(&mut z),
        ];
        Self { state }
    }

    /// The next 64 bits (xoshiro256**).
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.state;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// A uniform value in `[0, 1)`: the top 53 bits of `next_u64` times 2⁻⁵³.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// A uniform index in `0..n` without modulo bias (rejection sampling). `n` must be at
    /// least 1; for `n == 0` returns 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let n = n as u64;
        let zone = u64::MAX - (u64::MAX % n + 1) % n;
        loop {
            let v = self.next_u64();
            if v <= zone {
                return (v % n) as usize;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Rng, splitmix};

    #[test]
    fn splitmix_matches_reference_values_for_seed_zero() {
        let mut z = 0_u64;
        assert_eq!(splitmix(&mut z), 0xE220_A839_7B1D_CDAF);
        assert_eq!(splitmix(&mut z), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(splitmix(&mut z), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn new_seeds_state_from_splitmix_outputs() {
        let rng = Rng::new(0);
        assert_eq!(rng.state[0], 0xE220_A839_7B1D_CDAF);
        assert_eq!(rng.state[1], 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(rng.state[2], 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn same_seed_gives_same_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1_000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_differ_in_first_output() {
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
    }

    #[test]
    fn next_f64_is_in_unit_interval_with_mean_near_half() {
        let mut rng = Rng::new(7);
        let mut sum = 0.0;
        for _ in 0..100_000 {
            let x = rng.next_f64();
            assert!((0.0..1.0).contains(&x));
            sum += x;
        }
        let mean = sum / 100_000.0;
        assert!((mean - 0.5).abs() < 0.01, "mean {mean}");
    }

    #[test]
    fn below_is_always_less_than_n() {
        let mut rng = Rng::new(3);
        for n in [1_usize, 2, 3, 7, 1000] {
            for _ in 0..10_000 {
                assert!(rng.below(n) < n);
            }
        }
    }

    #[test]
    fn below_one_and_zero_return_zero() {
        let mut rng = Rng::new(5);
        for _ in 0..1_000 {
            assert_eq!(rng.below(1), 0);
            assert_eq!(rng.below(0), 0);
        }
    }

    #[test]
    fn below_six_faces_are_roughly_uniform() {
        let mut rng = Rng::new(11);
        let mut counts = [0_u32; 6];
        for _ in 0..60_000 {
            counts[rng.below(6)] += 1;
        }
        for c in counts {
            assert!(c.abs_diff(10_000) <= 400, "count {c}");
        }
    }

    #[test]
    fn state_never_becomes_all_zero() {
        let mut rng = Rng::new(0);
        for _ in 0..10_000 {
            rng.next_u64();
            assert!(rng.state.iter().any(|&w| w != 0));
        }
    }
}
