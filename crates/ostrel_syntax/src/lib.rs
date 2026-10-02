//! Lexer, parser and syntax tree of the Ostrel programming language.
//!
//! The lexer is in [`lex`], the syntax tree in [`ast`]. The parser follows
//! according to the v0.1 build plan.
//!
//! The parse limits of D54 live in [`ast`] and are re-exported here, so the
//! parser and its callers name one source for them.

// This crate handles untrusted input: no unwrap, expect, panic or unchecked indexing.
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

pub mod ast;
pub mod lex;

pub use ast::{Limits, MAX_HEIGHT, MAX_NODES};

#[cfg(test)]
pub(crate) mod test_support {
    //! Helpers shared by the unit tests of this crate.

    use std::time::{Duration, Instant};

    /// Growth factor between the small and the large input.
    const FACTOR: usize = 16;
    /// Largest accepted ratio of the large run to the batch of small runs.
    /// Linear work gives about 1, quadratic work about `FACTOR`.
    const MAX_RATIO: u32 = 4;
    /// Interleaved rounds; the fastest measurement of each side counts.
    const ROUNDS: usize = 3;

    /// Asserts that `run` does linear work in the size of its input, without
    /// any absolute time bound (D74).
    ///
    /// One run on an input of size `small * FACTOR` is compared with `FACTOR`
    /// runs on separate inputs of size `small`. Both sides do the same amount of
    /// linear work and keep the same amount of memory alive until they are
    /// timed, so cache and allocator effects hit both alike; quadratic work
    /// makes the single run `FACTOR` times slower. `build(n)` makes an input of
    /// size `n` and is not timed. The sides run in alternation and only the
    /// fastest measurement of each counts, so load on the machine slows both
    /// and does not fail the test by itself.
    pub(crate) fn assert_linear<T, R>(
        small: usize,
        build: impl Fn(usize) -> T,
        run: impl Fn(&T) -> R,
    ) {
        let smalls: Vec<T> = (0..FACTOR).map(|_| build(small)).collect();
        let large = build(small * FACTOR);
        let mut batch_time = Duration::MAX;
        let mut large_time = Duration::MAX;
        for _ in 0..ROUNDS {
            let start = Instant::now();
            let outputs: Vec<R> = smalls.iter().map(&run).collect();
            batch_time = batch_time.min(start.elapsed());
            drop(outputs);

            let start = Instant::now();
            let output = run(&large);
            large_time = large_time.min(start.elapsed());
            drop(output);
        }
        let bound = batch_time.max(Duration::from_micros(100)) * MAX_RATIO;
        assert!(
            large_time < bound,
            "not linear: {batch_time:?} for {FACTOR} inputs of size {small}, \
             {large_time:?} for one input of size {}",
            small * FACTOR
        );
    }
}
