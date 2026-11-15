//! The screen's brightness: in use, dimmed after a while, off at night
//! or when switched off with the power button.
//!
//! Changes fade over a fixed time (not a fixed step per loop pass), along a
//! perceptual curve: the square root of the duty cycle moves linearly, so
//! the steps look even from bright down to dark.

use super::*;

/// Brightness levels are in 0.01 % of the PWM period (0..=FULL).
const FULL: u32 = 10_000;
/// Getting brighter (a touch): quick.
const WAKE_MS: u64 = 150;
/// Getting darker (idle, night): a slow, visible fade.
const DIM_MS: u64 = 1500;

/// A fade in progress: from one level to another over `dur` ms.
#[derive(Debug, Clone, Copy)]
struct Fade {
    from: u32,
    to: u32,
    start: u64,
    dur: u64,
}

pub(super) struct Screen {
    bl: Option<Backlight>,
    /// What the backlight is set to now (u32::MAX: not set yet).
    level: u32,
    bright: u32,
    dim: u32,
    dim_after_ms: u64,
    night: Option<(u32, u32)>,
    last_activity: u64,
    /// Switched off with the power button (until the next touch or press).
    off: bool,
    fade: Option<Fade>,
}

impl Screen {
    pub(super) fn new() -> Screen {
        Screen::with_backlight(Backlight::init(), u32::MAX)
    }

    /// New settings, same backlight (and current level, for a smooth change).
    pub(super) fn new_from(old: Screen) -> Screen {
        Screen::with_backlight(old.bl, old.level)
    }

    pub(super) fn with_backlight(bl: Option<Backlight>, level: u32) -> Screen {
        let d = &config::get().display;
        let night = d.night.map(|(a, b)| (a as u32, b as u32));
        Screen {
            bl,
            level,
            bright: (d.brightness as u32 * 100).min(FULL),
            dim: (d.dim_brightness as u32 * 100).min(FULL),
            dim_after_ms: d.dim_after_seconds as u64 * 1000,
            night,
            last_activity: time::now_ms(),
            off: false,
            fade: None,
        }
    }

    /// Is the screen (nearly) dark: off, or at night?
    pub(super) fn dark(&self) -> bool {
        self.off || self.level < 200
    }

    /// Is a fade running (the main loop should come back soon)?
    pub(super) fn fading(&self) -> bool {
        self.fade.is_some()
    }

    /// How long until the screen counts as idle (0: it already does).
    pub(super) fn ms_until_idle(&self) -> u64 {
        (self.last_activity + self.dim_after_ms).saturating_sub(time::now_ms())
    }

    /// The power button's short press: off now (no fade).
    pub(super) fn switch_off(&mut self) {
        self.off = true;
    }

    /// Register user activity. Returns true if the screen was dark, i.e. this
    /// touch should only wake the screen and not act on what's underneath.
    pub(super) fn activity(&mut self) -> bool {
        self.last_activity = time::now_ms();
        let was_dark = self.dark();
        self.off = false;
        was_dark
    }

    pub(super) fn update(&mut self, minute_of_day: Option<u32>) {
        let now = time::now_ms();
        let idle = now.saturating_sub(self.last_activity) >= self.dim_after_ms;
        let at_night = match (self.night, minute_of_day) {
            (Some((start, end)), Some(m)) if start > end => m >= start || m < end,
            (Some((start, end)), Some(m)) => m >= start && m < end,
            _ => false,
        };
        let target = if self.off {
            0
        } else if !idle {
            self.bright
        } else if at_night {
            0
        } else {
            self.dim
        };
        // The first setting, and the power button: at once.
        if self.level == u32::MAX || self.off {
            self.fade = None;
            self.set(target);
            return;
        }
        let heading_for = self.fade.map_or(self.level, |f| f.to);
        if target != heading_for {
            let dur = if target > self.level { WAKE_MS } else { DIM_MS };
            ::log::info!(
                "backlight: {}.{:02} % -> {}.{:02} % in {dur} ms",
                self.level / 100,
                self.level % 100,
                target / 100,
                target % 100
            );
            self.fade = Some(Fade {
                from: self.level,
                to: target,
                start: now,
                dur,
            });
        }
        if let Some(f) = self.fade {
            let t = now.saturating_sub(f.start);
            if t >= f.dur {
                self.fade = None;
                ::log::info!("backlight: faded in {t} ms");
                self.set(f.to);
            } else {
                self.set(curve(f.from, f.to, t, f.dur));
            }
        }
    }

    fn set(&mut self, level: u32) {
        if level != self.level {
            self.level = level;
            if let Some(bl) = &self.bl {
                bl.set_permyriad(level);
            }
        }
    }
}

/// The level `t` ms into a fade of `dur` ms from `from` to `to`: the square
/// root of the level moves linearly (gamma 2), so it looks even.
fn curve(from: u32, to: u32, t: u64, dur: u64) -> u32 {
    let (from, to) = (from.min(FULL), to.min(FULL));
    // sqrt(level * FULL): 0..=FULL.
    let a = isqrt(from * FULL) as i32;
    let b = isqrt(to * FULL) as i32;
    let t = t.min(dur) as i32;
    let s = a + (b - a) * t / (dur.max(1) as i32);
    (s as u32 * s as u32) / FULL
}

fn isqrt(n: u32) -> u32 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}
