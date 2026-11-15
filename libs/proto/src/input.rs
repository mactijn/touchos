//! Input events (USB HID, touchscreen, accelerometer) for the dashboard.

use serde::{Deserialize, Serialize};

/// The input rings' geometry (USB HID and the platform drivers to the
/// dashboard): init sizes the memory with these, the producer formats it.
pub const RING_SLOTS: u32 = 256;
pub const RING_SLOT: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// HID keyboard usage code (page 7) going down/up, with modifier bits.
    Key {
        usage: u8,
        down: bool,
        mods: u8,
    },
    /// Relative pointer motion (mouse / touchpad in boot mode).
    PointerRel {
        dx: i16,
        dy: i16,
        buttons: u8,
    },
    /// Touchscreen (first finger), in panel pixels: down, moves, up.
    TouchDown {
        x: i32,
        y: i32,
    },
    TouchMove {
        x: i32,
        y: i32,
    },
    TouchUp {
        x: i32,
        y: i32,
    },
    /// Screen orientation from the accelerometer, in quarter turns.
    Rotation(u8),
    /// The power button going down/up.
    PowerButton {
        down: bool,
    },
}
