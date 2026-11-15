//! The config server's IPC protocol (labels and payloads).
//!
//! Capabilities to the config endpoint carry a badge that decides what the
//! holder may do ([`badge`]).

/// Request labels.
pub mod req {
    /// → `REPLY_OK`, data: version (u64 LE); caps[0]: read-only memory with
    /// the file text (`CAP_INFO` gives its length). Readers with the
    /// `PUBLIC` badge get the text without secrets.
    pub const GET: u32 = 1;
    /// caps[0]: a notification; data: bits (u64 LE). Signalled on every
    /// change. → `REPLY_OK`.
    pub const WATCH: u32 = 2;
    /// data: base version (u64 LE) + patch text (see `patch`). Needs the
    /// `ADMIN` badge. → `REPLY_OK` (data: new version u64, effects u32,
    /// saved u8: 1 = saved, 0 = not (read-only), 2 = saving failed) |
    /// `REPLY_CONFLICT` (data: current version) | `REPLY_INVALID` (data:
    /// "field: message" lines) | `REPLY_DENIED`.
    pub const UPDATE: u32 = 3;
    /// → `REPLY_OK`, data: version u64, source u32, then problems as
    /// "field: message" lines.
    pub const STATUS: u32 = 4;
}

pub const REPLY_OK: u32 = 0;
pub const REPLY_CONFLICT: u32 = 1;
pub const REPLY_INVALID: u32 = 2;
pub const REPLY_DENIED: u32 = 3;
pub const REPLY_BAD_REQUEST: u32 = 4;

/// Badges of config endpoint capabilities (minted by init).
pub mod badge {
    /// Read settings without secrets (the UI, the web server).
    pub const PUBLIC: usize = 1;
    /// Read everything (Home Assistant client, Wi-Fi).
    pub const FULL: usize = 2;
    /// Change settings; reads like `PUBLIC` (the web server).
    pub const ADMIN: usize = 3;
}

/// Where the settings came from.
pub mod source {
    pub const BOOT_FILE: u32 = 0;
    pub const BACKUP: u32 = 1;
    pub const DEFAULTS: u32 = 2;
}
