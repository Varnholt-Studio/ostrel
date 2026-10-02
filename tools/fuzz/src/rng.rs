//! Small deterministic pseudo random number generator (SplitMix64).
//!
//! The fuzzer must reproduce every input from its seed and iteration number alone,
//! on every platform and toolchain, so it does not use the randomized std hasher.

/// SplitMix64 generator. Not cryptographic; only used to pick fuzz inputs.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Creates a generator from a 64 bit seed.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Creates the generator for one input of a campaign: `(seed, iteration)` fully
    /// determines the stream, independent of every earlier iteration.
    pub fn for_input(seed: u64, iteration: u64) -> Self {
        let mut mixer = Self::new(seed ^ 0x6f73_7472_656c_2d66);
        let a = mixer.next_u64();
        Self::new(a ^ iteration.wrapping_mul(0x9e37_79b9_7f4a_7c15))
    }

    /// Returns the next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Returns a value in `0..n`; returns 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        // The modulo bias is irrelevant for input generation.
        (self.next_u64() % n as u64) as usize
    }

    /// Returns a value in `lo..=hi` (`lo` when the range is empty).
    pub fn range(&mut self, lo: usize, hi: usize) -> usize {
        if hi <= lo {
            return lo;
        }
        lo + self.below(hi - lo + 1)
    }

    /// Returns true with probability `percent` / 100.
    pub fn chance(&mut self, percent: u32) -> bool {
        self.below(100) < percent as usize
    }

    /// Picks one element of a non empty slice; returns `None` for an empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            items.get(self.below(items.len()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn same_seed_gives_same_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn known_first_value_is_stable() {
        // Reference value of SplitMix64 with seed 0; guards against accidental changes
        // that would make committed seeds reproduce different inputs.
        assert_eq!(Rng::new(0).next_u64(), 0xe220_a839_7b1d_cdaf);
    }

    #[test]
    fn inputs_are_independent_of_order() {
        let first = Rng::for_input(7, 3).next_u64();
        let _ = Rng::for_input(7, 2).next_u64();
        assert_eq!(Rng::for_input(7, 3).next_u64(), first);
        assert_ne!(Rng::for_input(7, 4).next_u64(), first);
        assert_ne!(Rng::for_input(8, 3).next_u64(), first);
    }

    #[test]
    fn bounds_hold() {
        let mut r = Rng::new(1);
        for _ in 0..10_000 {
            assert!(r.below(7) < 7);
            let v = r.range(3, 5);
            assert!((3..=5).contains(&v));
        }
        assert_eq!(r.below(0), 0);
        assert_eq!(r.range(9, 2), 9);
        assert!(r.pick::<u8>(&[]).is_none());
    }
}
