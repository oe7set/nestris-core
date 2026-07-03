//! Deterministic in-house RNG: PCG32 seeded via splitmix64.
//!
//! RANSAC (and any other randomized routine) draws exclusively from this so
//! runs are bit-identical across platforms and reproducible from a frame
//! sequence number. No `rand` dependency by design.

/// splitmix64: turns any 64-bit value (e.g. a frame seq) into a good seed.
#[inline]
pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// PCG32 (XSH-RR variant), the reference O'Neill generator.
#[derive(Clone, Debug)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    /// Seed from any 64-bit value (splitmix64-expanded into state + stream).
    pub fn new(seed: u64) -> Self {
        let s0 = splitmix64(seed);
        let s1 = splitmix64(s0);
        let mut rng = Self {
            state: 0,
            inc: (s1 << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(s0);
        rng.next_u32();
        rng
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform integer in `[0, bound)` via Lemire-style rejection (unbiased).
    #[inline]
    pub fn next_below(&mut self, bound: u32) -> u32 {
        debug_assert!(bound > 0);
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u32();
            if r >= threshold {
                return r % bound;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_across_instances() {
        let a: Vec<u32> = {
            let mut r = Pcg32::new(splitmix64(1234));
            (0..8).map(|_| r.next_u32()).collect()
        };
        let b: Vec<u32> = {
            let mut r = Pcg32::new(splitmix64(1234));
            (0..8).map(|_| r.next_u32()).collect()
        };
        assert_eq!(a, b);
        let c = Pcg32::new(splitmix64(1235)).next_u32();
        assert_ne!(a[0], c);
    }

    #[test]
    fn next_below_in_range() {
        let mut r = Pcg32::new(7);
        for _ in 0..1000 {
            assert!(r.next_below(24) < 24);
        }
    }
}
