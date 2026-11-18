//! mDNS on the interface: the group 224.0.0.251 and UDP port 5353, with
//! touchos_mdns's responder answering for our name (`<host>.local`) and
//! the services other processes register (MDNS_REGISTER).

use alloc::string::String;
use alloc::vec;
use log::{info, warn};
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpEndpoint, Ipv4Address};
use touchos_mdns::{Dest, GROUP, PORT, Packet, Responder, Service};
use touchos_rt::time;

pub struct Mdns {
    udp: SocketHandle,
    pub responder: Responder,
    /// The name and readiness we last logged.
    logged: (String, bool),
}

impl Mdns {
    /// Join the group and open the port.
    pub fn start(
        iface: &mut Interface,
        sockets: &mut SocketSet<'static>,
        host: &str,
        services: &[Service],
    ) -> Option<Mdns> {
        if let Err(e) = iface.join_multicast_group(Ipv4Address::from(GROUP)) {
            warn!("mDNS: can't join 224.0.0.251: {e:?}");
            return None;
        }
        let mut s = udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 8 * 1024]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0; 8 * 1024]),
        );
        // RFC 6762 §11: sent with TTL 255, so a receiver can tell they come
        // from the local link.
        s.set_hop_limit(Some(255));
        s.bind(PORT).ok()?;
        let mut responder = Responder::new(host);
        let now = time::now_ms();
        for svc in services {
            responder.add_service(svc.clone(), now);
        }
        Some(Mdns {
            udp: sockets.add(s),
            responder,
            logged: (host.into(), false),
        })
    }

    pub fn stop(self, iface: &mut Interface, sockets: &mut SocketSet<'static>) {
        let _ = iface.leave_multicast_group(Ipv4Address::from(GROUP));
        sockets.remove(self.udp);
    }

    /// A new name from the settings (not a conflict's `-2`).
    pub fn set_host(&mut self, host: &str) {
        self.responder.set_host(host, time::now_ms());
        self.logged = (host.into(), false);
    }

    /// Answer what arrived and send what is due. Returns when to come back,
    /// in ms from now.
    pub fn pump(&mut self, sockets: &mut SocketSet<'static>) -> Option<u64> {
        let now = time::now_ms();
        let udp = sockets.get_mut::<udp::Socket>(self.udp);
        let mut buf = [0u8; 1500];
        loop {
            let (n, meta) = match udp.recv_slice(&mut buf) {
                Ok(x) => x,
                Err(udp::RecvError::Truncated) => continue,
                Err(udp::RecvError::Exhausted) => break,
            };
            let IpAddress::Ipv4(src) = meta.endpoint.addr;
            if let Some(p) = self.responder.on_packet(src.octets(), meta.endpoint.port, &buf[..n], now) {
                send(udp, &p);
            }
        }
        while udp.can_send() {
            let Some(p) = self.responder.poll(now) else {
                break;
            };
            send(udp, &p);
        }
        self.log();
        self.responder.poll_at().map(|at| at.saturating_sub(now))
    }

    fn log(&mut self) {
        let (host, ready) = (self.responder.host(), self.responder.is_ready());
        if host != self.logged.0 {
            info!("mDNS: {}.local is taken; trying {host}.local", self.logged.0);
        }
        if ready && !self.logged.1 {
            info!("mDNS: answering as {host}.local");
        }
        self.logged = (host.into(), ready);
    }
}

fn send(udp: &mut udp::Socket, p: &Packet) {
    let (ip, port) = match p.dest {
        Dest::Multicast => (GROUP, PORT),
        Dest::Unicast(ip, port) => (ip, port),
    };
    let ep = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::from(ip)), port);
    if udp.send_slice(&p.data, ep).is_err() {
        warn!("mDNS: a {}-byte packet didn't fit the send buffer", p.data.len());
    }
}
