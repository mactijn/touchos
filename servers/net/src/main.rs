//! The network server: smoltcp over the frames the network drivers deliver
//! (through shared-memory rings that init sets up), with DHCP and DNS. Other
//! processes get TCP connections whose data flows through shared memory
//! (see `touchos_proto::net`); they never see the stack itself. It also
//! sets the clock from NTP and ships the kernel log to `debug.log_host`.
//!
//! Capabilities: "ep" (our endpoint), "wired_rx"/"wired_tx"/"wired_state" and
//! "wlan_rx"/... (frames and link state of each network driver), "status" (the board for the dashboard),
//! "config" (public settings), "system" (TIME_SET and KLOG_READ rights).

#![no_std]
#![no_main]

extern crate alloc;

mod ntp;
mod service;

use service::{Conn, Listener, handle, pump_conns, pump_listeners};

use alloc::vec;
use alloc::vec::Vec;
use log::{info, warn};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::{dhcpv4, dns, udp};
use smoltcp::time::Instant;
use smoltcp::wire::{
    EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr,
};
use touchos_proto::net::{self as proto, FRAME_SLOT, Link, Status};
use touchos_rt::shm::{self, Board, Ring, Shared};
use touchos_rt::{cap, exec, sys, time};

touchos_rt::entry!(main);

/// The frames from and to the drivers.
/// A network driver (USB Ethernet, Wi-Fi): frame rings and its link state.
struct Provider {
    name: &'static str,
    rx: Shared<Ring>,
    tx: Shared<Ring>,
    state: Shared<Board>,
    seq: u64,
    mac: Option<[u8; 6]>,
}

/// The frames of the active link (the first provider that is up).
struct Frames {
    providers: Vec<Provider>,
    active: Option<usize>,
}

impl Frames {
    fn rings(&self) -> Option<(&Ring, &Ring)> {
        let p = &self.providers[self.active?];
        Some((&p.rx, &p.tx))
    }

    /// Wake the active driver if frames wait for it.
    fn kick(&self) {
        if let Some(p) = self.active.map(|i| &self.providers[i])
            && !p.tx.is_empty()
        {
            let _ = sys::mem_notify(p.tx.mem());
        }
    }

    /// Read the link states; returns the (provider, MAC) that should be
    /// active now.
    fn update(&mut self) -> Option<(usize, [u8; 6])> {
        let mut buf = Vec::new();
        for p in &mut self.providers {
            let before = p.seq;
            let link: Option<Link> = p.state.read_as(&mut p.seq, &mut buf);
            if p.seq != before {
                p.mac = link.and_then(|l| l.mac);
            }
        }
        self.providers
            .iter()
            .enumerate()
            .find_map(|(i, p)| p.mac.map(|m| (i, m)))
    }
}

fn now() -> Instant {
    Instant::from_millis(time::now_ms() as i64)
}

struct RxToken(Vec<u8>);
struct TxToken<'a>(&'a Ring);

impl phy::RxToken for RxToken {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl phy::TxToken for TxToken<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0u8; len];
        let r = f(&mut frame);
        let _ = self.0.push(&frame);
        r
    }
}

impl phy::Device for Frames {
    type RxToken<'a> = RxToken;
    type TxToken<'a> = TxToken<'a>;

    fn receive(&mut self, _: Instant) -> Option<(RxToken, TxToken<'_>)> {
        let (rx, tx) = self.rings()?;
        if !tx.has_room() {
            return None; // a reply couldn't be sent: leave the frame queued
        }
        let mut buf = vec![0u8; FRAME_SLOT as usize];
        let n = rx.pop(&mut buf)?;
        buf.truncate(n);
        Some((RxToken(buf), TxToken(tx)))
    }

    /// Only with room in the ring: otherwise smoltcp keeps the packet
    /// (backpressure instead of silently dropping frames).
    fn transmit(&mut self, _: Instant) -> Option<TxToken<'_>> {
        let (_, tx) = self.rings()?;
        tx.has_room().then_some(TxToken(tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps
    }
}

/// The interface while a link is up.
pub struct Net {
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
    dhcp: SocketHandle,
    pub dns: SocketHandle,
    log_udp: SocketHandle,
    pub ipv4: Option<Ipv4Cidr>,
    /// The time servers DHCP named (option 42), for ntp.
    pub ntp_servers: Vec<Ipv4Address>,
    next_port: u16,
}

impl Net {
    fn new(mac: [u8; 6], dev: &mut Frames) -> Net {
        let config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
        let iface = Interface::new(config, dev, now());
        let mut sockets = SocketSet::new(Vec::new());
        // A DISCOVER right after link-up can get lost (a switch port that
        // isn't forwarding yet: seen on the Toon). smoltcp waits 10 s before
        // the next one; 4 s is plenty on a LAN.
        let mut dhcp = dhcpv4::Socket::new();
        let mut retry = dhcpv4::RetryConfig::default();
        retry.discover_timeout = smoltcp::time::Duration::from_secs(4);
        dhcp.set_retry_config(retry);
        // Also ask for the network's time servers (option 42), and keep
        // the ACK to read them from (smoltcp parses only the rest).
        dhcp.set_parameter_request_list(&[1, 3, 6, DHCP_OPT_NTP]);
        dhcp.set_receive_packet_buffer(alloc::boxed::Box::leak(vec![0u8; 1024].into_boxed_slice()));
        let dhcp = sockets.add(dhcp);
        let dns = sockets.add(dns::Socket::new(&[], vec![None, None, None, None]));
        let mut log = udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 2], vec![0; 256]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 64], vec![0; 32 * 1024]),
        );
        let _ = log.bind(5556);
        let log_udp = sockets.add(log);
        Net {
            iface,
            sockets,
            dhcp,
            dns,
            log_udp,
            ipv4: None,
            ntp_servers: Vec::new(),
            next_port: 49152,
        }
    }

    pub fn ephemeral_port(&mut self) -> u16 {
        self.next_port = if self.next_port >= 65000 {
            49152
        } else {
            self.next_port + 1
        };
        self.next_port
    }

    /// DHCP events after a poll.
    fn dhcp(&mut self) {
        let event = match self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp).poll() {
            Some(dhcpv4::Event::Configured(c)) => {
                self.ntp_servers = c.packet.map(|p| ntp_option(&p)).unwrap_or_default();
                Some(Some((c.address, c.router, c.dns_servers.iter().copied().collect::<Vec<_>>())))
            }
            Some(dhcpv4::Event::Deconfigured) => Some(None),
            None => None,
        };
        match event {
            Some(Some((address, router, dns_servers))) => {
                self.iface.update_ip_addrs(|a| {
                    a.clear();
                    let _ = a.push(IpCidr::Ipv4(address));
                });
                match router {
                    Some(r) => {
                        let _ = self.iface.routes_mut().add_default_ipv4_route(r);
                    }
                    None => {
                        self.iface.routes_mut().remove_default_ipv4_route();
                    }
                }
                let servers: Vec<IpAddress> =
                    dns_servers.iter().map(|s| IpAddress::Ipv4(*s)).collect();
                self.sockets
                    .get_mut::<dns::Socket>(self.dns)
                    .update_servers(&servers);
                info!("DHCP: {address} gateway {router:?} dns {dns_servers:?} ntp {:?}", self.ntp_servers);
                self.ipv4 = Some(address);
            }
            Some(None) => {
                if self.ipv4.is_some() {
                    warn!("DHCP lease lost");
                }
                self.iface.update_ip_addrs(|a| a.clear());
                self.iface.routes_mut().remove_default_ipv4_route();
                self.ipv4 = None;
            }
            None => {}
        }
    }
}

struct State {
    frames: Frames,
    net: Option<Net>,
    mac: Option<[u8; 6]>,
    generation: u32,
    conns: Vec<Conn>,
    listeners: Vec<Listener>,
    queries: Vec<(u32, dns::QueryHandle, u32)>,
    next_id: u32,
    /// Our notification (handed to clients) and the bits they signal.
    server: u32,
    server_bits: usize,
    log_host: Option<IpEndpoint>,
    log_cursor: u64,
    system: Option<u32>,
}

/// All state lives here, used between awaits only.
static STATE: touchos_rt::Local<Option<State>> = touchos_rt::Local::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(s.as_mut().expect("state")))
}

/// Run `f` with the interface, if a link is up.
pub fn with_net<R>(f: impl FnOnce(&mut Net) -> R) -> Option<R> {
    with(|s| s.net.as_mut().map(f))
}

fn status(st: &State) -> Status {
    Status {
        ipv4: st
            .net
            .as_ref()
            .and_then(|n| n.ipv4)
            .map(|c| (c.address().octets(), c.prefix_len())),
        generation: st.generation,
    }
}

/// Send new kernel log lines to `debug.log_host`.
fn ship_log(st: &mut State) {
    let (Some(ep), Some(sys_cap)) = (st.log_host, st.system) else {
        return;
    };
    let Some(net) = st.net.as_mut().filter(|n| n.ipv4.is_some()) else {
        return;
    };
    let udp = net.sockets.get_mut::<udp::Socket>(net.log_udp);
    let mut buf = [0u8; 2048];
    while udp.can_send() {
        let Ok((n, next)) = sys::klog_read(sys_cap, &mut buf, st.log_cursor) else {
            return;
        };
        if n == 0 {
            return;
        }
        // Advance past the lines actually sent only, so none get lost when
        // the UDP buffer fills up.
        let lines: Vec<&[u8]> = buf[..n]
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .collect();
        let first = next - lines.len() as u64;
        for (i, line) in lines.iter().enumerate() {
            if udp.send_slice(line, ep).is_err() {
                st.log_cursor = first + i as u64;
                return;
            }
        }
        st.log_cursor = next;
    }
}

/// The longest the protocol loop sleeps when nothing wakes it: frames
/// from the drivers and clients' writes do, smoltcp's timers are in its
/// delay; this bounds how late new kernel log lines go out (`ship_log`).
const MAX_WAIT_MS: u64 = 500;

/// The protocol loop.
async fn stack(link_bit: usize, tx_bit: usize, board: Option<Shared<Board>>) {
    let mut published: Option<Status> = None;
    loop {
        // Link up/down, and which driver carries it.
        with(|st| {
            let now = st.frames.update();
            let cur = st.frames.active.zip(st.mac);
            if now != cur {
                st.frames.active = now.map(|(i, _)| i);
                st.mac = now.map(|(_, m)| m);
                st.generation += 1;
                st.net = st.mac.map(|m| Net::new(m, &mut st.frames));
                st.queries.clear();
                match now {
                    Some((i, _)) => info!(
                        "interface up on {}, waiting for DHCP",
                        st.frames.providers[i].name
                    ),
                    None => info!("link down"),
                }
            }
        });
        let delay = with(|st| {
            let State { net, frames, .. } = st;
            let delay = match net.as_mut() {
                Some(n) => {
                    n.iface.poll(now(), frames, &mut n.sockets);
                    n.dhcp();
                    n.iface
                        .poll_delay(now(), &n.sockets)
                        .map_or(10, |d| d.total_millis())
                }
                None => 100,
            };
            pump_conns(st);
            pump_listeners(st);
            ship_log(st);
            // Send what the clients just wrote right away.
            let State { net, frames, .. } = st;
            if let Some(n) = net.as_mut() {
                n.iface.poll(now(), frames, &mut n.sockets);
            }
            frames.kick();
            delay
        });
        let s = with(|st| status(st));
        if published != Some(s) {
            published = Some(s);
            if let Some(b) = &board {
                b.write_as(&s);
                let _ = sys::mem_notify(b.mem()); // the dashboard waits on it
            }
        }
        if delay > 0 {
            exec::wait_bits_or_ms(link_bit | tx_bit, delay.min(MAX_WAIT_MS)).await;
        } else {
            exec::yield_now().await;
        }
    }
}

fn main() {
    let ep = cap("ep").expect("net: no endpoint");
    let link_bit = exec::alloc_bit();
    let mut providers = Vec::new();
    // (capability prefix, name in the log): the wired link is the FEC (or a
    // USB Ethernet adapter).
    for (name, label) in [("wired", "wired Ethernet"), ("wlan", "Wi-Fi")] {
        let names = (
            alloc::format!("{name}_rx"),
            alloc::format!("{name}_tx"),
            alloc::format!("{name}_state"),
        );
        let (Some(rx), Some(tx), Some(state)) = (cap(&names.0), cap(&names.1), cap(&names.2))
        else {
            continue;
        };
        let (Some(rx), Some(tx), Some(state)) = (shm::ring_of(rx), shm::ring_of(tx), shm::board_of(state, false)) else {
            warn!("can't map the {name} link");
            continue;
        };
        tx.init(proto::FRAME_SLOTS, FRAME_SLOT);
        let _ = sys::mem_wait(rx.mem(), link_bit);
        let _ = sys::mem_wait(state.mem(), link_bit);
        providers.push(Provider { name: label, rx, tx, state, seq: 0, mac: None });
    }
    if providers.is_empty() {
        warn!("no network drivers; nothing to do");
        return;
    }
    let board = shm::board("status", true);

    let tx_bit = exec::alloc_bit();
    let server = sys::notif_create().expect("notif_create");

    let log_host = log_endpoint(&touchos_config::client::settings::get());

    let state = State {
        frames: Frames {
            providers,
            active: None,
        },
        net: None,
        mac: None,
        generation: 0,
        conns: Vec::new(),
        listeners: Vec::new(),
        queries: Vec::new(),
        next_id: 0,
        server,
        server_bits: tx_bit,
        log_host,
        log_cursor: 0,
        system: cap("system"),
    };
    STATE.with(|s| *s = Some(state));
    exec::spawn(stack(link_bit, tx_bit, board));
    let ntp_bit = exec::alloc_bit();
    exec::spawn(ntp::task(ntp_bit));
    exec::spawn(watch_settings(ntp_bit));
    exec::serve(ep, |msg, _badge| with(|st| handle(st, msg)))
}

/// `debug.log_host` as an endpoint.
fn log_endpoint(cfg: &touchos_config::Config) -> Option<IpEndpoint> {
    cfg.debug.log_host.map(|(ip, port)| IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::from(ip)), port))
}

/// Settings that apply while running: the log host, and the NTP server
/// (ntp is woken with `ntp_bit` to ask the new one at once).
async fn watch_settings(ntp_bit: usize) {
    loop {
        let (old, new) = touchos_config::client::settings::changed().await;
        if new.debug.log_host != old.debug.log_host {
            with(|st| st.log_host = log_endpoint(&new));
            info!("log host: {:?}", new.debug.log_host);
        }
        if new.time.ntp_server != old.time.ntp_server {
            let server = with(|st| st.server);
            let _ = sys::signal(server, ntp_bit);
        }
    }
}

/// DHCP option 42: NTP servers, IPv4 addresses.
const DHCP_OPT_NTP: u8 = 42;

/// The NTP servers in a DHCP packet (option 42), at most four.
fn ntp_option(p: &smoltcp::wire::DhcpPacket<&[u8]>) -> Vec<Ipv4Address> {
    p.options()
        .filter(|o| o.kind == DHCP_OPT_NTP)
        .flat_map(|o| o.data.chunks_exact(4).map(|c| Ipv4Address::from(<[u8; 4]>::try_from(c).unwrap())).collect::<Vec<_>>())
        .take(4)
        .collect()
}

/// Used by ntp: a DNS lookup through our own stack.
pub async fn resolve(name: &str) -> Option<IpAddress> {
    if let Ok(ip) = name.parse::<Ipv4Address>() {
        return Some(IpAddress::Ipv4(ip));
    }
    let q = with_net(|n| {
        let cx = n.iface.context();
        n.sockets
            .get_mut::<dns::Socket>(n.dns)
            .start_query(cx, name, smoltcp::wire::DnsQueryType::A)
            .ok()
    })??;
    let end = time::now_ms() + 5000;
    loop {
        let r = with_net(|n| n.sockets.get_mut::<dns::Socket>(n.dns).get_query_result(q))?;
        match r {
            Ok(addrs) => return addrs.first().copied(),
            Err(dns::GetQueryResultError::Pending) if time::now_ms() < end => {
                exec::sleep_ms(20).await
            }
            Err(_) => return None,
        }
    }
}

/// Wait until DHCP has given us an address.
pub async fn wait_configured() {
    while with_net(|n| n.ipv4.is_some()) != Some(true) {
        exec::sleep_ms(200).await;
    }
}
