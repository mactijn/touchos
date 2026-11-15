//! The storage server: the Toon's NAND (libs/nand). The config server
//! may change touchos.toml there (the previous one is kept as the backup),
//! and the update server the copy of the system that isn't running; see
//! `nand`. U-Boot's blocks and the boot stage are never written.
//!
//! Arguments (from init, out of the kernel command line):
//! - `mode=off|probe|ro|rw` (default `rw`). `ro` reads, and a write is a
//!   dry run (logged); `rw` writes. (`probe`, like `ro`, writes nothing.)
//! - `path=touchos.toml`: the config file's name.
//! - `slot=A|B`: the copy of the system that is running; the update server
//!   may only write the other one.
//!
//! Capabilities: "ep"; "regs" (the NAND controller's page).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use log::{info, warn};
use touchos_rt::{cap, time};
use touchos_storage::{Access, Mode};

mod nand;

touchos_rt::entry!(main);

const MAX_FILE: usize = 64 * 1024;

struct Args {
    mode: Mode,
    /// Who may touch which file (the config's path, the running copy).
    access: Access,
}

fn args() -> Args {
    let get = touchos_rt::arg;
    let running = match get("slot") {
        Some("A") => Some('A'),
        Some("B") => Some('B'),
        _ => None,
    };
    Args {
        mode: Mode::parse(get("mode").unwrap_or("rw")),
        access: Access::new(get("path").unwrap_or("touchos.toml"), running),
    }
}

fn now_us() -> u64 {
    time::now_us()
}

/// Capabilities we sent in a reply (dropped at the next request, once the
/// reply is out).
static SENT: spin::Mutex<Vec<u32>> = spin::Mutex::new(Vec::new());

fn main() {
    let a = args();
    if a.mode == Mode::Off {
        info!("storage=off");
        return;
    }
    let Some(ep) = cap("ep") else { return };
    let Some(regs) = cap("regs").and_then(|r| touchos_drv::mmio::Regs::from_cap(r, 0, 0x1000).ok()) else {
        warn!("no NAND controller");
        return;
    };
    nand::serve(ep, regs, a.mode, a.access);
}
