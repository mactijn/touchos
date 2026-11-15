//! How long the station waits before it tries again.

/// The longest wait: failures in a row double the wait up to this, so an
/// access point that is restarting (or gone for hours) costs a scan a
/// minute, and is found again within a minute of its return.
pub const MAX_WAIT_MS: u64 = 60_000;

/// Failures in a row since the last successful connection.
#[derive(Debug, Default)]
pub struct Backoff(u32);

impl Backoff {
    /// The wait after one more failure: `first`, doubled for each failure
    /// before it, at most [`MAX_WAIT_MS`].
    pub fn next(&mut self, first: u64) -> u64 {
        let wait = first.saturating_mul(1 << self.0.min(16)).min(MAX_WAIT_MS);
        self.0 = self.0.saturating_add(1);
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_up_to_a_minute() {
        let mut b = Backoff::default();
        let waits: Vec<u64> = (0..6).map(|_| b.next(3000)).collect();
        assert_eq!(waits, [3000, 6000, 12_000, 24_000, 48_000, 60_000]);
        // Days of failures: still a minute, no overflow.
        for _ in 0..100_000 {
            assert_eq!(b.next(10_000), MAX_WAIT_MS);
        }
        // The first wait counts from where the failures are.
        let mut b = Backoff::default();
        assert_eq!(b.next(10_000), 10_000);
        assert_eq!(b.next(3000), 6000);
        assert_eq!(Backoff::default().next(u64::MAX), MAX_WAIT_MS);
    }
}
