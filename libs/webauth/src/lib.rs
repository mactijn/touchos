//! Who may change the panel's settings: a browser pairs by typing a PIN
//! that only the panel's screen shows, and then holds a session cookie.
//!
//! - PIN: 6 random digits, valid 2 minutes, one at a time, 5 tries; after
//!   that a lockout that starts at 1 minute and doubles up to 1 hour.
//! - Starting a pairing (putting a PIN on the screen) is rate limited too,
//!   so nobody can keep the panel's screen busy.
//! - Sessions: 32 random bytes (hex) in memory, at most 8 (the least
//!   recently used goes first); idle and absolute expiry. Lost on reboot.
//!
//! Pure logic: time and randomness come from the caller.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

pub const PIN_TTL_MS: u64 = 120_000;
pub const PIN_TRIES: u32 = 5;
pub const LOCKOUT_FIRST_MS: u64 = 60_000;
pub const LOCKOUT_MAX_MS: u64 = 3_600_000;
pub const START_SPACING_MS: u64 = 10_000;
pub const STARTS_PER_WINDOW: usize = 10;
pub const START_WINDOW_MS: u64 = 600_000;
pub const MAX_SESSIONS: usize = 8;
pub const SESSION_IDLE_MS: u64 = 24 * 3_600_000;

pub trait Rng {
    fn fill(&mut self, out: &mut [u8]);
}

struct Pin {
    code: [u8; 6],
    expires: u64,
    tries_left: u32,
}

struct Session {
    token: String,
    created: u64,
    last_used: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairError {
    /// No PIN is being shown (never started, expired, or used up).
    NoPin,
    /// Wrong PIN; this many tries are left for it.
    Wrong { tries_left: u32 },
    /// Too many wrong PINs: wait this many ms.
    Locked { retry_after_ms: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// Wait this many ms before showing another PIN.
    TooSoon {
        retry_after_ms: u64,
    },
    Locked {
        retry_after_ms: u64,
    },
}

pub struct Auth {
    pin: Option<Pin>,
    sessions: Vec<Session>,
    locked_until: u64,
    next_lockout_ms: u64,
    starts: VecDeque<u64>,
    /// Absolute session lifetime.
    session_max_ms: u64,
}

fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 15) as usize] as char);
    }
    s
}

/// Equal without revealing where they differ (no early exit).
fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Auth {
    pub fn new(session_hours: u32) -> Auth {
        Auth {
            pin: None,
            sessions: Vec::new(),
            locked_until: 0,
            next_lockout_ms: LOCKOUT_FIRST_MS,
            starts: VecDeque::new(),
            session_max_ms: session_hours as u64 * 3_600_000,
        }
    }

    pub fn set_session_hours(&mut self, h: u32) {
        self.session_max_ms = h as u64 * 3_600_000;
    }

    /// Make a new PIN to show on the panel (replacing an older one).
    pub fn start_pairing(&mut self, now: u64, rng: &mut impl Rng) -> Result<String, StartError> {
        if now < self.locked_until {
            return Err(StartError::Locked {
                retry_after_ms: self.locked_until - now,
            });
        }
        while self
            .starts
            .front()
            .is_some_and(|&t| now - t >= START_WINDOW_MS)
        {
            self.starts.pop_front();
        }
        if let Some(&last) = self.starts.back()
            && now - last < START_SPACING_MS
        {
            return Err(StartError::TooSoon {
                retry_after_ms: START_SPACING_MS - (now - last),
            });
        }
        if self.starts.len() >= STARTS_PER_WINDOW {
            return Err(StartError::TooSoon {
                retry_after_ms: START_WINDOW_MS - (now - self.starts[0]),
            });
        }
        self.starts.push_back(now);
        let mut code = [0u8; 6];
        for d in &mut code {
            // Rejection sampling: no bias towards low digits.
            loop {
                let mut b = [0u8; 1];
                rng.fill(&mut b);
                if b[0] < 250 {
                    *d = b'0' + b[0] % 10;
                    break;
                }
            }
        }
        self.pin = Some(Pin {
            code,
            expires: now + PIN_TTL_MS,
            tries_left: PIN_TRIES,
        });
        Ok(String::from_utf8(code.to_vec()).unwrap())
    }

    /// The PIN is still being shown (for the panel's overlay).
    pub fn pin_active(&self, now: u64) -> bool {
        self.pin.as_ref().is_some_and(|p| now < p.expires)
    }

    pub fn cancel_pairing(&mut self) {
        self.pin = None;
    }

    /// Check a typed PIN; on success the browser gets a session token.
    pub fn pair(&mut self, now: u64, typed: &str, rng: &mut impl Rng) -> Result<String, PairError> {
        if now < self.locked_until {
            return Err(PairError::Locked {
                retry_after_ms: self.locked_until - now,
            });
        }
        let Some(pin) = self.pin.as_mut().filter(|p| now < p.expires) else {
            self.pin = None;
            return Err(PairError::NoPin);
        };
        if same(pin.code.as_slice(), typed.trim().as_bytes()) {
            self.pin = None;
            self.next_lockout_ms = LOCKOUT_FIRST_MS;
            return Ok(self.new_session(now, rng));
        }
        pin.tries_left -= 1;
        if pin.tries_left == 0 {
            self.pin = None;
            self.locked_until = now + self.next_lockout_ms;
            let retry = self.next_lockout_ms;
            self.next_lockout_ms = (self.next_lockout_ms * 2).min(LOCKOUT_MAX_MS);
            return Err(PairError::Locked {
                retry_after_ms: retry,
            });
        }
        Err(PairError::Wrong {
            tries_left: pin.tries_left,
        })
    }

    fn new_session(&mut self, now: u64, rng: &mut impl Rng) -> String {
        let mut b = [0u8; 32];
        rng.fill(&mut b);
        let token = hex(&b);
        if self.sessions.len() >= MAX_SESSIONS {
            let oldest = (0..self.sessions.len())
                .min_by_key(|&i| self.sessions[i].last_used)
                .unwrap();
            self.sessions.remove(oldest);
        }
        self.sessions.push(Session {
            token: token.clone(),
            created: now,
            last_used: now,
        });
        token
    }

    /// Is this a valid session token? Refreshes its idle timer.
    pub fn check(&mut self, now: u64, token: &str) -> bool {
        let max = self.session_max_ms;
        self.sessions
            .retain(|s| now - s.last_used < SESSION_IDLE_MS && now - s.created < max);
        match self
            .sessions
            .iter_mut()
            .find(|s| same(s.token.as_bytes(), token.as_bytes()))
        {
            Some(s) => {
                s.last_used = now;
                true
            }
            None => false,
        }
    }

    pub fn logout(&mut self, token: &str) {
        self.sessions
            .retain(|s| !same(s.token.as_bytes(), token.as_bytes()));
    }

    /// Seconds a session lives at most (for the cookie's Max-Age).
    pub fn session_max_secs(&self) -> u64 {
        self.session_max_ms / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic "random" bytes.
    struct Seq(u8);
    impl Rng for Seq {
        fn fill(&mut self, out: &mut [u8]) {
            for b in out {
                self.0 = self.0.wrapping_mul(37).wrapping_add(11);
                *b = self.0;
            }
        }
    }

    #[test]
    fn pair_and_use_session() {
        let mut a = Auth::new(168);
        let mut r = Seq(1);
        let pin = a.start_pairing(1000, &mut r).unwrap();
        assert_eq!(pin.len(), 6);
        assert!(pin.bytes().all(|c| c.is_ascii_digit()));
        assert!(a.pin_active(2000));
        let token = a.pair(2000, &pin, &mut r).unwrap();
        assert_eq!(token.len(), 64);
        assert!(!a.pin_active(2000)); // used up
        assert!(a.check(3000, &token));
        assert!(!a.check(3000, "nope"));
        a.logout(&token);
        assert!(!a.check(3000, &token));
    }

    #[test]
    fn pin_expires() {
        let mut a = Auth::new(168);
        let mut r = Seq(2);
        let pin = a.start_pairing(0, &mut r).unwrap();
        assert_eq!(a.pair(PIN_TTL_MS + 1, &pin, &mut r), Err(PairError::NoPin));
    }

    #[test]
    fn wrong_pins_lock_out_and_double() {
        let mut a = Auth::new(168);
        let mut r = Seq(3);
        a.start_pairing(0, &mut r).unwrap();
        for left in (1..PIN_TRIES).rev() {
            assert_eq!(
                a.pair(10, "xxxxxx", &mut r),
                Err(PairError::Wrong { tries_left: left })
            );
        }
        assert_eq!(
            a.pair(10, "xxxxxx", &mut r),
            Err(PairError::Locked {
                retry_after_ms: LOCKOUT_FIRST_MS
            })
        );
        // Locked: no new PIN either.
        assert!(matches!(
            a.start_pairing(20, &mut r),
            Err(StartError::Locked { .. })
        ));
        // After the lockout, the next lockout is twice as long.
        let t = LOCKOUT_FIRST_MS + 100;
        a.start_pairing(t, &mut r).unwrap();
        for _ in 0..PIN_TRIES - 1 {
            let _ = a.pair(t, "xxxxxx", &mut r);
        }
        assert_eq!(
            a.pair(t, "xxxxxx", &mut r),
            Err(PairError::Locked {
                retry_after_ms: 2 * LOCKOUT_FIRST_MS
            })
        );
    }

    #[test]
    fn starts_are_rate_limited() {
        let mut a = Auth::new(168);
        let mut r = Seq(4);
        a.start_pairing(0, &mut r).unwrap();
        assert_eq!(
            a.start_pairing(4000, &mut r),
            Err(StartError::TooSoon {
                retry_after_ms: 6000
            })
        );
        let mut t = 0;
        for _ in 1..STARTS_PER_WINDOW {
            t += START_SPACING_MS;
            a.start_pairing(t, &mut r).unwrap();
        }
        t += START_SPACING_MS;
        assert!(matches!(
            a.start_pairing(t, &mut r),
            Err(StartError::TooSoon { .. })
        ));
        assert!(a.start_pairing(START_WINDOW_MS + 1, &mut r).is_ok());
    }

    #[test]
    fn sessions_expire_and_are_capped() {
        let mut a = Auth::new(1);
        let mut r = Seq(5);
        let mut tokens = Vec::new();
        for i in 0..=MAX_SESSIONS as u64 {
            let t = i * START_SPACING_MS;
            let pin = a.start_pairing(t, &mut r).unwrap();
            tokens.push(a.pair(t, &pin, &mut r).unwrap());
        }
        let now = MAX_SESSIONS as u64 * START_SPACING_MS;
        assert!(!a.check(now, &tokens[0])); // pushed out (least recently used)
        assert!(a.check(now, &tokens[MAX_SESSIONS]));
        assert!(!a.check(now + 3_600_000, &tokens[MAX_SESSIONS])); // 1 h absolute
    }

    #[test]
    fn digits_are_uniform_enough() {
        let mut counts = [0u32; 10];
        let mut r = Seq(9);
        for i in 0..2000u64 {
            let mut a = Auth::new(1);
            let pin = a.start_pairing(i, &mut r).unwrap();
            for c in pin.bytes() {
                counts[(c - b'0') as usize] += 1;
            }
        }
        assert!(counts.iter().all(|&c| c > 900 && c < 1500), "{counts:?}");
    }
}
