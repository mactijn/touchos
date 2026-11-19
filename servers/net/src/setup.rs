//! The setup network (the Wi-Fi link is our own open access point, see
//! `wifi.setup_ap`): we are 192.168.4.1/24, lease addresses with
//! touchos_setupnet's DHCP server and answer every DNS name with
//! ourselves, so a phone that joins opens the settings page as a captive
//! portal. No DHCP client, no time servers, nowhere to route to.

use alloc::vec;
use log::{info, warn};
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp;
use smoltcp::wire::{IpAddress, IpCidr, IpEndpoint, Ipv4Address, Ipv4Cidr};
use touchos_setupnet::{ADDR, PREFIX, dhcp, dns};
use touchos_rt::time;

pub struct Setup {
    dhcp_udp: SocketHandle,
    dns_udp: SocketHandle,
    dhcp: dhcp::Server,
}

fn socket(slots: usize, bytes: usize) -> udp::Socket<'static> {
    udp::Socket::new(
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; slots], vec![0; bytes]),
        udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; slots], vec![0; bytes]),
    )
}

impl Setup {
    /// Take the setup address and open the DHCP and DNS ports.
    pub fn start(iface: &mut Interface, sockets: &mut SocketSet<'static>) -> Option<Setup> {
        iface.update_ip_addrs(|a| {
            a.clear();
            let _ = a.push(IpCidr::Ipv4(Ipv4Cidr::new(Ipv4Address::from(ADDR), PREFIX)));
        });
        iface.routes_mut().remove_default_ipv4_route();
        let mut d = socket(4, 4 * 1024);
        let mut n = socket(8, 8 * 1024);
        if d.bind(dhcp::SERVER_PORT).is_err() || n.bind(dns::PORT).is_err() {
            warn!("setup network: can't open the DHCP or DNS port");
            return None;
        }
        info!("setup network: 192.168.4.1/24, DHCP for .{}-.{}, every DNS name answered with us", dhcp::FIRST, dhcp::LAST);
        Some(Setup { dhcp_udp: sockets.add(d), dns_udp: sockets.add(n), dhcp: dhcp::Server::new(ADDR) })
    }

    pub fn cidr() -> Ipv4Cidr {
        Ipv4Cidr::new(Ipv4Address::from(ADDR), PREFIX)
    }

    /// Answer what arrived.
    pub fn pump(&mut self, sockets: &mut SocketSet<'static>) {
        let now = time::now_ms();
        let mut buf = [0u8; 1500];
        let udp = sockets.get_mut::<udp::Socket>(self.dhcp_udp);
        loop {
            let n = match udp.recv_slice(&mut buf) {
                Ok((n, _)) => n,
                Err(udp::RecvError::Truncated) => continue,
                Err(udp::RecvError::Exhausted) => break,
            };
            let (reply, ev) = self.dhcp.on_packet(&buf[..n], now);
            match ev {
                Some(dhcp::Event::Leased(mac, ip)) => {
                    info!("setup network: {}.{}.{}.{} for {mac:02x?}", ip[0], ip[1], ip[2], ip[3]);
                }
                Some(dhcp::Event::Released(mac)) => info!("setup network: {mac:02x?} gave its address back"),
                None => {}
            }
            if let Some(r) = reply {
                let to = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), dhcp::CLIENT_PORT);
                let _ = udp.send_slice(&r, to);
            }
        }
        let udp = sockets.get_mut::<udp::Socket>(self.dns_udp);
        loop {
            let (n, meta) = match udp.recv_slice(&mut buf) {
                Ok((n, m)) => (n, m),
                Err(udp::RecvError::Truncated) => continue,
                Err(udp::RecvError::Exhausted) => break,
            };
            if let Some(r) = dns::answer(&buf[..n], ADDR) {
                let _ = udp.send_slice(&r, meta.endpoint);
            }
        }
    }
}
