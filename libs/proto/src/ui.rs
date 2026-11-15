//! The dashboard's endpoint (used by the web server for pairing).

/// data: the PIN (ASCII digits), then seconds until it expires (u32 LE).
/// Shows it on the panel (waking the screen). → OK.
pub const SHOW_PIN: u32 = 1;
/// Take the PIN off the screen. → OK.
pub const HIDE_PIN: u32 = 2;

pub const OK: u32 = 0;
/// A request the dashboard doesn't know, or a malformed one.
pub const BAD_REQUEST: u32 = 1;
