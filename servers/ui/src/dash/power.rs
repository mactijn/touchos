//! The power button: a short press switches the screen off and on, a
//! long one opens the menu (shut down, restart). A keyboard's Power key
//! works the same way.

use super::*;

/// The power button's menu: open (since when), or carrying out a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PowerMenu {
    Open { since: u64 },
    Busy(PowerAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PowerAction {
    Off,
    Restart,
}

/// Holding the power button this long opens the menu (shorter: screen
/// on/off). A power button may switch the machine off in hardware when
/// held for several seconds, so the menu must appear well before that.
pub(super) const LONG_PRESS_MS: u64 = 700;
pub(super) const MENU_TIMEOUT_MS: u64 = 30_000;

pub(super) type Rect = (i32, i32, i32, i32);

pub(super) fn inside((x, y, w, h): Rect, px: i32, py: i32) -> bool {
    px >= x && px < x + w && py >= y && py < y + h
}

/// A tap at (x, y) while the menu is up: a button, or outside = cancel.
pub(super) fn menu_tap(menu: Option<PowerMenu>, cw: i32, ch: i32, x: i32, y: i32) -> Option<PowerMenu> {
    if !matches!(menu, Some(PowerMenu::Open { .. })) {
        return menu;
    }
    let (_, [off, restart, _]) = power_menu_rects(cw, ch);
    if inside(off, x, y) {
        Some(PowerMenu::Busy(PowerAction::Off))
    } else if inside(restart, x, y) {
        Some(PowerMenu::Busy(PowerAction::Restart))
    } else {
        None
    }
}

/// The menu card and its buttons: shut down, restart, cancel.
pub(super) fn power_menu_rects(cw: i32, ch: i32) -> (Rect, [Rect; 3]) {
    let w = (cw - 2 * PAD).min(s(960));
    let h = s(340);
    let (x, y) = ((cw - w) / 2, (ch - h) / 2);
    let bw = (w - 4 * PAD) / 3;
    let (by, bh) = (y + s(130), s(170));
    let b = |i: i32| (x + PAD + i * (bw + PAD), by, bw, bh);
    ((x, y, w, h), [b(0), b(1), b(2)])
}

pub(super) fn draw_power_menu(cv: &mut Canvas, m: PowerMenu) {
    let ((x, y, w, h), buttons) = power_menu_rects(cv.w, cv.h);
    cv.fill_round_rect(x - s(4), y - s(4), w + s(8), h + s(8), RADIUS + s(4), TEXT_DIM);
    cv.fill_round_rect(x, y, w, h, RADIUS, TILE_OFF);
    let center =
        |cv: &mut Canvas, face: &gfx::Face, (bx, _, bw, _): Rect, yy: i32, t: &str, c: Color| {
            let tw = face.width(t).min(bw);
            cv.text_fit(face, bx + (bw - tw) / 2, yy, t, bw, c);
        };
    match m {
        PowerMenu::Open { .. } => {
            center(cv, &f::TEXT_BOLD, (x, y, w, h), y + s(80), "Power", TEXT);
            let labels = [
                ("Shut down", ACCENT_WARN, TEXT_ON_DARK),
                ("Restart", BUTTON, TEXT),
                ("Cancel", BUTTON, TEXT),
            ];
            for (r, (label, bg, fg)) in buttons.into_iter().zip(labels) {
                cv.fill_round_rect(r.0, r.1, r.2, r.3, RADIUS, bg);
                center(cv, &f::TEXT_BOLD, r, r.1 + r.3 / 2 + s(14), label, fg);
            }
        }
        PowerMenu::Busy(a) => {
            let t = match a {
                PowerAction::Off => "Switching off\u{2026}",
                PowerAction::Restart => "Restarting\u{2026}",
            };
            center(cv, &f::TEXT_BOLD, (x, y, w, h), y + h / 2 + s(14), t, TEXT);
        }
    }
}

/// Switch off or restart (through the system capability init gave us).
pub(super) fn power(a: PowerAction) {
    let Some(system) = touchos_rt::cap("system") else {
        ::log::error!("power: no system capability");
        return;
    };
    info!("power: {a:?}");
    let r = match a {
        PowerAction::Off => touchos_rt::sys::poweroff(system),
        PowerAction::Restart => touchos_rt::sys::reboot(system),
    };
    ::log::error!("power: {a:?} failed: {r:?}");
}
