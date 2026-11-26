//! A small seeded generator, so a bootstrap reproduces exactly from its stored seed.

/// xoshiro256** seeded through SplitMix64. Not for cryptography.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    state: [u64; 4],
}

#[allow(unused_variables, reason = "stub: Phase 5 task ST3")]
impl Rng {
    /// The generator for `seed`: the four state words are four successive SplitMix64 outputs
    /// starting from `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        todo!("Phase 5 task ST3")
    }

    /// The next 64 bits (xoshiro256**).
    pub fn next_u64(&mut self) -> u64 {
        todo!("Phase 5 task ST3")
    }

    /// A uniform value in `[0, 1)`: the top 53 bits of `next_u64` times 2⁻⁵³.
    pub fn next_f64(&mut self) -> f64 {
        todo!("Phase 5 task ST3")
    }

    /// A uniform index in `0..n` without modulo bias (rejection sampling). `n` must be at
    /// least 1; for `n == 0` returns 0.
    pub fn below(&mut self, n: usize) -> usize {
        todo!("Phase 5 task ST3")
    }
}
