//! When a copy of the system on trial is confirmed or sent back: init asks
//! [`health`] and acts on the answer (the boot stage in NAND counts the
//! starts and keeps the state record).

/// The other copy: 'A' <-> 'B'.
pub fn other(slot: char) -> char {
    if slot == 'A' { 'B' } else { 'A' }
}

/// A copy is healthy after running this long without a program failure,
/// with a network address (ms since boot).
pub const CONFIRM_AFTER_MS: u64 = 60_000;
/// A copy on trial that isn't healthy this long after boot goes back.
pub const TRIAL_LIMIT_MS: u64 = 5 * 60_000;

/// What a running copy does about its health (init asks, and acts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Healthy: make it the active copy (if it isn't).
    Confirm,
    /// On trial and failing: restart, so the active copy starts next.
    GoBack(Why),
    /// The active copy has failures: nothing to confirm; stop checking.
    Stop,
    /// Ask again later.
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A program failed.
    Failure,
    /// No network address within TRIAL_LIMIT_MS.
    NoNetwork,
}

/// The health rule. `failures`: program failures since boot; `network`: a
/// network address, or none needed; `elapsed_ms`: since boot. A copy on
/// trial goes back at its first failure, or when it isn't healthy within
/// [`TRIAL_LIMIT_MS`]; any copy is confirmed once healthy, from
/// [`CONFIRM_AFTER_MS`] on.
pub fn health(on_trial: bool, failures: u32, network: bool, elapsed_ms: u64) -> Health {
    if on_trial && failures > 0 {
        return Health::GoBack(Why::Failure);
    }
    if elapsed_ms < CONFIRM_AFTER_MS {
        return Health::Wait;
    }
    if failures == 0 && network {
        return Health::Confirm;
    }
    if on_trial && elapsed_ms >= TRIAL_LIMIT_MS {
        return Health::GoBack(Why::NoNetwork);
    }
    if !on_trial && failures > 0 {
        return Health::Stop;
    }
    Health::Wait
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_rule() {
        use Health::*;
        let min = 60_000;
        // On trial: the first failure sends it back, at any time.
        assert_eq!(health(true, 1, true, 1_000), GoBack(Why::Failure));
        // Healthy only after a minute.
        assert_eq!(health(true, 0, true, min - 1), Wait);
        assert_eq!(health(true, 0, true, min), Confirm);
        assert_eq!(health(false, 0, true, min), Confirm);
        // No network: wait, and a copy on trial gives up after 5 minutes.
        assert_eq!(health(true, 0, false, 4 * min), Wait);
        assert_eq!(health(true, 0, false, 5 * min), GoBack(Why::NoNetwork));
        assert_eq!(health(false, 0, false, 10 * min), Wait);
        // The active copy with failures: nothing to confirm.
        assert_eq!(health(false, 2, true, min), Stop);
        assert_eq!(health(false, 2, true, min - 1), Wait);
    }

    #[test]
    fn the_other_copy() {
        assert_eq!(other('A'), 'B');
        assert_eq!(other('B'), 'A');
    }
}
