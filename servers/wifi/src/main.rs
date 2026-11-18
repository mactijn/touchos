//! The Wi-Fi station in user space: find the chip, bring it up with its
//! driver, and run the chip-independent station (touchos_wlan: scan, join,
//! WPA2-PSK, CCMP, the data pump to `net`) on it. The chip is the Toon's
//! Ralink RT5370 on the board's EHCI (USB host 2), driven by `rt5370` with
//! libs/rt2800 (a port of OpenBSD's run(4)).
//!
//! Capabilities: "firmware" (rt3071.bin), "config" (FULL: the Wi-Fi
//! password), "link_rx"/"link_tx"/"link_state", "regs" (the USB block's
//! page, with the arguments "ehci <controller offset> [ulpi]").
//! When the Wi-Fi settings change, the program exits with exit::RESTART
//! and init starts it again at once. When the radio fails (bring-up, or a
//! dead chip later), it exits after a pause and init starts it again.

#![no_std]
#![no_main]

extern crate alloc;

mod rt5370;

use spin::Once;
use touchos_wlan::station;

pub use touchos_drv::{link, mem};
pub use touchos_rt::{exec, time};

touchos_rt::entry!(main);

/// The Wi-Fi settings.
pub mod config {
    pub use touchos_config::client::settings::get;
}

/// How long a settings change waits for the goodbye to the access point.
const LEAVE_MS: u64 = 500;

/// Exit (to be restarted) when the Wi-Fi settings change.
async fn watch_settings() {
    loop {
        let (old, new) = touchos_config::client::settings::changed().await;
        if new.wifi != old.wifi {
            log::info!("settings changed; restarting");
            // Say goodbye first: an access point that still counts us as
            // associated may ignore the new process's attempts.
            station::leave(LEAVE_MS).await;
            link::down();
            touchos_rt::sys::exit(touchos_abi::exit::RESTART);
        }
    }
}

/// The chip's firmware, from the "firmware" capability.
static FIRMWARE: Once<&'static [u8]> = Once::new();

/// Find a supported chip, bring it up, and be a station on it.
async fn task() {
    station::log_self_test();
    let fw = FIRMWARE.get().copied();
    let radio = rt5370::start(fw).await;
    let Some(mut radio) = radio else {
        return; // no chip, or it failed (logged)
    };
    let cfg = &config::get().wifi;
    if let Err(e) = station::serve(&mut radio, cfg.ssid.as_deref(), cfg.psk.as_deref(), cfg.ht).await {
        log::error!("station stopped: {e}");
        // Start over with a fresh process: the chip is reset and brought
        // up again (on the Toon also the USB port).
        restart_later().await;
    }
}

/// After a failure: wait this long, then exit for init to start us
/// again. Init gives up on a program that dies 5 times within a minute,
/// and for wifi it then restarts the machine; this keeps us well below
/// that.
const RESTART_DELAY_MS: u64 = 30_000;

/// Wait, then exit (a failure) to be started again (see [`RESTART_DELAY_MS`]).
pub async fn restart_later() -> ! {
    log::warn!("starting over in {} s", RESTART_DELAY_MS / 1000);
    exec::sleep_ms(RESTART_DELAY_MS).await;
    link::down();
    touchos_rt::sys::exit(4)
}

fn main() {
    if let Some(fw) =
        touchos_rt::cap("firmware").and_then(|c| touchos_rt::caps::Mapping::map(c, false).ok())
    {
        let bytes: &'static [u8] =
            unsafe { core::slice::from_raw_parts(fw.va as *const u8, fw.len) };
        core::mem::forget(fw);
        FIRMWARE.call_once(|| bytes);
    }
    exec::spawn(watch_settings());
    exec::spawn(task());
    exec::run()
}
