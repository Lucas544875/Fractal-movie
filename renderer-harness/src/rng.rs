//! Small deterministic generator for harness-side randomness. The mandate and
//! the scouting sampler must be reproducible from a recorded seed, and must
//! not depend on an LLM's notion of "random".

/// SplitMix64. Adequate for sampling categorical constraints and parameters.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform index in `0..bound`. `bound` must be non-zero.
    pub fn below(&mut self, bound: usize) -> usize {
        assert!(bound > 0, "bound must be non-zero");
        // Multiply-shift keeps the modulo bias far below anything observable
        // for the tiny bounds used here.
        ((u128::from(self.next_u64()) * bound as u128) >> 64) as usize
    }

    /// Uniform value in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform value in `[low, high)`.
    pub fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// True with the given probability.
    pub fn chance(&mut self, probability: f64) -> bool {
        self.unit() < probability
    }

    /// Uniformly distributed direction on the unit sphere.
    pub fn unit_vector(&mut self) -> [f64; 3] {
        let z = self.range(-1.0, 1.0);
        let angle = self.range(0.0, std::f64::consts::TAU);
        let radius = (1.0 - z * z).max(0.0).sqrt();
        [radius * angle.cos(), radius * angle.sin(), z]
    }

    /// Removes and returns a uniformly chosen element.
    pub fn take<T>(&mut self, items: &mut Vec<T>) -> Option<T> {
        if items.is_empty() {
            None
        } else {
            Some(items.swap_remove(self.below(items.len())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_reproduces_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..32 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn below_stays_in_range_and_covers_it() {
        let mut rng = Rng::new(7);
        let mut seen = [false; 5];
        for _ in 0..500 {
            seen[rng.below(5)] = true;
        }
        assert!(seen.iter().all(|hit| *hit));
    }

    #[test]
    fn unit_vectors_have_unit_length_and_cover_both_hemispheres() {
        let mut rng = Rng::new(9);
        let mut up = 0;
        for _ in 0..200 {
            let [x, y, z] = rng.unit_vector();
            assert!(((x * x + y * y + z * z).sqrt() - 1.0).abs() < 1e-12);
            up += usize::from(z > 0.0);
        }
        assert!((60..140).contains(&up), "{up}");
    }

    #[test]
    fn range_and_chance_respect_their_bounds() {
        let mut rng = Rng::new(4);
        for _ in 0..200 {
            assert!((2.0..5.0).contains(&rng.range(2.0, 5.0)));
        }
        assert!(!rng.chance(0.0));
        assert!(rng.chance(1.0));
    }

    #[test]
    fn unit_is_half_open() {
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            let value = rng.unit();
            assert!((0.0..1.0).contains(&value));
        }
    }
}
