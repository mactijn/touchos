//! The Toon's Z-Wave controller: a ZW0301 with Sigma's Serial API 4.55
//! static-controller firmware, on UART3 (`ttymxc2`, 0x1000c000, IRQ 18) at
//! 115200 8N1 (from the stock system: docs/hardware-toon.md, "Z-Wave").
//!
//! **Queries only.** It asks the chip what it is and what it knows (version,
//! home ID, capabilities, the nodes in its network and their device
//! classes), logs it, and then listens: every frame the chip sends is
//! acknowledged (or it would send it again), and reports from other nodes
//! are logged. Meanwhile it pings the other nodes (NoOperation) every 30 s
//! for 30 minutes, until each answers, and asks the ones that do for their
//! manufacturer and version. Nothing it sends changes the chip, its
//! network or a node, except with a test switch on the command line:
//! - `zwave=pair`: exclusion, then inclusion (a button press each), then
//!   ask the new node who it is;
//! - `zwave=reset-pair`: the same after SetDefault (a fresh network: the
//!   old one is forgotten);
//! - `zwave=include`: inclusion only (one press; for a device in its
//!   factory state).
//!
//! Capabilities: "uart" (UART3's page), "irq" (its interrupt).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use log::{info, warn};
use touchos_drv::mmio::Regs;
use touchos_rt::irq::Irq;
use touchos_rt::{cap, exec, time};
use touchos_zwave::{self as zw, AddRemoveStatus, Event, Frame, Kind, Manage, NodeCmd, Query, TxStatus};

touchos_rt::entry!(main);

const URXD: usize = 0x00;
const UTXD: usize = 0x40;
const UCR1: usize = 0x80;
const UCR2: usize = 0x84;
const UCR3: usize = 0x88;
const UCR4: usize = 0x8c;
const UFCR: usize = 0x90;
const USR2: usize = 0x98;
const UBIR: usize = 0xa4;
const UBMR: usize = 0xa8;
const UTS: usize = 0xb4;

const UCR1_UARTEN: u32 = 1 << 0;
/// Interrupt when the receive FIFO reaches its threshold.
const UCR1_RRDYEN: u32 = 1 << 9;
/// Not in reset, receiver and transmitter on, 8 bits, ignore RTS.
const UCR2_RXTX: u32 = 1 << 0 | 1 << 1 | 1 << 2 | 1 << 5 | 1 << 14;
const USR2_RDR: u32 = 1 << 0;
const URXD_ERR: u32 = 1 << 14;
const UTS_TXFULL: u32 = 1 << 4;

/// The Serial API's rate; the older firmware's (9600) if nothing answers.
const RATES: [u32; 2] = [115_200, 9600];
/// How long an ACK may take, and how often a frame is sent at most.
const ACK_MS: u64 = 1600;
const TRIES: u32 = 3;
/// How long a response may take after the ACK.
const RESPONSE_MS: u64 = 2000;
/// A frame cut off: forget it after this long without a byte.
const BYTE_GAP_MS: u64 = 1500;
const SUMMARY_EVERY_MS: u64 = 10 * 60_000;
/// SendData: how long the transmit status may take (routing included).
const SEND_STATUS_MS: u64 = 10_000;
/// Nodes not reached are pinged again this often, for this long.
const PROBE_EVERY_MS: u64 = 30_000;
const PROBE_FOR_MS: u64 = 30 * 60_000;
/// How long SetDefault may take, and how long a pairing step waits for a
/// button (long: the person pressing it reads the log second-hand).
const RESET_MS: u64 = 10_000;
const PAIR_WINDOW_MS: u64 = 5 * 60_000;
/// After inclusion, wait this long before asking the new node anything.
const AFTER_JOIN_MS: u64 = 5000;

struct Uart {
    regs: Regs,
}

impl Uart {
    fn new(regs: Regs) -> Uart {
        // As UART1 (the console): UCR3 0x704, UCR4 0x8000, the reference
        // divider by 2 (UFCR 0x200), receive threshold 1 character.
        regs.w32(UCR2, 0); // reset
        regs.w32(UCR3, 0x704);
        regs.w32(UCR4, 0x8000);
        regs.w32(UFCR, 0x200 | 1);
        Uart { regs }
    }

    /// Out of reset first (the rate registers written in reset don't
    /// stick); UBMR last, which loads the rate. As servers/boiler.
    fn set_rate(&self, baud: u32) {
        let ubmr = 103 * 115_200 / baud - 1;
        self.regs.w32(UCR2, UCR2_RXTX);
        self.regs.w32(UCR1, UCR1_UARTEN);
        self.regs.w32(UBIR, 15);
        self.regs.w32(UBMR, ubmr);
        self.regs.w32(UCR1, UCR1_UARTEN | UCR1_RRDYEN);
    }

    fn send(&self, bytes: &[u8]) {
        for &b in bytes {
            while self.regs.r32(UTS) & UTS_TXFULL != 0 {}
            self.regs.w32(UTXD, b as u32);
        }
    }

    /// Everything in the receive FIFO: (byte, had an error).
    fn drain(&self, out: &mut Vec<(u8, bool)>) {
        while self.regs.r32(USR2) & USR2_RDR != 0 {
            let w = self.regs.r32(URXD);
            out.push((w as u8, w & URXD_ERR != 0));
        }
    }
}

#[derive(Default)]
struct Stats {
    bytes: u64,
    line_errors: u64,
    frames: u64,
    bad: u64,
    stray: u64,
    reports: u64,
}

/// The line: the UART, its interrupt, the receiver, and what arrived but
/// wasn't asked for yet.
struct Link {
    uart: Uart,
    irq: Option<Irq>,
    rx: Receiver,
    st: Stats,
    /// The last SendData callback ID (1..=255).
    callback: u8,
}

struct Receiver {
    inner: zw::Receiver,
    last_byte_ms: u64,
    buf: Vec<(u8, bool)>,
    events: Vec<Event>,
}

#[derive(Debug)]
enum Fail {
    /// No ACK after all tries (or NAK/CAN each time).
    NoAck,
    /// ACKed, but no response in time.
    NoResponse,
    /// SendData: the chip didn't queue the frame.
    NotQueued,
    /// SendData: no transmit status in time.
    NoCallback,
}

impl Link {
    /// The next event, or None at `deadline`. Data frames are
    /// acknowledged here (bad ones NAKed), as the Serial API requires.
    async fn next(&mut self, deadline: u64) -> Option<Event> {
        loop {
            if !self.rx.events.is_empty() {
                return Some(self.rx.events.remove(0));
            }
            let now = time::now_ms();
            if self.rx.inner.busy() && now >= self.rx.last_byte_ms + BYTE_GAP_MS {
                warn!("a frame cut off; dropped");
                self.rx.inner.reset();
            }
            if now >= deadline {
                return None;
            }
            let wait = (deadline - now).min(BYTE_GAP_MS);
            match &self.irq {
                Some(i) => {
                    i.ack();
                    exec::wait_bits_or_ms(i.bit(), wait).await;
                }
                None => exec::sleep_ms(wait.min(10)).await,
            }
            self.rx.buf.clear();
            self.uart.drain(&mut self.rx.buf);
            if !self.rx.buf.is_empty() {
                self.rx.last_byte_ms = time::now_ms();
            }
            for i in 0..self.rx.buf.len() {
                let (b, err) = self.rx.buf[i];
                self.st.bytes += 1;
                if err {
                    self.st.line_errors += 1;
                }
                let Some(ev) = self.rx.inner.push(b) else { continue };
                match &ev {
                    Event::Frame(f) => {
                        self.st.frames += 1;
                        self.uart.send(&[zw::ACK]);
                        log::debug!("rx {:?} {:#04x}: {}", f.kind, f.func, hex(&f.data));
                    }
                    Event::Bad => {
                        self.st.bad += 1;
                        self.uart.send(&[zw::NAK]);
                    }
                    Event::Stray(_) => self.st.stray += 1,
                    _ => {}
                }
                self.rx.events.push(ev);
            }
        }
    }

    /// Send `q` and wait for its response (frames from the chip that
    /// arrive meanwhile are handled by `other`).
    async fn ask(&mut self, q: Query, other: &mut impl FnMut(&mut Stats, Frame)) -> Result<Frame, Fail> {
        self.transmit(&q.frame(), other).await?;
        let deadline = time::now_ms() + RESPONSE_MS;
        loop {
            match self.next(deadline).await {
                Some(Event::Frame(f)) if f.kind == Kind::Response && f.func == q.func() => return Ok(f),
                Some(Event::Frame(f)) => other(&mut self.st, f),
                Some(_) => {}
                None => return Err(Fail::NoResponse),
            }
        }
    }

    /// The next request from the chip with function `func` (a callback),
    /// or None at `deadline`.
    async fn wait_request(&mut self, func: u8, deadline: u64, other: &mut impl FnMut(&mut Stats, Frame)) -> Option<Frame> {
        loop {
            match self.next(deadline).await? {
                Event::Frame(f) if f.kind == Kind::Request && f.func == func => return Some(f),
                Event::Frame(f) => other(&mut self.st, f),
                _ => {}
            }
        }
    }

    fn next_callback(&mut self) -> u8 {
        self.callback = self.callback.checked_add(1).unwrap_or(1);
        self.callback
    }

    /// Send a frame until the chip acknowledges it (up to TRIES times).
    async fn transmit(&mut self, frame: &Frame, other: &mut impl FnMut(&mut Stats, Frame)) -> Result<(), Fail> {
        let bytes = frame.encode();
        let mut acked = false;
        for attempt in 0..TRIES {
            if attempt > 0 {
                exec::sleep_ms(100 + 1000 * attempt as u64).await;
            }
            self.uart.send(&bytes);
            let deadline = time::now_ms() + ACK_MS;
            loop {
                match self.next(deadline).await {
                    Some(Event::Ack) => {
                        acked = true;
                        break;
                    }
                    Some(Event::Nak | Event::Can) | None => break,
                    Some(Event::Frame(f)) => other(&mut self.st, f),
                    Some(_) => {}
                }
            }
            if acked {
                break;
            }
        }
        if acked { Ok(()) } else { Err(Fail::NoAck) }
    }

    /// Send `cmd` to `node` and wait for the transmit status (the node's
    /// answer to a Get comes later, as a report).
    async fn send_to(&mut self, node: u8, cmd: NodeCmd, other: &mut impl FnMut(&mut Stats, Frame)) -> Result<TxStatus, Fail> {
        let id = self.next_callback();
        let res = self.ask(Query::Send { node, cmd, callback: id }, other).await?;
        if res.data.first() != Some(&1) {
            return Err(Fail::NotQueued);
        }
        let deadline = time::now_ms() + SEND_STATUS_MS;
        loop {
            match self.next(deadline).await {
                Some(Event::Frame(f)) => match zw::send_callback(&f) {
                    Some((cb, st)) if cb == id => return Ok(st),
                    Some((cb, st)) => info!("a late transmit status: callback {cb}, {st:?}"),
                    None => other(&mut self.st, f),
                },
                Some(_) => {}
                None => return Err(Fail::NoCallback),
            }
        }
    }
}

fn hex(d: &[u8]) -> String {
    let mut s = String::new();
    for (i, b) in d.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&alloc::format!("{b:02x}"));
    }
    s
}

/// A frame the chip sent on its own: a report from a node, or a change in
/// the network; logged.
fn unsolicited(st: &mut Stats, f: Frame) {
    match f.func {
        zw::func::APPLICATION_COMMAND_HANDLER => match zw::Command::parse(&f.data) {
            Some(c) if zw::version_report(&c).is_some() => {
                st.reports += 1;
                let v = zw::version_report(&c).unwrap_or(zw::NodeVersion { library: 0, protocol: (0, 0), application: (0, 0) });
                // Versions in hex digits: protocol 0x03.0x43 is "3.67" (decimal minor).
                info!(
                    "node {}: library type {}, protocol {}.{:02}, application {}.{:02}",
                    c.node, v.library, v.protocol.0, v.protocol.1, v.application.0, v.application.1
                );
            }
            Some(c) if zw::manufacturer_report(&c).is_some() => {
                st.reports += 1;
                let (m, t, p) = zw::manufacturer_report(&c).unwrap_or_default();
                info!("node {}: manufacturer {m:04x}, product type {t:04x}, product {p:04x}", c.node);
            }
            Some(c) => {
                st.reports += 1;
                info!(
                    "node {}: {} ({:#04x}) command {} [{}]",
                    c.node,
                    zw::command_class_name(c.class),
                    c.class,
                    c.cmd.map_or(String::from("-"), |c| alloc::format!("{c:#04x}")),
                    hex(&c.params)
                );
            }
            None => info!("a short report: {}", hex(&f.data)),
        },
        zw::func::APPLICATION_UPDATE => info!("network update: {}", hex(&f.data)),
        _ => info!("{:?} {:#04x}: {}", f.kind, f.func, hex(&f.data)),
    }
}

/// What the interview found: the other nodes the controller knows.
struct Network {
    nodes: Vec<u8>,
}

/// Ask everything; None if the chip doesn't answer at all.
async fn interview(link: &mut Link) -> Option<Network> {
    let mut other = unsolicited;
    match link.ask(Query::Version, &mut other).await {
        Ok(f) => match zw::Version::parse(&f.data) {
            Some(v) => info!("{}, library: {}", v.text, v.library_name()),
            None => info!("version: {}", hex(&f.data)),
        },
        Err(e) => {
            warn!("GetVersion: {e:?}");
            return None;
        }
    }
    let mut own = 1;
    match link.ask(Query::MemoryId, &mut other).await {
        Ok(f) => match zw::MemoryId::parse(&f.data) {
            Some(m) => {
                own = m.node_id;
                info!("home ID {:08x}, this controller is node {}", m.home_id, m.node_id);
            }
            None => info!("memory ID: {}", hex(&f.data)),
        },
        Err(e) => warn!("MemoryGetId: {e:?}"),
    }
    match link.ask(Query::Capabilities, &mut other).await {
        Ok(f) => match zw::Capabilities::parse(&f.data) {
            Some(c) => info!(
                "firmware {}.{:02}, manufacturer {:04x}, product {:04x}:{:04x}, {} functions",
                c.app_version, c.app_revision, c.manufacturer, c.product_type, c.product_id, c.count()
            ),
            None => info!("capabilities: {}", hex(&f.data)),
        },
        Err(e) => warn!("SerialApiGetCapabilities: {e:?}"),
    }
    match link.ask(Query::ControllerCapabilities, &mut other).await {
        Ok(f) => match zw::ControllerCaps::parse(&f.data) {
            Some(c) => info!(
                "controller: {:#04x} (secondary {}, other network {}, SIS present {}, real primary {}, SUC {})",
                c.0, c.secondary(), c.on_other_network(), c.sis_present(), c.real_primary(), c.suc()
            ),
            None => info!("controller capabilities: {}", hex(&f.data)),
        },
        Err(e) => warn!("GetControllerCapabilities: {e:?}"),
    }
    let nodes = match link.ask(Query::InitData, &mut other).await {
        Ok(f) => match zw::InitData::parse(&f.data) {
            Some(d) => {
                info!(
                    "Serial API {}, caps {:#04x}, chip type {:?} version {:?}, {} nodes: {:?}",
                    d.api_version, d.caps, d.chip_type, d.chip_version, d.nodes.len(), d.nodes
                );
                d.nodes
            }
            None => {
                info!("init data: {}", hex(&f.data));
                Vec::new()
            }
        },
        Err(e) => {
            warn!("SerialApiGetInitData: {e:?}");
            Vec::new()
        }
    };
    for &n in &nodes {
        match link.ask(Query::NodeProtocolInfo(n), &mut other).await {
            Ok(f) => match zw::NodeInfo::parse(&f.data) {
                Some(i) if i.known() => info!(
                    "node {n}: {} / {} ({:#04x}:{:#04x}), {}{}{}protocol {}",
                    zw::basic_class_name(i.basic),
                    zw::generic_class_name(i.generic),
                    i.generic,
                    i.specific,
                    if i.listening() { "listening, " } else { "sleeping, " },
                    if i.routing() { "routing, " } else { "" },
                    if i.flirs() { "FLiRS, " } else { "" },
                    i.protocol_version()
                ),
                Some(_) => info!("node {n}: not known to the controller"),
                None => info!("node {n}: {}", hex(&f.data)),
            },
            Err(e) => warn!("GetNodeProtocolInfo({n}): {e:?}"),
        }
    }
    Some(Network { nodes: nodes.into_iter().filter(|&n| n != own).collect() })
}

/// SetDefault: the chip forgets its network and starts a new one.
async fn reset_network(link: &mut Link) -> bool {
    let mut other = unsolicited;
    let m = Manage::SetDefault { callback: link.next_callback() };
    warn!("zwave=reset-pair: forgetting the controller's network (SetDefault)");
    if let Err(e) = link.transmit(&m.frame(), &mut other).await {
        warn!("SetDefault: {e:?}");
        return false;
    }
    let deadline = time::now_ms() + RESET_MS;
    while let Some(f) = link.wait_request(m.func(), deadline, &mut other).await {
        if f.data.first() == Some(&m.callback()) {
            match link.ask(Query::MemoryId, &mut other).await.ok().and_then(|f| zw::MemoryId::parse(&f.data)) {
                Some(id) => info!("a new network: home ID {:08x}, this controller is node {}", id.home_id, id.node_id),
                None => info!("a new network (its home ID not read)"),
            }
            return true;
        }
    }
    warn!("SetDefault: no callback");
    false
}

/// Exclusion (`remove`) or inclusion: wait up to PAIR_WINDOW_MS for a
/// node's button. Returns the node ID (when adding: the new one; when
/// removing: the one it had, 0 if it had none).
async fn add_or_remove(link: &mut Link, add: bool) -> Option<u8> {
    let mut other = unsolicited;
    let what = if add { "inclusion (adding)" } else { "exclusion (removing)" };
    let cb = link.next_callback();
    let start = if add { Manage::AddNodeStart { callback: cb } } else { Manage::RemoveNodeStart { callback: cb } };
    let stop = |callback| if add { Manage::AddNodeStop { callback } } else { Manage::RemoveNodeStop { callback } };
    if let Err(e) = link.transmit(&start.frame(), &mut other).await {
        warn!("{what}: {e:?}");
        return None;
    }
    let deadline = time::now_ms() + PAIR_WINDOW_MS;
    let mut node = None;
    loop {
        let Some(f) = link.wait_request(start.func(), deadline, &mut other).await else {
            warn!("{what}: no device in {} s; stopped", PAIR_WINDOW_MS / 1000);
            let _ = link.transmit(&stop(0).frame(), &mut other).await;
            return None;
        };
        let Some(a) = zw::AddRemove::parse(&f) else { continue };
        if a.callback != cb {
            continue;
        }
        match a.status {
            AddRemoveStatus::Ready => info!("{what}: ready. PRESS THE DEVICE'S BUTTON (within {} s)", PAIR_WINDOW_MS / 1000),
            AddRemoveStatus::NodeFound => info!("{what}: a device found"),
            AddRemoveStatus::Slave | AddRemoveStatus::Controller => {
                node = Some(a.node);
                let classes = a.classes.map_or(String::from("-"), |(b, g, s)| {
                    alloc::format!("{} / {} ({g:#04x}:{s:#04x})", zw::basic_class_name(b), zw::generic_class_name(g))
                });
                let ccs: Vec<String> =
                    a.command_classes.iter().map(|&c| alloc::format!("{} ({c:#04x})", zw::command_class_name(c))).collect();
                info!("{what}: node {} ({:?}): {classes}; command classes: {}", a.node, a.status, ccs.join(", "));
            }
            AddRemoveStatus::ProtocolDone => {
                info!("{what}: protocol done; finishing");
                let _ = link.transmit(&stop(cb).frame(), &mut other).await;
            }
            AddRemoveStatus::Done => {
                if !add {
                    let _ = link.transmit(&stop(0).frame(), &mut other).await;
                }
                info!("{what}: done, node {}", node.unwrap_or(a.node));
                return Some(node.unwrap_or(a.node));
            }
            AddRemoveStatus::Failed => {
                warn!("{what}: failed");
                let _ = link.transmit(&stop(0).frame(), &mut other).await;
                return None;
            }
            AddRemoveStatus::Other(b) => info!("{what}: status {b}"),
        }
    }
}

/// `zwave=pair` / `zwave=reset-pair` / `zwave=include`: (a fresh
/// network,) (exclusion,) inclusion, then who the new node is. Returns the
/// new node.
async fn pair(link: &mut Link, reset: bool, exclude: bool) -> Option<u8> {
    if reset && !reset_network(link).await {
        return None;
    }
    if exclude {
        info!("pairing: exclusion (resets a device from any network), then inclusion. Press the button once; press again only if it doesn't join by itself");
        match add_or_remove(link, false).await {
            Some(0) => info!("excluded a device that wasn't in a network"),
            Some(n) => info!("excluded a device that was node {n} of its old network"),
            None => {}
        }
    } else {
        info!("pairing: inclusion only (the device must be in its factory state): press the button once");
    }
    let node = add_or_remove(link, true).await?;
    // Just after joining, a device is busy (a meter adapter didn't answer
    // Gets sent within a second).
    exec::sleep_ms(AFTER_JOIN_MS).await;
    let mut other = unsolicited;
    match link.ask(Query::NodeProtocolInfo(node), &mut other).await.ok().and_then(|f| zw::NodeInfo::parse(&f.data)) {
        Some(i) => info!(
            "node {node}: {} / {}, {}",
            zw::basic_class_name(i.basic),
            zw::generic_class_name(i.generic),
            if i.listening() { "listening" } else { "sleeping" }
        ),
        None => warn!("node {node}: no protocol info"),
    }
    for cmd in [NodeCmd::ManufacturerSpecificGet, NodeCmd::VersionGet] {
        match link.send_to(node, cmd, &mut other).await {
            Ok(TxStatus::Ok) => info!("node {node}: {cmd:?} acknowledged"),
            Ok(s) => warn!("node {node}: {cmd:?}: {s:?}"),
            Err(e) => warn!("node {node}: {cmd:?}: {e:?}"),
        }
    }
    Some(node)
}

/// Ping the nodes not reached yet; ask the ones that answer who they are.
/// Returns the nodes still not reached.
async fn probe(link: &mut Link, unreached: Vec<u8>, first: bool) -> Vec<u8> {
    let mut other = unsolicited;
    let mut still = Vec::new();
    for n in unreached {
        match link.send_to(n, NodeCmd::NoOperation, &mut other).await {
            Ok(TxStatus::Ok) => {
                info!("node {n}: reached (it acknowledged a ping); asking who it is");
                for cmd in [NodeCmd::ManufacturerSpecificGet, NodeCmd::VersionGet] {
                    match link.send_to(n, cmd, &mut other).await {
                        Ok(TxStatus::Ok) => info!("node {n}: {cmd:?} acknowledged"),
                        Ok(s) => warn!("node {n}: {cmd:?}: {s:?}"),
                        Err(e) => warn!("node {n}: {cmd:?}: {e:?}"),
                    }
                }
            }
            Ok(s) => {
                if first {
                    info!("node {n}: not reached ({s:?})");
                }
                still.push(n);
            }
            Err(e) => {
                warn!("node {n}: ping: {e:?}");
                still.push(n);
            }
        }
    }
    still
}

async fn run(uart: Uart, irq: Option<Irq>) {
    let mut link = Link {
        uart,
        irq,
        rx: Receiver { inner: zw::Receiver::new(), last_byte_ms: 0, buf: Vec::new(), events: Vec::new() },
        st: Stats::default(),
        callback: 0,
    };
    let mut network = None;
    for &rate in &RATES {
        link.uart.set_rate(rate);
        link.rx.inner.reset();
        info!("UART3 at {rate} baud; NAK to resync, then queries only");
        // A NAK makes the chip drop a frame it was half-way through.
        link.uart.send(&[zw::NAK]);
        let settle = time::now_ms() + 200;
        while let Some(ev) = link.next(settle).await {
            info!("before asking: {ev:?}");
        }
        network = interview(&mut link).await;
        if network.is_some() {
            break;
        }
    }
    let mode = touchos_rt::arg("zwave");
    let mut unreached = match network {
        Some(_) if matches!(mode, Some("pair" | "reset-pair" | "include")) => {
            pair(&mut link, mode == Some("reset-pair"), mode != Some("include")).await;
            Vec::new()
        }
        Some(n) => n.nodes,
        None => {
            warn!(
                "the controller doesn't answer ({} bytes, {} line errors, {} stray); suspect its reset line or the pins",
                link.st.bytes, link.st.line_errors, link.st.stray
            );
            Vec::new()
        }
    };
    // Listen: acknowledge and log whatever the chip sends. Meanwhile ping
    // the nodes not reached yet, every PROBE_EVERY_MS for PROBE_FOR_MS
    // (a device may be powered later).
    let start = time::now_ms();
    let mut first = true;
    let mut next_probe = start;
    let mut next_summary = start + SUMMARY_EVERY_MS;
    loop {
        let now = time::now_ms();
        let probing = !unreached.is_empty() && now < start + PROBE_FOR_MS;
        if probing && now >= next_probe {
            unreached = probe(&mut link, unreached, first).await;
            if first && !unreached.is_empty() {
                info!("pinging {:?} again every {} s for {} min", unreached, PROBE_EVERY_MS / 1000, PROBE_FOR_MS / 60_000);
            }
            first = false;
            next_probe = time::now_ms() + PROBE_EVERY_MS;
            if !unreached.is_empty() && next_probe >= start + PROBE_FOR_MS {
                info!("no longer pinging {:?}", unreached);
            }
            continue;
        }
        if now >= next_summary {
            next_summary = now + SUMMARY_EVERY_MS;
            let s = &link.st;
            if s.bytes > 0 {
                info!(
                    "{} bytes, {} frames ({} reports), {} bad, {} stray, {} line errors",
                    s.bytes, s.frames, s.reports, s.bad, s.stray, s.line_errors
                );
            }
            continue;
        }
        let deadline = if probing { next_probe.min(next_summary) } else { next_summary };
        match link.next(deadline).await {
            Some(Event::Frame(f)) => unsolicited(&mut link.st, f),
            Some(Event::Stray(b)) if link.st.stray <= 64 => info!("stray byte {b:#04x}"),
            Some(Event::Bad) => warn!("a frame with a bad checksum (NAKed)"),
            _ => {}
        }
    }
}

fn main() {
    let Some(regs) = cap("uart").and_then(|c| Regs::from_cap(c, 0, 0x1000).ok()) else {
        warn!("no UART (\"uart\")");
        return;
    };
    let irq = Irq::take("irq");
    exec::spawn(run(Uart::new(regs), irq));
    exec::run()
}
