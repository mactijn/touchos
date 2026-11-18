/*
 * Copyright (c) 2006
 *	Damien Bergamini <damien.bergamini@free.fr>
 *
 * Permission to use, copy, modify, and distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
 * ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
 * ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
 * OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

//! AMRR rate control (Adaptive Multi Rate Retry; Lacage, Manshaei and
//! Turletti, 2004): a port of OpenBSD's `ieee80211_amrr.c` (ISC license
//! above), with the thresholds its run(4) uses. It steps through any
//! ordered list of rates: the legacy rates, or the HT MCS (as OpenBSD's
//! does for an HT node).
//!
//! For chips that report only totals (frames sent, retries, failures)
//! rather than a result per frame: when few frames needed a retry, step up
//! a rate; when many did, step down. A step up that fails at once makes the
//! next attempt wait twice as long (up to [`MAX_SUCCESS_THRESHOLD`]
//! good periods), so a link at the edge of a rate doesn't flap every time.
//!
//! One deviation from OpenBSD: a step down also waits for enough
//! transmissions ([`Amrr::choose`] keeps counting until then). OpenBSD
//! steps down on any sample, which on a quiet link (the Toon's dashboard
//! sends a few frames a second) means one retry in two frames costs a
//! rate: in an hour on the Toon that walked the rate from 54 down to
//! 6 Mbit/s with no frame lost. A link that really fails still gets there
//! fast, since every retry counts as a transmission.

use alloc::vec::Vec;

/// Good periods in a row before stepping up (run(4)'s values).
pub const MIN_SUCCESS_THRESHOLD: u32 = 1;
pub const MAX_SUCCESS_THRESHOLD: u32 = 10;

/// Which way [`Amrr::choose`] moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
}

/// One peer's rate (a station has one: its access point). `R` is the rate
/// type: `u8` in 500 kbit/s units, or [`crate::rates::Rate`].
#[derive(Debug, Clone)]
pub struct Amrr<R = u8> {
    /// The rates to choose from, slowest first.
    rates: Vec<R>,
    /// Index of the rate in use.
    cur: usize,
    /// Good periods in a row.
    success: u32,
    success_threshold: u32,
    /// The last step was up (a failure now means that step was too far).
    recovery: bool,
    /// Transmissions and retransmissions since the counts were last reset.
    txcnt: u32,
    retrycnt: u32,
}

impl<R: Copy + PartialOrd> Amrr<R> {
    /// Rate control over `rates` (slowest first, not empty), starting at
    /// `start` (or the nearest rate below it in the set).
    /// (`ieee80211_amrr_node_init`; OpenBSD starts at the lowest rate.)
    pub fn new(rates: Vec<R>, start: R) -> Amrr<R> {
        assert!(!rates.is_empty());
        let cur = rates.iter().rposition(|&r| r <= start).unwrap_or(0);
        Amrr {
            rates,
            cur,
            success: 0,
            success_threshold: MIN_SUCCESS_THRESHOLD,
            recovery: false,
            txcnt: 0,
            retrycnt: 0,
        }
    }

    /// The rate to send data at.
    pub fn rate(&self) -> R {
        self.rates[self.cur]
    }

    /// Add the chip's TX statistics: frames sent and ACKed, frames given
    /// up on, and retransmissions. As run(4) counts them: a failure is a
    /// retry too, and every retry a transmission.
    pub fn add(&mut self, ok: u32, failed: u32, retries: u32) {
        let retry = failed + retries;
        self.retrycnt += retry;
        self.txcnt += retry + ok;
    }

    /// (transmissions, retransmissions) counted towards the next
    /// [`Amrr::choose`], for the log.
    pub fn counts(&self) -> (u32, u32) {
        (self.txcnt, self.retrycnt)
    }

    fn is_success(&self) -> bool {
        self.retrycnt < self.txcnt / 10
    }

    fn is_failure(&self) -> bool {
        self.retrycnt > self.txcnt / 3
    }

    /// Enough frames to judge the rate by.
    fn is_enough(&self) -> bool {
        self.txcnt > 10
    }

    /// Judge the counts so far, maybe change the rate (`ieee80211_amrr_choose`).
    /// Call it about once a second.
    pub fn choose(&mut self) -> Option<Step> {
        let mut step = None;
        if self.is_success() && self.is_enough() {
            self.success += 1;
            if self.success >= self.success_threshold && self.cur + 1 < self.rates.len() {
                self.recovery = true;
                self.success = 0;
                self.cur += 1;
                step = Some(Step::Up);
            } else {
                self.recovery = false;
            }
        } else if self.is_failure() && self.is_enough() {
            self.success = 0;
            if self.cur > 0 {
                self.success_threshold = if self.recovery {
                    (self.success_threshold * 2).min(MAX_SUCCESS_THRESHOLD)
                } else {
                    MIN_SUCCESS_THRESHOLD
                };
                self.cur -= 1;
                step = Some(Step::Down);
            }
            self.recovery = false; // "original paper was incorrect"
        }
        if self.is_enough() || step.is_some() {
            self.txcnt = 0;
            self.retrycnt = 0;
        }
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rates::OFDM;

    fn amrr() -> Amrr {
        Amrr::new(OFDM.to_vec(), 48)
    }

    #[test]
    fn starts_at_the_start_rate() {
        assert_eq!(amrr().rate(), 48);
        assert_eq!(Amrr::new([12, 24, 72].to_vec(), 48).rate(), 24); // nearest below
        assert_eq!(Amrr::new([36, 72].to_vec(), 24).rate(), 36); // none below: the lowest
    }

    #[test]
    fn steps_up_on_a_clean_link_and_stops_at_the_top() {
        let mut a = amrr();
        for want in [72, 96, 108, 108] {
            a.add(20, 0, 0);
            a.choose();
            assert_eq!(a.rate(), want);
        }
    }

    #[test]
    fn too_few_frames_change_nothing_but_add_up() {
        let mut a = amrr();
        a.add(5, 0, 0);
        assert_eq!(a.choose(), None);
        a.add(6, 0, 0); // 11 frames now
        assert_eq!(a.choose(), Some(Step::Up));
    }

    #[test]
    fn steps_down_on_retries() {
        let mut a = amrr();
        a.add(10, 0, 10); // half of the transmissions were retries
        assert_eq!(a.choose(), Some(Step::Down));
        assert_eq!(a.rate(), 36);
    }

    #[test]
    fn a_quiet_link_needs_enough_frames_to_step_down() {
        let mut a = amrr();
        a.add(1, 0, 1); // one retry in two transmissions: not judged yet
        assert_eq!(a.choose(), None);
        assert_eq!(a.rate(), 48);
        a.add(9, 0, 1); // 12 transmissions, 2 retries: fine after all
        assert_eq!(a.choose(), None);
        assert_eq!(a.rate(), 48);
    }

    #[test]
    fn a_failing_link_steps_down_within_a_few_frames() {
        let mut a = amrr();
        // Two frames, one given up after 7 retries, one after 4: every
        // retry is a transmission, so that is enough to judge.
        a.add(1, 1, 11);
        assert_eq!(a.choose(), Some(Step::Down));
        assert_eq!(a.rate(), 36);
    }

    #[test]
    fn a_failed_step_up_doubles_the_wait() {
        let mut a = amrr();
        a.add(20, 0, 0);
        assert_eq!(a.choose(), Some(Step::Up)); // 72: too fast, it turns out
        a.add(10, 0, 10);
        assert_eq!(a.choose(), Some(Step::Down)); // back to 48, threshold 2
        a.add(20, 0, 0);
        assert_eq!(a.choose(), None); // one good period is not enough now
        a.add(20, 0, 0);
        assert_eq!(a.choose(), Some(Step::Up));
        assert_eq!(a.rate(), 72);
    }

    #[test]
    fn never_below_the_lowest() {
        let mut a = Amrr::new([2].to_vec(), 2);
        a.add(0, 20, 20);
        assert_eq!(a.choose(), None);
        assert_eq!(a.rate(), 2);
    }
}
