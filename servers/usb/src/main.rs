//! The USB stack in user space: a host controller from libs/usb (polled,
//! DMA via contiguous memory objects), hot-plug, HID keyboards/mice for
//! the dashboard, CDC-ECM Ethernet adapters for the net server, and USB
//! disks (mass storage, read-only: `msc`). The controller is the ChipIdea
//! EHCI: on the Toon its external port's (the OTG controller); on QEMU
//! imx25, where this program only enumerates what is plugged in. The EHCI
//! has no periodic schedule yet, so no HID; bulk devices (disks) work.
//!
//! Capabilities: "regs" (the USB block's page, with the arguments
//! "ehci <controller offset> [ulpi] [vbus]"), "irq", "input" (ring to the
//! dashboard), "link_rx"/"link_tx"/"link_state" (the Ethernet link). The
//! Toon's host 2 (in the same block) belongs to the Wi-Fi server.

#![no_std]
#![no_main]

extern crate alloc;

mod ecm;
mod hid;
mod msc;

pub use touchos_drv::{input, link, mem};
pub use touchos_rt::{exec, time};

use ::log::{error, info, warn};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use ecm::Ecm;
use hid::Hid;
use msc::Msc;
use touchos_usb::{Device, HostController, desc, speed_name};

touchos_rt::entry!(main);

fn main() {
    exec::spawn(task());
    exec::run()
}

/// Find the controller and run the stack on it.
async fn task() {
    // A ChipIdea EHCI in the USB block whose page the "regs" capability
    // maps. Arguments: "ehci <controller offset> [ulpi]".
    let Some(regs) = touchos_rt::cap("regs") else {
        warn!("no USB controller");
        return;
    };
    match touchos_usb::ehci::Ehci::from_cap(regs, touchos_rt::cmdline()) {
        Some(Ok(hc)) => run(hc, touchos_rt::irq::Irq::take("irq")).await,
        Some(Err(e)) => warn!("EHCI init failed: {e}"),
        None => {} // logged
    }
}

/// Watch the root ports and drive the devices on them, forever.
async fn run<H: HostController>(mut hc: H, irq: Option<touchos_rt::irq::Irq>) {
    // Give link training and port power a moment, then look at every port.
    exec::sleep_ms(200).await;
    let mut ports: Vec<Option<Device<H>>> = (0..=hc.ports()).map(|_| None).collect();
    let mut drv = Drivers::default();
    let mut changes: Vec<u8> = (1..=hc.ports()).collect();
    let mut recovery = Recovery::default();
    // `test_fail=usb`: the first driver fails once, 40 s after it started.
    let mut test_fail = touchos_rt::arg("test_fail").filter(|t| *t == "usb").map(|_| time::now_ms() + 40_000);
    if test_fail.is_some() {
        warn!("test_fail=usb: the first driver fails on purpose in 40 s");
    }
    // The controller's interrupt line: ports
    // and transfers are watched through it instead of every 4 or 50 ms.
    let irq = IRQ.with(|i| {
        *i = irq;
        i.as_ref().map(|i| (i.bit(), ()))
    });
    if irq.is_some() {
        hc.enable_interrupts();
    }
    // Frames the net server queues for a USB Ethernet adapter.
    let tx_bit = exec::alloc_bit();
    touchos_drv::link::wait_tx(tx_bit);

    loop {
        // Status first: a change after this look interrupts again.
        hc.clear_interrupts();
        for p in hc.poll() {
            if !changes.contains(&p) {
                changes.push(p);
            }
        }
        for port in core::mem::take(&mut changes) {
            let st = hc.port_status(port);
            hc.ack_port(port);
            let known = ports[port as usize].is_some();
            if known && (!st.connected || !st.enabled) {
                detach(&mut hc, port, &mut ports, &mut drv).await;
            }
            if !st.connected {
                // Unplugged: whatever comes next starts with a clean slate.
                recovery.forget(port);
            }
            // (A device being recovered waits for its turn: detaching it
            // changes the port's status too.)
            if st.connected && ports[port as usize].is_none() && !recovery.holds(port) {
                attach(&mut hc, port, &mut ports, &mut drv).await;
            }
        }
        for h in drv.hids.iter_mut() {
            if let Some(dev) = ports[h.port as usize].as_mut() {
                h.poll(&mut hc, dev);
            }
        }
        for e in drv.ecms.iter_mut() {
            if let Some(dev) = ports[e.port as usize].as_mut() {
                e.poll(&mut hc, dev);
            }
        }
        for m in drv.mscs.iter_mut() {
            if let Some(dev) = ports[m.port as usize].as_mut() {
                m.check(&mut hc, dev).await;
            }
        }
        // A driver that failed (a transaction error: a loose contact, a
        // device that reset itself): detach the device and enumerate it
        // again a little later.
        drv.test_fail(&mut test_fail);
        for port in drv.failed_ports() {
            let wait = recovery.failed(port, time::now_ms());
            detach(&mut hc, port, &mut ports, &mut drv).await;
            match wait {
                Some(ms) => warn!("port {port}: a driver failed; enumerating the device again in {} s (try {})", ms / 1000, recovery.tries(port)),
                None => error!(
                    "port {port}: a driver failed {} times in a row; giving up on it until it is plugged in again",
                    MAX_TRIES
                ),
            }
        }
        for port in recovery.due(time::now_ms()) {
            if ports[port as usize].is_none() && hc.port_status(port).connected {
                info!("port {port}: enumerating the device again");
                attach(&mut hc, port, &mut ports, &mut drv).await;
                recovery.attached(port, time::now_ms());
            }
        }
        // Keyboards, mice and Ethernet want their reports often; with none
        // of them, only hot-plug is watched (fewer wake-ups on the Toon).
        let busy = !drv.hids.is_empty() || !drv.ecms.is_empty();
        match irq {
            // Completed transfers (keyboards, mice, Ethernet) and port
            // changes interrupt; frames to send signal tx_bit. (A mass
            // storage device's check keeps its own time, in minutes; a
            // retry after a failure in seconds.)
            Some((bit, ())) => {
                IRQ.with(|i| i.as_ref().map(|i| i.ack()));
                exec::wait_bits_or_ms(bit | tx_bit, IDLE_WAIT_MS).await;
            }
            None => exec::sleep_ms(if busy { 4 } else { 50 }).await,
        }
    }
}

/// The controller's interrupt, where the kernel hands one out.
static IRQ: exec::Local<Option<touchos_rt::irq::Irq>> = exec::Local::new(None);

/// A transfer isn't done yet: wait for the controller's next interrupt
/// (at most a tick), or without one just let the others run. Call
/// `hc.clear_interrupts()` before looking at the transfer, so one that
/// completes after the look interrupts again.
pub async fn wait_for_controller() {
    match IRQ.with(|i| i.as_ref().map(|i| { i.ack(); i.bit() })) {
        Some(bit) => {
            exec::wait_bits_or_ms(bit, 4).await;
        }
        None => exec::yield_now().await,
    }
}

/// With the controller's interrupt and nothing to poll: the longest sleep.
const IDLE_WAIT_MS: u64 = 1000;

#[derive(Default)]
struct Drivers {
    hids: Vec<Hid>,
    ecms: Vec<Ecm>,
    mscs: Vec<Msc>,
}

impl Drivers {
    /// Ports with a driver that failed (each once).
    fn failed_ports(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self
            .hids
            .iter()
            .filter(|h| h.failed)
            .map(|h| h.port)
            .chain(self.ecms.iter().filter(|e| e.failed).map(|e| e.port))
            .chain(self.mscs.iter().filter(|m| m.failed).map(|m| m.port))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// `test_fail=usb`: when it is time, the first driver fails.
    fn test_fail(&mut self, at: &mut Option<u64>) {
        if at.is_none_or(|t| time::now_ms() < t) {
            return;
        }
        let port = if let Some(h) = self.hids.first_mut() {
            h.failed = true;
            h.port
        } else if let Some(m) = self.mscs.first_mut() {
            m.failed = true;
            m.port
        } else if let Some(e) = self.ecms.first_mut() {
            e.failed = true;
            e.port
        } else {
            return; // nothing attached yet
        };
        warn!("test_fail=usb: failing the driver on port {port} on purpose");
        *at = None;
    }
}

/// Tries before giving up on a port, and the waits before each.
const MAX_TRIES: u32 = 5;
const RETRY_MS: [u64; MAX_TRIES as usize] = [1000, 2000, 5000, 10_000, 30_000];
/// A device that ran this long since it was enumerated again is fine:
/// its next failure counts from 1.
const GOOD_RUN_MS: u64 = 60_000;

/// Devices whose driver failed: when to enumerate them again, and how
/// often that went wrong in a row.
#[derive(Default)]
struct Recovery {
    /// (port, tries in a row, enumerate again at, last enumerated at)
    ports: Vec<(u8, u32, Option<u64>, u64)>,
}

impl Recovery {
    fn entry(&mut self, port: u8) -> &mut (u8, u32, Option<u64>, u64) {
        if let Some(i) = self.ports.iter().position(|p| p.0 == port) {
            return &mut self.ports[i];
        }
        self.ports.push((port, 0, None, 0));
        self.ports.last_mut().unwrap()
    }

    /// A driver on `port` failed: how long to wait before enumerating it
    /// again, or None to give up.
    fn failed(&mut self, port: u8, now: u64) -> Option<u64> {
        let e = self.entry(port);
        if e.1 > 0 && now.saturating_sub(e.3) > GOOD_RUN_MS {
            e.1 = 0;
        }
        e.1 += 1;
        if e.1 > MAX_TRIES {
            e.2 = None;
            return None;
        }
        let wait = RETRY_MS[e.1 as usize - 1];
        e.2 = Some(now + wait);
        Some(wait)
    }

    fn tries(&self, port: u8) -> u32 {
        self.ports.iter().find(|p| p.0 == port).map_or(0, |p| p.1)
    }

    /// Ports due to be enumerated again (each once).
    fn due(&mut self, now: u64) -> Vec<u8> {
        let mut v = Vec::new();
        for p in self.ports.iter_mut() {
            if p.2.is_some_and(|t| t <= now) {
                p.2 = None;
                v.push(p.0);
            }
        }
        v
    }

    /// Waiting to be enumerated again, or given up on.
    fn holds(&self, port: u8) -> bool {
        self.ports.iter().any(|p| p.0 == port && (p.2.is_some() || p.1 > MAX_TRIES))
    }

    fn attached(&mut self, port: u8, now: u64) {
        self.entry(port).3 = now;
    }

    fn forget(&mut self, port: u8) {
        self.ports.retain(|p| p.0 != port);
    }
}

/// Pick a configuration: the first one with a driver we have (ECM), else the first.
async fn choose_config<H: HostController>(hc: &mut H, dev: &mut Device<H>) -> touchos_usb::Result<desc::Config> {
    let first = dev.read_config(hc, 0).await?;
    for i in 1..dev.desc.num_configs.min(4) {
        if ecm::is_ecm(&first) {
            break;
        }
        if let Ok(cfg) = dev.read_config(hc, i).await
            && ecm::is_ecm(&cfg)
        {
            info!("port {}: using configuration {} (CDC-ECM)", dev.port, i + 1);
            return Ok(cfg);
        }
    }
    Ok(first)
}

async fn attach<H: HostController>(hc: &mut H, port: u8, ports: &mut [Option<Device<H>>], drv: &mut Drivers) {
    let Some(mut dev) = Device::attach(hc, port).await else { return };
    // A failed attempt can leave port-change events; they're handled below as no-ops.
    let d = &dev.desc;
    info!(
        "port {port}: {:04x}:{:04x} \"{}\" {} USB {:x}.{:02x} class {:02x}",
        d.vendor,
        d.product,
        dev.product,
        speed_name(dev.speed),
        d.usb_version >> 8,
        d.usb_version & 0xff,
        d.class
    );
    let chosen = match choose_config(hc, &mut dev).await {
        Ok(c) => c,
        Err(e) => {
            warn!("port {port}: reading configuration failed: {e}");
            ports[port as usize] = Some(dev);
            return;
        }
    };
    if let Err(e) = dev.set_configuration(hc, chosen).await {
        warn!("port {port}: set configuration failed: {e}");
        ports[port as usize] = Some(dev);
        return;
    }
    let cfg = dev.config.clone().unwrap();
    for iface in &cfg.interfaces {
        let eps: Vec<String> = iface
            .endpoints
            .iter()
            .map(|e| {
                let t = ["ctrl", "iso", "bulk", "intr"][e.transfer_type() as usize];
                format!("{:02x} {t} {}B", e.address, e.max_packet & 0x7ff)
            })
            .collect();
        ::log::debug!(
            "  if{}.{} {:02x}:{:02x}:{:02x} {} [{}]",
            iface.number,
            iface.alt,
            iface.class,
            iface.subclass,
            iface.protocol,
            desc::class_name(iface.class),
            eps.join(", ")
        );
    }
    for iface in cfg.interfaces.iter().filter(|i| i.alt == 0) {
        if iface.class == 0x02 && iface.subclass == 0x06 {
            match Ecm::attach(hc, &mut dev).await {
                Ok(e) => drv.ecms.push(e),
                Err(e) => warn!("port {port}: ethernet setup failed: {e}"),
            }
        }
        if Msc::is_msc(iface) {
            match Msc::attach(hc, &mut dev, iface).await {
                Ok(m) => drv.mscs.push(m),
                Err(e) => warn!("port {port} if{}: mass storage setup failed: {e}", iface.number),
            }
        }
        if iface.class == 0x03 {
            match Hid::attach(hc, &mut dev, iface).await {
                Ok(h) => drv.hids.push(h),
                Err(e) => warn!("port {port} if{}: HID setup failed: {e}", iface.number),
            }
        }
    }
    ports[port as usize] = Some(dev);
}

async fn detach<H: HostController>(hc: &mut H, port: u8, ports: &mut [Option<Device<H>>], drv: &mut Drivers) {
    drv.hids.retain(|h| h.port != port);
    drv.mscs.retain(|m| m.port != port);
    drv.ecms.retain(|e| {
        if e.port == port {
            e.detach();
        }
        e.port != port
    });
    if let Some(dev) = ports[port as usize].take() {
        info!(
            "port {port}: {:04x}:{:04x} detached",
            dev.desc.vendor, dev.desc.product
        );
        dev.disable(hc).await;
    }
}
