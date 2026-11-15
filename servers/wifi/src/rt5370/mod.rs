//! The Toon's Wi-Fi: a Ralink RT5370 (USB `148f:5370`) on the i.MX27's
//! USB host 2, a ChipIdea EHCI that carries nothing else. This program
//! owns the controller (so the Wi-Fi password never leaves it): it
//! enumerates the module, brings it up with libs/rt2800 (a port of
//! OpenBSD's run(4)) and hands it to the station as a `Radio`
//! (touchos_rt2800::radio).
//!
//! The kernel has set up the port (pins, clocks, the ULPI PHY: see
//! `setup_usb_h2`); everything here is polled.

use ::log::{error, info, warn};
use touchos_rt2800::{Rt2800, fw::FW_SIZE};
use touchos_usb::ehci::Ehci;
use touchos_usb::{speed_name, wait_for_device};

/// The station's radio: the chip on the board's EHCI.
pub type Rt5370 = touchos_rt2800::radio::Rt5370<Ehci>;

use crate::restart_later;

/// Find the RT5370 on the board's EHCI ("regs", arguments "ehci <offset>
/// [ulpi]") and bring it up, transmitter on. None: this board has no
/// Wi-Fi set up (no "regs" or no firmware). A failing chip or USB: we
/// start over later (never returns).
pub async fn start(firmware: Option<&'static [u8]>) -> Option<Rt5370> {
    let regs = touchos_rt::cap("regs")?;
    let Some(fw) = firmware else {
        warn!("rt2800: no firmware (put rt3071.bin in build/firmware; see docs/quick-start-toon.md)");
        return None;
    };
    // The capability is whole pages; the image is 4 KiB.
    let fw = &fw[..fw.len().min(FW_SIZE)];
    let mut hc = match Ehci::from_cap(regs, touchos_rt::cmdline())? {
        Ok(hc) => hc,
        Err(e) => {
            error!("EHCI init failed: {e}");
            restart_later().await
        }
    };
    let mut dev = wait_for_device(&mut hc).await;
    let d = &dev.desc;
    info!(
        "port {}: {:04x}:{:04x} \"{}\" {}",
        dev.port,
        d.vendor,
        d.product,
        dev.product,
        speed_name(dev.speed)
    );
    if (d.vendor, d.product) != (0x148f, 0x5370) {
        warn!("not the RT5370 (148f:5370); no driver for it");
        return None;
    }
    let configured = match dev.read_config(&mut hc, 0).await {
        Ok(cfg) => dev.set_configuration(&mut hc, cfg).await,
        Err(e) => Err(e),
    };
    if let Err(e) = configured {
        error!("port {}: configuration failed: {e}", dev.port);
        restart_later().await
    }
    match Rt2800::init(&mut hc, &mut dev, fw, true).await {
        Ok(chip) => {
            let mut radio = Rt5370::new(hc, dev, chip);
            // Received frames wake the station (else it polls every 1 ms).
            if let Some(irq) = touchos_rt::irq::Irq::take("irq") {
                radio.set_irq(irq);
            }
            Some(radio)
        }
        Err(e) => {
            error!("rt2800: init failed: {e}");
            restart_later().await
        }
    }
}
