//! The screen's brightness: in use, dimmed after a while, off at night
//! or when switched off with the power button.

use super::*;

pub(super) struct Screen {
    bl: Option<Backlight>,
    level: u32,
    bright: u32,
    dim: u32,
    dim_after_ms: u64,
    night: Option<(u32, u32)>,
    last_activity: u64,
    /// Switched off with the power button (until the next touch or press).
    off: bool,
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
            bright: d.brightness as u32 * 10,
            dim: d.dim_brightness as u32 * 10,
            dim_after_ms: d.dim_after_seconds as u64 * 1000,
            night,
            last_activity: time::now_ms(),
            off: false,
        }
    }

    /// Is the screen (nearly) dark: off, or at night?
    pub(super) fn dark(&self) -> bool {
        self.off || self.level < 20
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
        let idle = time::now_ms() - self.last_activity >= self.dim_after_ms;
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
        let new = if self.level == u32::MAX || self.off {
            target
        } else if target > self.level {
            (self.level + 100).min(target) // wake up fast
        } else {
            self.level.saturating_sub(8).max(target) // fade out slowly
        };
        if new != self.level {
            self.level = new;
            if let Some(bl) = &self.bl {
                bl.set_permille(new);
            }
        }
    }
}
