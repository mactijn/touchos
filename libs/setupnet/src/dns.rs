//! The setup network's DNS server: every A query is answered with the
//! panel's address, so whatever a phone looks up (its captive-portal
//! check first) leads to the settings page. AAAA and other types get an
//! empty answer, so clients fall back to IPv4 at once.

use alloc::vec::Vec;
use touchos_mdns::wire::{self, Question, Record, Writer};

pub const PORT: u16 = 53;
/// Short: once set up, the phone's next lookups should go to its real DNS.
pub const TTL: u32 = 10;

const OPCODE_MASK: u16 = 0x7800;
const AA: u16 = 0x0400;
const RD: u16 = 0x0100;
const RA: u16 = 0x0080;
const RCODE_NOTIMP: u16 = 4;

/// The answer to a query (a UDP payload to port 53), with `addr` for every
/// A question; None for what isn't a query.
pub fn answer(query: &[u8], addr: [u8; 4]) -> Option<Vec<u8>> {
    let m = wire::parse(query)?;
    if m.flags & wire::QR != 0 {
        return None;
    }
    let opcode = m.flags & OPCODE_MASK;
    let mut flags = wire::QR | opcode | AA | (m.flags & RD) | RA;
    if opcode != 0 {
        flags |= RCODE_NOTIMP;
    }
    let mut w = Writer::new(m.id, flags);
    for q in &m.questions {
        w.question(&Question { unicast: false, ..q.clone() });
    }
    if opcode == 0 {
        for q in m.questions.iter().filter(|q| q.qtype == wire::TYPE_A && q.class == wire::CLASS_IN) {
            w.record(
                1,
                &Record {
                    name: q.name.clone(),
                    rtype: wire::TYPE_A,
                    class: wire::CLASS_IN,
                    flush: false,
                    ttl: TTL,
                    rdata: addr.to_vec(),
                },
            );
        }
    }
    Some(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const US: [u8; 4] = [192, 168, 4, 1];

    fn name(s: &str) -> wire::Name {
        s.split('.').map(|l| l.as_bytes().to_vec()).collect()
    }

    fn query(id: u16, flags: u16, qs: &[(&str, u16)]) -> Vec<u8> {
        let mut w = Writer::new(id, flags);
        for (n, t) in qs {
            w.question(&Question { name: name(n), qtype: *t, class: wire::CLASS_IN, unicast: false });
        }
        w.finish()
    }

    #[test]
    fn every_a_query_is_us() {
        let a = answer(&query(0x4242, RD, &[("connectivitycheck.gstatic.com", wire::TYPE_A)]), US).unwrap();
        let m = wire::parse(&a).unwrap();
        assert_eq!(m.id, 0x4242);
        assert_eq!(m.flags, wire::QR | AA | RD | RA);
        assert_eq!(m.questions.len(), 1);
        assert_eq!(m.answers.len(), 1);
        let r = &m.answers[0];
        assert!(wire::name_eq(&r.name, &name("connectivitycheck.gstatic.com")));
        assert_eq!((r.rtype, r.class, r.ttl, &r.rdata[..]), (wire::TYPE_A, wire::CLASS_IN, TTL, &US[..]));
    }

    #[test]
    fn aaaa_and_others_are_empty() {
        let a = answer(&query(1, 0, &[("captive.apple.com", wire::TYPE_AAAA), ("x", 16)]), US).unwrap();
        let m = wire::parse(&a).unwrap();
        assert_eq!((m.questions.len(), m.answers.len()), (2, 0));
        assert_eq!(m.flags & 0x000f, 0, "NOERROR");
    }

    #[test]
    fn not_a_query() {
        // A response, junk, and another opcode (an update): refused or
        // not answered.
        assert!(answer(&query(1, wire::QR, &[("a", wire::TYPE_A)]), US).is_none());
        assert!(answer(&[1, 2, 3], US).is_none());
        let a = answer(&query(1, 5 << 11, &[("a", wire::TYPE_A)]), US).unwrap();
        let m = wire::parse(&a).unwrap();
        assert_eq!((m.flags & 0x000f, m.answers.len()), (RCODE_NOTIMP, 0));
        assert_eq!(vec![m.flags & OPCODE_MASK], vec![5 << 11]);
    }
}
