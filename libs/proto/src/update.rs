//! The update server: installs a signed bundle (see libs/update) into the
//! copy of the system that isn't running, then restarts into it (on trial).

use alloc::string::String;
use serde::{Deserialize, Serialize};

/// data: bundle length (u32); caps[0]: read-only memory with the bundle.
/// Checked (signature, every file) before the reply. → OK (data: the
/// build) | INVALID (data: why) | BUSY.
pub const INSTALL: u32 = 1;

pub const OK: u32 = 0;
pub const INVALID: u32 = 1;
pub const BUSY: u32 = 2;

/// Bytes of the board with the `State` (update writes; web and the
/// dashboard read; init makes it).
pub const STATE_BYTES: u64 = 4096;

/// Published on a board the web server reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The running copy ('A'/'B') and its build.
    pub running: Option<char>,
    pub build: String,
    /// The other copy's VERSION.TXT.
    pub other: Option<String>,
    /// Whether this build accepts updates (it has a public key built in).
    pub enabled: bool,
    pub phase: Phase,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    #[default]
    Idle,
    /// Writing part `done + 1` of `total` (`name`).
    Writing {
        name: String,
        done: u32,
        total: u32,
    },
    /// Everything written; restarting into the new copy (`build`).
    Restarting {
        build: String,
    },
    Failed(String),
}
