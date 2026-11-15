//! Platform drivers: init names the board (`board=` on our command line)
//! and hands us its controllers.
//!
//! On the Toon (`toon`, and QEMU's `imx25` as its test bench): the i.MX
//! I²C controllers and the Toon's devices on them. Capabilities: "i2c1",
//! "i2c2", "system" (to set the clock from the RTC).
//!
//! Touches, orientation changes and button presses go to the dashboard
//! through a ring in shared memory: "input" (we are its producer).

#![no_std]
#![no_main]

extern crate alloc;

mod toon;

use log::warn;
use touchos_rt::exec;

touchos_rt::entry!(main);

/// The input ring to the dashboard.
pub use touchos_drv::input;

fn main() {
    let board = touchos_rt::arg("board").unwrap_or("");
    match board {
        "toon" | "imx25" => toon::start(),
        _ => {
            warn!("no drivers for board \"{board}\"");
            return;
        }
    }
    exec::run()
}
