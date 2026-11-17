//! An mDNS responder (RFC 6762) with DNS-SD services (RFC 6763), without
//! I/O: the caller feeds it the datagrams that arrive on port 5353 and
//! sends what it returns. It answers for our name (`<host>.local`, its
//! address, the reverse name) and the services registered with it; it
//! doesn't ask questions or keep a cache. IPv4 only.
//!
//! Before it answers for a name it probes for it (three queries, 250 ms
//! apart), then announces it twice, a second apart (RFC 6762 §8). Another
//! host that answers for our name with another address is a conflict: the
//! name gets `-2` (then `-3`, ...) and is probed again (§9). A host
//! probing for the same name at the same time is resolved by comparing the
//! records both probe with (§8.2).
//!
//! `no_std` + `alloc`; times are milliseconds on any monotonic clock.

#![no_std]

extern crate alloc;

pub mod wire;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use wire::{
    CLASS_ANY, CLASS_IN, FLAGS_RESPONSE, Message, Name, Question, Record, TYPE_A, TYPE_AAAA,
    TYPE_ANY, TYPE_NSEC, TYPE_PTR, TYPE_SRV, TYPE_TXT, Writer, canonical, name_eq, put_name,
};

/// The mDNS port and IPv4 group.
pub const PORT: u16 = 5353;
pub const GROUP: [u8; 4] = [224, 0, 0, 251];
/// The group's Ethernet address.
pub const GROUP_MAC: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];

/// TTLs (RFC 6762 §10): records with a host name in them, and the rest.
const TTL_HOST: u32 = 120;
const TTL_OTHER: u32 = 4500;
/// The most a legacy unicast answer may say (§6.7).
const TTL_LEGACY: u32 = 10;

const PROBES: u8 = 3;
const PROBE_GAP_MS: u64 = 250;
const ANNOUNCEMENTS: u8 = 2;
const ANNOUNCE_GAP_MS: u64 = 1000;
/// After losing a simultaneous probe (§8.2).
const LOST_TIEBREAK_MS: u64 = 1000;
/// After 15 conflicts within 10 s, wait 5 s before probing again (§8.1).
const CONFLICT_WINDOW_MS: u64 = 10_000;
const CONFLICT_LIMIT: u32 = 15;
const CONFLICT_PAUSE_MS: u64 = 5000;
/// Leave room for the IP and UDP headers in a 1500-byte frame.
const MAX_PACKET: usize = 1400;

/// Where a packet goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    /// 224.0.0.251:5353.
    Multicast,
    Unicast([u8; 4], u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub dest: Dest,
    pub data: Vec<u8>,
}

/// A service to announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    /// `_http._tcp`.
    pub kind: String,
    /// One label ("Kitchen panel"); empty = the host name.
    pub instance: String,
    pub port: u16,
    /// `key=value` strings.
    pub txt: Vec<String>,
}

impl Service {
    /// Whether this is a usable service: a type of two labels
    /// (`_name._tcp` or `_name._udp`), at most 63-byte labels, TXT strings
    /// of at most 255 bytes.
    pub fn is_valid(&self) -> bool {
        let mut parts = self.kind.split('.');
        let (Some(name), Some(proto), None) = (parts.next(), parts.next(), parts.next()) else {
            return false;
        };
        name.len() >= 2
            && name.len() <= 16
            && name.starts_with('_')
            && (proto == "_tcp" || proto == "_udp")
            && self.instance.len() <= 63
            && self.txt.iter().all(|t| !t.is_empty() && t.len() <= 255)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No address: nothing to say.
    Idle,
    /// `sent` probes went out; the next (or the end of probing) is at `at`.
    Probing { sent: u8, at: u64 },
    Announcing { sent: u8, at: u64 },
    Ready,
}

pub struct Responder {
    /// The name we want (`net.hostname` or the default).
    base: String,
    /// 1 = `base` itself; n = `base-n` after conflicts.
    suffix: u32,
    host: String,
    ip: Option<[u8; 4]>,
    services: Vec<Service>,
    phase: Phase,
    out: VecDeque<Packet>,
    /// Conflicts in the current window: (window start, count).
    conflicts: (u64, u32),
}

fn label(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn local(labels: &[&str]) -> Name {
    labels.iter().map(|l| label(l)).chain([label("local")]).collect()
}

impl Responder {
    pub fn new(host: &str) -> Responder {
        Responder {
            base: host.into(),
            suffix: 1,
            host: host.into(),
            ip: None,
            services: Vec::new(),
            phase: Phase::Idle,
            out: VecDeque::new(),
            conflicts: (0, 0),
        }
    }

    /// The name we answer to (without `.local`).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Whether the name is ours: probed without a conflict.
    pub fn is_ready(&self) -> bool {
        matches!(self.phase, Phase::Announcing { .. } | Phase::Ready)
    }

    /// A new name (the setting changed): probe for it.
    pub fn set_host(&mut self, host: &str, now: u64) {
        if host == self.base {
            return;
        }
        if self.is_ready() {
            self.out.push_back(self.goodbye_host());
        }
        self.base = host.into();
        self.suffix = 1;
        self.host = host.into();
        self.conflicts = (now, 0);
        self.restart(now);
    }

    /// Our address (None: we have none). A new one is probed and announced.
    pub fn set_ip(&mut self, ip: Option<[u8; 4]>, now: u64) {
        if ip == self.ip {
            return;
        }
        self.ip = ip;
        self.restart(now);
    }

    /// Announce a service; one with the same type and port is replaced.
    pub fn add_service(&mut self, s: Service, now: u64) {
        self.services.retain(|x| !(x.kind == s.kind && x.port == s.port));
        self.services.push(s);
        if self.is_ready() {
            self.phase = Phase::Announcing { sent: 0, at: now };
        }
    }

    /// Stop announcing a service, and tell the others it's gone.
    pub fn remove_service(&mut self, kind: &str, port: u16) {
        let Some(i) = self.services.iter().position(|x| x.kind == kind && x.port == port) else {
            return;
        };
        let s = self.services.remove(i);
        if self.is_ready() {
            let mut w = Writer::new(0, FLAGS_RESPONSE);
            let mut ptr = self.service_ptr(&s);
            ptr.ttl = 0;
            w.record(1, &ptr);
            self.out.push_back(Packet { dest: Dest::Multicast, data: w.finish() });
        }
    }

    pub fn services(&self) -> &[Service] {
        &self.services
    }

    fn restart(&mut self, now: u64) {
        // Probes start after 0-250 ms (§8.1), so that panels powered up
        // together don't probe in step; the name and address make the delay
        // differ between them.
        let jitter = self
            .host
            .bytes()
            .chain(self.ip.unwrap_or_default())
            .fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
        self.phase = match self.ip {
            Some(_) => Phase::Probing { sent: 0, at: now + (jitter % 250) as u64 },
            None => Phase::Idle,
        };
    }

    /// When `poll` has something to send next.
    pub fn poll_at(&self) -> Option<u64> {
        if !self.out.is_empty() {
            return Some(0);
        }
        match self.phase {
            Phase::Probing { at, .. } | Phase::Announcing { at, .. } => Some(at),
            Phase::Idle | Phase::Ready => None,
        }
    }

    /// The next probe or announcement that is due.
    pub fn poll(&mut self, now: u64) -> Option<Packet> {
        if let Some(p) = self.out.pop_front() {
            return Some(p);
        }
        match self.phase {
            Phase::Probing { sent, at } if now >= at => {
                if sent == PROBES {
                    self.phase = Phase::Announcing { sent: 0, at: now };
                    return self.poll(now);
                }
                self.phase = Phase::Probing { sent: sent + 1, at: now + PROBE_GAP_MS };
                Some(self.probe())
            }
            Phase::Announcing { sent, at } if now >= at => {
                self.phase = if sent + 1 == ANNOUNCEMENTS {
                    Phase::Ready
                } else {
                    Phase::Announcing { sent: sent + 1, at: now + ANNOUNCE_GAP_MS }
                };
                Some(self.announcement())
            }
            _ => None,
        }
    }

    /// A datagram that arrived on port 5353 from `src`: maybe an answer.
    pub fn on_packet(&mut self, src: [u8; 4], src_port: u16, data: &[u8], now: u64) -> Option<Packet> {
        // Our own packets, which the access point sends back to us.
        if self.ip == Some(src) || self.ip.is_none() {
            return None;
        }
        let m = wire::parse(data)?;
        if !m.is_standard() {
            return None;
        }
        if m.is_response() {
            self.check_conflict(&m, now);
            return None;
        }
        if let Phase::Probing { .. } = self.phase {
            self.check_tiebreak(&m, now);
            return None;
        }
        if !self.is_ready() {
            return None;
        }
        self.answer(&m, src, src_port)
    }

    fn host_name(&self) -> Name {
        local(&[&self.host])
    }

    fn a_record(&self, ip: [u8; 4]) -> Record {
        Record {
            name: self.host_name(),
            rtype: TYPE_A,
            class: CLASS_IN,
            flush: true,
            ttl: TTL_HOST,
            rdata: ip.to_vec(),
        }
    }

    fn service_type_name(s: &Service) -> Name {
        let mut parts: Vec<&str> = s.kind.split('.').collect();
        parts.push("local");
        parts.into_iter().map(label).collect()
    }

    fn instance_name(&self, s: &Service) -> Name {
        let inst = if s.instance.is_empty() { &self.host } else { &s.instance };
        let mut n = vec![label(inst)];
        n.extend(Self::service_type_name(s));
        n
    }

    fn service_ptr(&self, s: &Service) -> Record {
        let mut rdata = Vec::new();
        put_name(&mut rdata, &self.instance_name(s));
        Record {
            name: Self::service_type_name(s),
            rtype: TYPE_PTR,
            class: CLASS_IN,
            flush: false,
            ttl: TTL_OTHER,
            rdata,
        }
    }

    /// Every record we answer for, with which records go along with it as
    /// additional records (indices into the same list).
    fn records(&self) -> Vec<(Record, Vec<usize>)> {
        let Some(ip) = self.ip else {
            return Vec::new();
        };
        let host = self.host_name();
        let mut host_rdata = Vec::new();
        put_name(&mut host_rdata, &host);
        let mut nsec = host_rdata.clone();
        nsec.extend_from_slice(&[0, 1, 0x40]); // only an A record (type 1)
        let reverse: Name = ip
            .iter()
            .rev()
            .map(|b| format!("{b}").into_bytes())
            .chain([label("in-addr"), label("arpa")])
            .collect();
        let rec = |name: Name, rtype, flush, ttl, rdata| Record { name, rtype, class: CLASS_IN, flush, ttl, rdata };
        // 0: A, 1: NSEC, 2: the reverse PTR.
        let mut v = vec![
            (self.a_record(ip), vec![1]),
            (rec(host.clone(), TYPE_NSEC, true, TTL_HOST, nsec), vec![]),
            (rec(reverse, TYPE_PTR, true, TTL_HOST, host_rdata.clone()), vec![0]),
        ];
        let mut kinds: Vec<&str> = Vec::new();
        for s in &self.services {
            let at = v.len();
            let inst = self.instance_name(s);
            let mut srv = Vec::new();
            srv.extend_from_slice(&[0, 0, 0, 0]);
            srv.extend_from_slice(&s.port.to_be_bytes());
            srv.extend_from_slice(&host_rdata);
            let mut txt = Vec::new();
            for t in &s.txt {
                txt.push(t.len() as u8);
                txt.extend_from_slice(t.as_bytes());
            }
            if txt.is_empty() {
                txt.push(0); // an empty TXT record is one empty string
            }
            v.push((self.service_ptr(s), vec![at + 1, at + 2, 0, 1]));
            v.push((rec(inst.clone(), TYPE_SRV, true, TTL_HOST, srv), vec![0, 1]));
            v.push((rec(inst, TYPE_TXT, true, TTL_OTHER, txt), vec![]));
            if !kinds.contains(&s.kind.as_str()) {
                kinds.push(&s.kind);
            }
        }
        for k in kinds {
            let s = self.services.iter().find(|s| s.kind == k).unwrap();
            let mut rdata = Vec::new();
            put_name(&mut rdata, &Self::service_type_name(s));
            v.push((rec(local(&["_services", "_dns-sd", "_udp"]), TYPE_PTR, false, TTL_OTHER, rdata), vec![]));
        }
        v
    }

    fn probe(&self) -> Packet {
        let mut w = Writer::new(0, 0);
        w.question(&Question { name: self.host_name(), qtype: TYPE_ANY, class: CLASS_IN, unicast: true });
        if let Some(ip) = self.ip {
            let mut a = self.a_record(ip);
            a.flush = false;
            w.record(2, &a);
        }
        Packet { dest: Dest::Multicast, data: w.finish() }
    }

    fn announcement(&self) -> Packet {
        let mut w = Writer::new(0, FLAGS_RESPONSE);
        for (r, _) in self.records() {
            if w.buf.len() + r.rdata.len() + 64 > MAX_PACKET {
                break;
            }
            w.record(1, &r);
        }
        Packet { dest: Dest::Multicast, data: w.finish() }
    }

    fn goodbye_host(&self) -> Packet {
        let mut w = Writer::new(0, FLAGS_RESPONSE);
        if let Some(ip) = self.ip {
            let mut a = self.a_record(ip);
            a.ttl = 0;
            w.record(1, &a);
        }
        Packet { dest: Dest::Multicast, data: w.finish() }
    }

    fn answer(&self, m: &Message, src: [u8; 4], src_port: u16) -> Option<Packet> {
        let legacy = src_port != PORT;
        let records = self.records();
        let mut answers: Vec<usize> = Vec::new();
        for q in &m.questions {
            if q.class != CLASS_IN && q.class != CLASS_ANY {
                continue;
            }
            for (i, (r, _)) in records.iter().enumerate() {
                let fits = q.qtype == TYPE_ANY
                    || q.qtype == r.rtype
                    // No AAAA: the NSEC says so (§6.1).
                    || (q.qtype == TYPE_AAAA && r.rtype == TYPE_NSEC);
                if fits && name_eq(&q.name, &r.name) && !answers.contains(&i) {
                    answers.push(i);
                }
            }
        }
        // Known-answer suppression (§7.1): what the asker already has with
        // at least half its TTL left.
        answers.retain(|&i| {
            let r = &records[i].0;
            !m.answers.iter().any(|k| {
                k.rtype == r.rtype
                    && k.ttl >= r.ttl / 2
                    && name_eq(&k.name, &r.name)
                    && canonical(k.rtype, &k.rdata) == canonical(r.rtype, &r.rdata)
            })
        });
        if answers.is_empty() {
            return None;
        }
        let mut additional: Vec<usize> = Vec::new();
        for &i in &answers {
            for &j in &records[i].1 {
                if !answers.contains(&j) && !additional.contains(&j) {
                    additional.push(j);
                }
            }
        }
        let unicast = legacy || m.questions.iter().all(|q| q.unicast);
        let dest = match (legacy, unicast) {
            (true, _) => Dest::Unicast(src, src_port),
            (false, true) => Dest::Unicast(src, PORT),
            (false, false) => Dest::Multicast,
        };
        let mut w = Writer::new(if legacy { m.id } else { 0 }, FLAGS_RESPONSE);
        if legacy {
            for q in &m.questions {
                w.question(&Question { unicast: false, ..q.clone() });
            }
        }
        let fix = |r: &Record| -> Record {
            let mut r = r.clone();
            if legacy {
                r.flush = false;
                r.ttl = r.ttl.min(TTL_LEGACY);
            }
            r
        };
        for &i in &answers {
            w.record(1, &fix(&records[i].0));
        }
        for &j in &additional {
            let r = &records[j].0;
            if w.buf.len() + r.rdata.len() + 64 > MAX_PACKET {
                break;
            }
            w.record(3, &fix(r));
        }
        Some(Packet { dest, data: w.finish() })
    }

    /// A response that claims our name with another address (§9).
    fn check_conflict(&mut self, m: &Message, now: u64) {
        let (Some(ip), true) = (self.ip, self.phase != Phase::Idle) else {
            return;
        };
        let host = self.host_name();
        let conflict = m.answers.iter().chain(&m.additional).any(|r| {
            r.rtype == TYPE_A && r.ttl > 0 && name_eq(&r.name, &host) && r.rdata != ip
        });
        if conflict {
            self.rename(now);
        }
    }

    /// Another host probing for our name at the same time (§8.2): the one
    /// whose records sort later wins; the other waits a second and probes
    /// again (and then sees the winner's answer).
    fn check_tiebreak(&mut self, m: &Message, now: u64) {
        let host = self.host_name();
        if !m.questions.iter().any(|q| name_eq(&q.name, &host)) {
            return;
        }
        let key = |r: &Record| (r.class, r.rtype, canonical(r.rtype, &r.rdata));
        let mut theirs: Vec<_> = m.authority.iter().filter(|r| name_eq(&r.name, &host)).map(key).collect();
        if theirs.is_empty() {
            return;
        }
        theirs.sort();
        let ours = match self.ip {
            Some(ip) => vec![key(&self.a_record(ip))],
            None => return,
        };
        if theirs > ours {
            self.phase = Phase::Probing { sent: 0, at: now + LOST_TIEBREAK_MS };
        }
    }

    fn rename(&mut self, now: u64) {
        if now.saturating_sub(self.conflicts.0) > CONFLICT_WINDOW_MS {
            self.conflicts = (now, 0);
        }
        self.conflicts.1 += 1;
        self.suffix += 1;
        let tail = format!("-{}", self.suffix);
        let mut keep = self.base.len().min(63 - tail.len());
        while !self.base.is_char_boundary(keep) {
            keep -= 1;
        }
        self.host = format!("{}{tail}", &self.base[..keep]);
        self.restart(now);
        if self.conflicts.1 > CONFLICT_LIMIT
            && let Phase::Probing { at, .. } = &mut self.phase
        {
            *at += CONFLICT_PAUSE_MS;
        }
    }
}

#[cfg(test)]
mod tests;
