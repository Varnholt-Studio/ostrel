//! The output budget of one run (ARCHITECTURE 3.4 and 5.9, D113).
//!
//! `print` writes at most [`MAX_OUTPUT_BYTES`] bytes in one run, the newline of each
//! line included. The limit is fixed: there is no flag and no field in
//! [`crate::Limits`]. A `print` that would exceed it writes nothing, not even a
//! part, and the run fails with `OutputLimit`.

use crate::RuntimeErrorKind;

/// Maximum number of bytes `print` writes in one run, newlines included (256 MiB).
pub(crate) const MAX_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Bytes written by `print` so far in one run, and the most it may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Output {
    written: u64,
    max: u64,
}

impl Output {
    /// An empty budget of `max` bytes.
    pub(crate) fn new(max: u64) -> Self {
        Output { written: 0, max }
    }

    /// Bytes written so far.
    #[cfg(test)]
    pub(crate) fn written(&self) -> u64 {
        self.written
    }

    /// Total bytes after a line of `text_len` bytes and its newline, if that total
    /// stays within the budget.
    fn after(&self, text_len: u64) -> Option<u64> {
        self.written
            .checked_add(text_len)
            .and_then(|total| total.checked_add(1))
            .filter(|total| *total <= self.max)
    }

    /// Whether a line of `text_len` bytes and its newline still fit.
    pub(crate) fn fits(&self, text_len: u64) -> bool {
        self.after(text_len).is_some()
    }

    /// Records a line of `text_len` bytes and its newline before it is written, or
    /// fails with `OutputLimit` and records nothing if it would exceed the budget.
    pub(crate) fn admit(&mut self, text_len: u64) -> Result<(), RuntimeErrorKind> {
        match self.after(text_len) {
            Some(total) => {
                self.written = total;
                Ok(())
            }
            None => Err(RuntimeErrorKind::OutputLimit),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_256_mib() {
        assert_eq!(MAX_OUTPUT_BYTES, 268_435_456);
    }

    #[test]
    fn newline_counts_and_the_exact_limit_fits() {
        let mut out = Output::new(10);
        assert!(out.fits(9));
        assert!(!out.fits(10));
        assert_eq!(out.admit(4), Ok(()));
        assert_eq!(out.written(), 5);
        assert_eq!(out.admit(4), Ok(()));
        assert_eq!(out.written(), 10);
        assert!(!out.fits(0));
        assert_eq!(out.admit(0), Err(RuntimeErrorKind::OutputLimit));
        assert_eq!(out.written(), 10);
    }

    #[test]
    fn refused_line_records_nothing() {
        let mut out = Output::new(10);
        assert_eq!(out.admit(5), Ok(()));
        assert_eq!(out.admit(5), Err(RuntimeErrorKind::OutputLimit));
        assert_eq!(out.written(), 6);
        assert_eq!(out.admit(3), Ok(()));
        assert_eq!(out.written(), 10);
    }

    #[test]
    fn huge_lengths_do_not_overflow() {
        let mut out = Output::new(u64::MAX);
        assert_eq!(out.admit(u64::MAX - 1), Ok(()));
        assert_eq!(out.admit(0), Err(RuntimeErrorKind::OutputLimit));
        assert!(!Output::new(u64::MAX).fits(u64::MAX));
    }
}
