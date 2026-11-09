//! Deterministic pseudo-random number generation.
//!
//! A minimal, allocation-free PRNG layer used throughout the pipeline for
//! reproducible variety: score jitter in the picker, speed-ramp coin flips,
//! random-seed generation. All functions are pure (no hidden state) so every
//! caller gets the same output for the same `(seed, index)` pair regardless
//! of call order.
//!
//! # Algorithm: SplitMix64
//!
//! SplitMix64 was designed for fast, high-quality output from a counter-based
//! stream — exactly the use case here where `index` is the counter. It passes
//! BigCrush and produces well-distributed outputs from sparse inputs (small
//! seeds, sequential indices).
//!
//! Reference: Guy L. Steele Jr., Doug Lea, Christine H. Flood, "Fast Splittable
//! Pseudorandom Number Generators", OOPSLA 2014.

/// Golden ratio-derived stream constant. Adding this to consecutive integer
/// seeds before mixing gives a well-distributed sequence even for small inputs.
pub const GOLDEN_RATIO_64: u64 = 0x9e3779b97f4a7c15;

/// SplitMix64 finalizer: thoroughly avalanches all bits of `x`.
///
/// This is the core mixing step. Call it on `seed.wrapping_add(idx *
/// GOLDEN_RATIO_64)` to produce an independent value per `(seed, idx)` pair.
#[inline]
pub fn splitmix64(x: u64) -> u64 {
    let mut x = x;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

/// Derive a deterministic `f32` in `[0, 1)` from `(seed, idx)`.
///
/// Uses the high 24 bits (mantissa precision for `f32`) of the SplitMix64
/// output to avoid low-bit weaknesses. Suitable for coin-flip comparisons:
/// `coin_f32(seed, i) < probability`.
#[inline]
pub fn coin_f32(seed: u64, idx: usize) -> f32 {
    let x = splitmix64(seed.wrapping_add((idx as u64).wrapping_mul(GOLDEN_RATIO_64)));
    (x >> 40) as f32 / (1u32 << 24) as f32
}

/// Derive a deterministic `f32` in `[-1, 1)` from `(seed, idx)`.
///
/// Used for multiplicative score jitter in the picker's variety mode: the
/// same `(project_seed, candidate_index)` pair always yields the same jitter
/// factor, so re-running with the same seed produces the same cut.
#[inline]
pub fn jitter_f32(seed: u64, idx: usize) -> f32 {
    coin_f32(seed, idx) * 2.0 - 1.0
}

/// The largest seed a fresh one ([`clock_seed`]) can be: 2^53 − 1.
///
/// A seed has to survive being written back as `seed = N` — TOML integers
/// are signed 64-bit, so a full-range `u64` fails to serialise about half
/// the time — and the browser, which reads the config as JSON into an
/// `f64` that holds integers exactly only up to here
/// (`Number.MAX_SAFE_INTEGER`).
pub const MAX_FRESH_SEED: u64 = (1 << 53) - 1;

/// A fresh seed from a clock reading in nanoseconds: SplitMix64 over the
/// golden-ratio-advanced counter (so readings a nanosecond apart give
/// unrelated seeds), kept to its top 53 bits — see [`MAX_FRESH_SEED`].
#[inline]
pub fn seed_from_nanos(nanos: u64) -> u64 {
    splitmix64(nanos.wrapping_add(GOLDEN_RATIO_64)) >> 11
}

/// A fresh seed from the system clock (`--random-seed`, the Shuffle
/// feedback). Always `<= MAX_FRESH_SEED`, so it can be written to a
/// config file and pasted back as `seed = N`.
pub fn clock_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xdeadbeef);
    seed_from_nanos(nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_seeds_fit_a_toml_integer_and_a_js_number() {
        // The full-range SplitMix64 output these replaced is >= 2^63 about
        // half the time; a thousand readings would have hit one.
        let mut high = 0u64;
        for i in 0..1000u64 {
            let s = seed_from_nanos(1_700_000_000_000_000_000 + i * 997);
            assert!(s <= MAX_FRESH_SEED, "seed {s} is past 2^53 - 1");
            high = high.max(s);
        }
        // Still spread over the range, not stuck low.
        assert!(high > MAX_FRESH_SEED / 2, "seeds don't use the range ({high})");
        assert!(clock_seed() <= MAX_FRESH_SEED);
        assert!(MAX_FRESH_SEED <= i64::MAX as u64);
    }

    #[test]
    fn coin_f32_is_in_range() {
        for i in 0..1000 {
            let v = coin_f32(42, i);
            assert!(v >= 0.0 && v < 1.0, "coin_f32 out of [0,1): {v}");
        }
    }

    #[test]
    fn jitter_f32_is_in_range() {
        for i in 0..1000 {
            let v = jitter_f32(42, i);
            assert!(v >= -1.0 && v < 1.0, "jitter_f32 out of [-1,1): {v}");
        }
    }

    #[test]
    fn different_seeds_produce_different_output() {
        let a = coin_f32(1, 0);
        let b = coin_f32(2, 0);
        assert_ne!(a, b, "different seeds should produce different values");
    }

    #[test]
    fn same_seed_is_reproducible() {
        assert_eq!(coin_f32(99, 7), coin_f32(99, 7));
    }
}
