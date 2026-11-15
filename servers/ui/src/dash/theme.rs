//! The dashboard's look: colours, and lengths scaled for this build's
//! panel.

use super::*;

pub(super) const BG: Color = Color::rgb(0x10, 0x13, 0x17);
pub(super) const TILE_OFF: Color = Color::rgb(0x23, 0x27, 0x2e);
pub(super) const TILE_UNAVAILABLE: Color = Color::rgb(0x1a, 0x1d, 0x22);
pub(super) const TILE_LIGHT_ON: Color = Color::rgb(0xf5, 0xb8, 0x3d);
pub(super) const TILE_ON: Color = Color::rgb(0x3e, 0xa6, 0xa0);
pub(super) const TILE_PRESSED: Color = Color::rgb(0x45, 0x4b, 0x55);
pub(super) const BAR_TRACK: Color = Color::rgb(0xd4, 0x96, 0x24);
pub(super) const BAR_FILL: Color = Color::rgb(0x1a, 0x1a, 0x1a);
pub(super) const BUTTON: Color = Color::rgb(0x34, 0x3a, 0x44);
pub(super) const TEXT: Color = Color::rgb(0xec, 0xee, 0xf1);
pub(super) const TEXT_DIM: Color = Color::rgb(0x8b, 0x93, 0x9e);
pub(super) const TEXT_ON_DARK: Color = Color::rgb(0x1a, 0x1a, 0x1a);
pub(super) const ACCENT_WARN: Color = Color::rgb(0xe0, 0x7a, 0x5f);
pub(super) const DOT_ON: Color = Color::rgb(0xec, 0xee, 0xf1);
pub(super) const DOT_OFF: Color = Color::rgb(0x45, 0x4b, 0x55);

/// A length in design pixels (a 1920x1200 10.1" panel), scaled for this
/// build's panel (the fonts are scaled the same way; see build.rs).
pub(super) const fn s(v: i32) -> i32 {
    v * f::SCALE.0 / f::SCALE.1
}

pub(super) const PAD: i32 = s(24);
pub(super) const TOP: i32 = s(190);
pub(super) const TILE_H: i32 = s(220);
pub(super) const RADIUS: i32 = s(28);
pub(super) const DOTS_H: i32 = s(40);
/// Inner margin of a tile.
pub(super) const M: i32 = s(28);
/// Movement (px) before a touch counts as a drag instead of a tap.
pub(super) const DRAG_THRESHOLD: i32 = s(28);

pub(super) const CHIP_H: i32 = s(60);
pub(super) const CHIPS_Y: i32 = s(172);
