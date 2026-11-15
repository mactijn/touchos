//! The pairing PIN card, while the settings page waits for it.

use super::*;

/// The pairing card: the PIN in large digits over the dashboard.
pub(super) fn draw_pin(cv: &mut Canvas, pin: &str, secs: u64) {
    let w = (cv.w - 2 * PAD).min(s(960));
    let h = s(440);
    let (x, y) = ((cv.w - w) / 2, (cv.h - h) / 2);
    cv.fill_round_rect(x - s(4), y - s(4), w + s(8), h + s(8), RADIUS + s(4), TEXT_DIM);
    cv.fill_round_rect(x, y, w, h, RADIUS, TILE_OFF);
    let center = |cv: &mut Canvas, face: &gfx::Face, yy: i32, t: &str, c: Color| {
        let tw = face.width(t);
        cv.text(face, x + (w - tw) / 2, yy, t, c);
    };
    center(cv, &f::TEXT_BOLD, y + s(80), "Pair a browser", TEXT);
    let spaced: String = pin.chars().flat_map(|c| [c, ' ']).collect();
    center(cv, &f::CLOCK, y + s(250), spaced.trim_end(), TILE_LIGHT_ON);
    center(
        cv,
        &f::TEXT,
        y + s(330),
        "Type this code on the settings page",
        TEXT,
    );
    let foot = format!("Tap to hide \u{2013} expires in {secs} s");
    center(cv, &f::TEXT_SMALL, y + s(395), &foot, TEXT_DIM);
}
