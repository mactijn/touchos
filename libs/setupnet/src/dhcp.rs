//! A DHCP server for the setup network (RFC 2131, the parts a phone or a
//! laptop uses): DISCOVER → OFFER, REQUEST → ACK or NAK, RELEASE,
//! DECLINE and INFORM. It leases 192.168.4.10-50 with the panel as router
//! and DNS server. Replies go to the broadcast address (port 68): a client
//! without an address can't answer ARP.

use alloc::vec::Vec;

pub const SERVER_PORT: u16 = 67;
pub const CLIENT_PORT: u16 = 68;

/// The addresses handed out: x.x.x.FIRST to x.x.x.LAST.
pub const FIRST: u8 = 10;
pub const LAST: u8 = 50;
/// The lease: one hour (setup takes minutes).
pub const LEASE_S: u32 = 3600;

const MAGIC: [u8; 4] = [99, 130, 83, 99];
/// A BOOTP message without options.
const FIXED: usize = 236;

// Message types (option 53).
pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const DECLINE: u8 = 4;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;
pub const RELEASE: u8 = 7;
pub const INFORM: u8 = 8;

/// The parts of a client's message the server uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub kind: u8,
    pub xid: u32,
    pub flags: u16,
    pub ciaddr: [u8; 4],
    pub chaddr: [u8; 6],
    pub requested: Option<[u8; 4]>,
    pub server_id: Option<[u8; 4]>,
}

/// A client's message (BOOTREQUEST, Ethernet, the magic cookie, a message
/// type); None for anything else.
pub fn parse(b: &[u8]) -> Option<Request> {
    if b.len() < FIXED + 4 || b[0] != 1 || b[1] != 1 || b[2] != 6 || b[FIXED..FIXED + 4] != MAGIC {
        return None;
    }
    let ip = |o: usize| -> [u8; 4] { b[o..o + 4].try_into().unwrap() };
    let mut r = Request {
        kind: 0,
        xid: u32::from_be_bytes(ip(4)),
        flags: u16::from_be_bytes([b[10], b[11]]),
        ciaddr: ip(12),
        chaddr: b[28..34].try_into().unwrap(),
        requested: None,
        server_id: None,
    };
    let mut o = &b[FIXED + 4..];
    while let Some(&code) = o.first() {
        match code {
            0 => {
                o = &o[1..];
                continue;
            }
            255 => break,
            _ => {}
        }
        let len = *o.get(1)? as usize;
        let v = o.get(2..2 + len)?;
        match (code, len) {
            (53, 1) => r.kind = v[0],
            (50, 4) => r.requested = Some(v.try_into().unwrap()),
            (54, 4) => r.server_id = Some(v.try_into().unwrap()),
            _ => {}
        }
        o = &o[2 + len..];
    }
    (r.kind != 0).then_some(r)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lease {
    mac: [u8; 6],
    host: u8,
    /// Offered but not yet requested: freed again soon.
    offered: bool,
    until_ms: u64,
}

/// How long an offer holds its address for the client's REQUEST.
const OFFER_MS: u64 = 30_000;

pub struct Server {
    /// Ours (router and DNS server too).
    addr: [u8; 4],
    leases: Vec<Lease>,
}

/// What [`Server::on_packet`] did, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Leased([u8; 6], [u8; 4]),
    Released([u8; 6]),
}

impl Server {
    pub fn new(addr: [u8; 4]) -> Server {
        Server { addr, leases: Vec::new() }
    }

    fn ip(&self, host: u8) -> [u8; 4] {
        [self.addr[0], self.addr[1], self.addr[2], host]
    }

    /// The host part of `ip`, if it is one of ours to lease.
    fn ours(&self, ip: [u8; 4]) -> Option<u8> {
        (ip[..3] == self.addr[..3] && (FIRST..=LAST).contains(&ip[3])).then_some(ip[3])
    }

    fn free(&self, host: u8, mac: &[u8; 6]) -> bool {
        self.leases.iter().all(|l| l.host != host || l.mac == *mac)
    }

    /// The address for `mac`: the one it has, the one it asks for if free,
    /// or the first free one.
    fn pick(&self, mac: &[u8; 6], wish: Option<[u8; 4]>) -> Option<u8> {
        if let Some(l) = self.leases.iter().find(|l| l.mac == *mac) {
            return Some(l.host);
        }
        if let Some(h) = wish.and_then(|w| self.ours(w)).filter(|&h| self.free(h, mac)) {
            return Some(h);
        }
        (FIRST..=LAST).find(|&h| self.free(h, mac))
    }

    fn hold(&mut self, mac: [u8; 6], host: u8, offered: bool, until_ms: u64) {
        self.leases.retain(|l| l.mac != mac);
        self.leases.push(Lease { mac, host, offered, until_ms });
    }

    /// A message from a client (UDP to port 67): the reply to broadcast to
    /// port 68, if any.
    pub fn on_packet(&mut self, data: &[u8], now_ms: u64) -> (Option<Vec<u8>>, Option<Event>) {
        self.leases.retain(|l| l.until_ms > now_ms);
        let Some(r) = parse(data) else { return (None, None) };
        match r.kind {
            DISCOVER => match self.pick(&r.chaddr, r.requested) {
                Some(h) => {
                    if !self.leases.iter().any(|l| l.mac == r.chaddr && !l.offered) {
                        self.hold(r.chaddr, h, true, now_ms + OFFER_MS);
                    }
                    (Some(self.reply(&r, OFFER, self.ip(h))), None)
                }
                None => (None, None), // full: nothing to offer
            },
            REQUEST => {
                // An answer to another server's offer: not ours.
                if r.server_id.is_some_and(|s| s != self.addr) {
                    self.leases.retain(|l| !(l.mac == r.chaddr && l.offered));
                    return (None, None);
                }
                let wanted = r.requested.unwrap_or(r.ciaddr);
                match self.ours(wanted).filter(|&h| self.free(h, &r.chaddr)) {
                    Some(h) => {
                        self.hold(r.chaddr, h, false, now_ms + LEASE_S as u64 * 1000);
                        (Some(self.reply(&r, ACK, self.ip(h))), Some(Event::Leased(r.chaddr, self.ip(h))))
                    }
                    // Not an address of ours (left over from another
                    // network) or someone else's: start over.
                    None => (Some(self.reply(&r, NAK, [0; 4])), None),
                }
            }
            RELEASE | DECLINE => {
                let before = self.leases.len();
                self.leases.retain(|l| l.mac != r.chaddr);
                let ev = (self.leases.len() != before).then_some(Event::Released(r.chaddr));
                (None, ev)
            }
            INFORM => (Some(self.reply(&r, ACK, [0; 4])), None),
            _ => (None, None),
        }
    }

    /// Addresses leased (not just offered) now.
    pub fn leases(&self) -> usize {
        self.leases.iter().filter(|l| !l.offered).count()
    }

    fn reply(&self, r: &Request, kind: u8, yiaddr: [u8; 4]) -> Vec<u8> {
        let mut b = alloc::vec![0u8; FIXED];
        b[0] = 2; // BOOTREPLY
        b[1] = 1;
        b[2] = 6;
        b[4..8].copy_from_slice(&r.xid.to_be_bytes());
        b[10..12].copy_from_slice(&r.flags.to_be_bytes());
        if kind == ACK && yiaddr == [0; 4] {
            b[12..16].copy_from_slice(&r.ciaddr); // INFORM
        }
        b[16..20].copy_from_slice(&yiaddr);
        b[28..34].copy_from_slice(&r.chaddr);
        b.extend_from_slice(&MAGIC);
        let mut opt = |code: u8, v: &[u8]| {
            b.push(code);
            b.push(v.len() as u8);
            b.extend_from_slice(v);
        };
        opt(53, &[kind]);
        opt(54, &self.addr);
        if kind != NAK {
            if yiaddr != [0; 4] {
                opt(51, &LEASE_S.to_be_bytes());
                opt(58, &(LEASE_S / 2).to_be_bytes());
                opt(59, &(LEASE_S / 8 * 7).to_be_bytes());
            }
            opt(1, &[255, 255, 255, 0]);
            opt(3, &self.addr);
            opt(6, &self.addr);
        }
        b.push(255);
        // BOOTP's minimum: some clients drop shorter replies.
        if b.len() < 300 {
            b.resize(300, 0);
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: [u8; 4] = [192, 168, 4, 1];
    const PHONE: [u8; 6] = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0x01];

    fn msg(kind: u8, mac: [u8; 6], ciaddr: [u8; 4], requested: Option<[u8; 4]>, server: Option<[u8; 4]>) -> Vec<u8> {
        let mut b = alloc::vec![0u8; FIXED];
        b[0] = 1;
        b[1] = 1;
        b[2] = 6;
        b[4..8].copy_from_slice(&0x1234_5678u32.to_be_bytes());
        b[10] = 0x80; // broadcast flag
        b[12..16].copy_from_slice(&ciaddr);
        b[28..34].copy_from_slice(&mac);
        b.extend_from_slice(&MAGIC);
        b.extend_from_slice(&[53, 1, kind]);
        b.push(0); // a pad byte
        if let Some(r) = requested {
            b.extend_from_slice(&[50, 4]);
            b.extend_from_slice(&r);
        }
        if let Some(s) = server {
            b.extend_from_slice(&[54, 4]);
            b.extend_from_slice(&s);
        }
        b.extend_from_slice(&[55, 3, 1, 3, 6, 255]);
        b
    }

    /// (message type, yiaddr, options as (code, value)).
    fn read(b: &[u8]) -> (u8, [u8; 4], Vec<(u8, Vec<u8>)>) {
        assert_eq!((b[0], b[1], b[2]), (2, 1, 6));
        assert_eq!(&b[4..8], &0x1234_5678u32.to_be_bytes());
        assert_eq!(&b[236..240], &MAGIC);
        assert!(b.len() >= 300);
        let mut opts = Vec::new();
        let mut o = &b[240..];
        while o[0] != 255 {
            opts.push((o[0], o[2..2 + o[1] as usize].to_vec()));
            o = &o[2 + o[1] as usize..];
        }
        let kind = opts.iter().find(|(c, _)| *c == 53).unwrap().1[0];
        (kind, b[16..20].try_into().unwrap(), opts)
    }

    fn opt(opts: &[(u8, Vec<u8>)], code: u8) -> Option<&[u8]> {
        opts.iter().find(|(c, _)| *c == code).map(|(_, v)| v.as_slice())
    }

    #[test]
    fn discover_request_ack() {
        let mut s = Server::new(US);
        let (offer, ev) = s.on_packet(&msg(DISCOVER, PHONE, [0; 4], None, None), 0);
        assert_eq!(ev, None);
        let (kind, ip, opts) = read(&offer.unwrap());
        assert_eq!((kind, ip), (OFFER, [192, 168, 4, 10]));
        assert_eq!(opt(&opts, 54), Some(&US[..]));
        assert_eq!(opt(&opts, 1), Some(&[255, 255, 255, 0][..]));
        assert_eq!(opt(&opts, 3), Some(&US[..]));
        assert_eq!(opt(&opts, 6), Some(&US[..]));
        assert_eq!(opt(&opts, 51), Some(&3600u32.to_be_bytes()[..]));
        assert_eq!(s.leases(), 0);

        let (ack, ev) = s.on_packet(&msg(REQUEST, PHONE, [0; 4], Some(ip), Some(US)), 100);
        let (kind, ip2, _) = read(&ack.unwrap());
        assert_eq!((kind, ip2), (ACK, ip));
        assert_eq!(ev, Some(Event::Leased(PHONE, ip)));
        assert_eq!(s.leases(), 1);

        // Renewing (unicast, ciaddr set, no server id): ACK again.
        let (ack, _) = s.on_packet(&msg(REQUEST, PHONE, ip, None, None), 1_000_000);
        assert_eq!(read(&ack.unwrap()).0, ACK);
        // The same client discovering again gets the same address.
        let (offer, _) = s.on_packet(&msg(DISCOVER, PHONE, [0; 4], None, None), 1_000_001);
        assert_eq!(read(&offer.unwrap()).1, ip);
    }

    #[test]
    fn addresses_are_unique_and_run_out() {
        let mut s = Server::new(US);
        let mut got = Vec::new();
        for i in 0..=(LAST - FIRST) {
            let mac = [2, 0, 0, 0, 0, i];
            let (o, _) = s.on_packet(&msg(DISCOVER, mac, [0; 4], None, None), 0);
            let ip = read(&o.unwrap()).1;
            assert!(!got.contains(&ip));
            got.push(ip);
            s.on_packet(&msg(REQUEST, mac, [0; 4], Some(ip), Some(US)), 0);
        }
        assert_eq!(s.leases(), 41);
        assert_eq!(s.on_packet(&msg(DISCOVER, [2, 0, 0, 0, 1, 0], [0; 4], None, None), 0).0, None);
        // A lease runs out after an hour, and its address is free again.
        let (o, _) = s.on_packet(&msg(DISCOVER, [2, 0, 0, 0, 1, 0], [0; 4], None, None), 3_600_001);
        assert!(o.is_some());
    }

    #[test]
    fn a_wish_is_granted_when_free() {
        let mut s = Server::new(US);
        let (o, _) = s.on_packet(&msg(DISCOVER, PHONE, [0; 4], Some([192, 168, 4, 33]), None), 0);
        assert_eq!(read(&o.unwrap()).1, [192, 168, 4, 33]);
        // Another network's address: the first free one instead.
        let (o, _) = s.on_packet(&msg(DISCOVER, [2; 6], [0; 4], Some([10, 0, 0, 7]), None), 0);
        assert_eq!(read(&o.unwrap()).1, [192, 168, 4, 10]);
    }

    #[test]
    fn nak_and_other_servers() {
        let mut s = Server::new(US);
        // A phone that remembers its home network's address.
        let (nak, _) = s.on_packet(&msg(REQUEST, PHONE, [0; 4], Some([10, 0, 42, 99]), None), 0);
        let (kind, ip, opts) = read(&nak.unwrap());
        assert_eq!((kind, ip), (NAK, [0; 4]));
        assert!(opt(&opts, 3).is_none());
        // Someone else's address.
        s.on_packet(&msg(DISCOVER, PHONE, [0; 4], None, None), 0);
        s.on_packet(&msg(REQUEST, PHONE, [0; 4], Some([192, 168, 4, 10]), Some(US)), 0);
        let (nak, _) = s.on_packet(&msg(REQUEST, [2; 6], [0; 4], Some([192, 168, 4, 10]), None), 0);
        assert_eq!(read(&nak.unwrap()).0, NAK);
        // A REQUEST for another server's offer: silence.
        assert_eq!(s.on_packet(&msg(REQUEST, [3; 6], [0; 4], Some([192, 168, 4, 11]), Some([192, 168, 4, 2])), 0).0, None);
    }

    #[test]
    fn release_frees_the_address() {
        let mut s = Server::new(US);
        s.on_packet(&msg(DISCOVER, PHONE, [0; 4], None, None), 0);
        s.on_packet(&msg(REQUEST, PHONE, [0; 4], Some([192, 168, 4, 10]), Some(US)), 0);
        let (r, ev) = s.on_packet(&msg(RELEASE, PHONE, [192, 168, 4, 10], None, Some(US)), 5);
        assert_eq!((r, ev), (None, Some(Event::Released(PHONE))));
        assert_eq!(s.leases(), 0);
    }

    #[test]
    fn inform_and_junk() {
        let mut s = Server::new(US);
        let (ack, _) = s.on_packet(&msg(INFORM, PHONE, [192, 168, 4, 77], None, None), 0);
        let b = ack.unwrap();
        let (kind, ip, opts) = read(&b);
        assert_eq!((kind, ip, &b[12..16]), (ACK, [0; 4], &[192, 168, 4, 77][..]));
        assert!(opt(&opts, 51).is_none(), "no lease for INFORM");
        assert_eq!(s.on_packet(&[0; 10], 0), (None, None));
        let mut reply = msg(DISCOVER, PHONE, [0; 4], None, None);
        reply[0] = 2; // a BOOTREPLY (another server's)
        assert_eq!(s.on_packet(&reply, 0), (None, None));
        // Options cut short.
        let mut cut = msg(DISCOVER, PHONE, [0; 4], None, None);
        cut.truncate(FIXED + 4 + 2);
        assert_eq!(parse(&cut), None);
    }
}
