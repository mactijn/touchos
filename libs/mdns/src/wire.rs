//! DNS messages as mDNS uses them: parsing (with name compression) and
//! writing (without it; our packets are small).

use alloc::vec::Vec;

pub const TYPE_A: u16 = 1;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SRV: u16 = 33;
pub const TYPE_NSEC: u16 = 47;
pub const TYPE_ANY: u16 = 255;
pub const CLASS_IN: u16 = 1;
pub const CLASS_ANY: u16 = 255;
/// In a question: "unicast response wanted" (QU). In a record: "cache
/// flush", this is the whole set of records with this name and type.
pub const TOP_BIT: u16 = 0x8000;

/// Header flags: a response, authoritative.
pub const FLAGS_RESPONSE: u16 = 0x8400;
pub const QR: u16 = 0x8000;
const OPCODE: u16 = 0x7800;

/// A domain name as labels, without the root.
pub type Name = Vec<Vec<u8>>;

/// The most entries of one section we look at: a real query has a few.
const MAX_ENTRIES: usize = 64;

pub fn name_eq(a: &Name, b: &Name) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: Name,
    pub qtype: u16,
    pub class: u16,
    /// The QU bit: the asker wants a unicast answer.
    pub unicast: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: Name,
    pub rtype: u16,
    /// Without the cache-flush bit.
    pub class: u16,
    pub flush: bool,
    pub ttl: u32,
    /// With any names in it uncompressed.
    pub rdata: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub id: u16,
    pub flags: u16,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    pub authority: Vec<Record>,
    pub additional: Vec<Record>,
}

impl Message {
    pub fn is_response(&self) -> bool {
        self.flags & QR != 0
    }

    /// A standard query or response (opcode 0).
    pub fn is_standard(&self) -> bool {
        self.flags & OPCODE == 0
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(((self.u16()? as u32) << 16) | self.u16()? as u32)
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    /// A name, following compression pointers (only backwards, so a loop
    /// can't happen); ends after the name or its first pointer.
    fn name(&mut self) -> Option<Name> {
        let mut labels = Vec::new();
        let mut total = 0;
        let mut at = self.pos;
        let mut end = None;
        loop {
            let len = *self.b.get(at)? as usize;
            match len {
                0 => {
                    at += 1;
                    break;
                }
                1..=63 => {
                    let l = self.b.get(at + 1..at + 1 + len)?;
                    total += len + 1;
                    if total > 255 {
                        return None;
                    }
                    labels.push(l.to_vec());
                    at += 1 + len;
                }
                0xc0..=0xff => {
                    let target = ((len & 0x3f) << 8) | *self.b.get(at + 1)? as usize;
                    end.get_or_insert(at + 2);
                    if target >= at {
                        return None;
                    }
                    at = target;
                }
                _ => return None,
            }
        }
        self.pos = end.unwrap_or(at);
        Some(labels)
    }

    fn question(&mut self) -> Option<Question> {
        let name = self.name()?;
        let qtype = self.u16()?;
        let class = self.u16()?;
        Some(Question {
            name,
            qtype,
            class: class & !TOP_BIT,
            unicast: class & TOP_BIT != 0,
        })
    }

    fn record(&mut self) -> Option<Record> {
        let name = self.name()?;
        let rtype = self.u16()?;
        let class = self.u16()?;
        let ttl = self.u32()?;
        let len = self.u16()? as usize;
        let end = self.pos.checked_add(len).filter(|&e| e <= self.b.len())?;
        let mut rdata = Vec::new();
        match rtype {
            TYPE_PTR | TYPE_CNAME => put_name(&mut rdata, &self.name()?),
            TYPE_SRV => {
                rdata.extend_from_slice(self.bytes(6)?);
                put_name(&mut rdata, &self.name()?);
            }
            TYPE_NSEC => {
                put_name(&mut rdata, &self.name()?);
                rdata.extend_from_slice(self.b.get(self.pos..end)?);
            }
            _ => rdata.extend_from_slice(self.bytes(len)?),
        }
        if self.pos > end {
            return None;
        }
        self.pos = end;
        Some(Record {
            name,
            rtype,
            class: class & !TOP_BIT,
            flush: class & TOP_BIT != 0,
            ttl,
            rdata,
        })
    }
}

/// Parse a message. A broken question, answer or authority section makes
/// it unusable; a broken additional section is cut off (we don't need it).
pub fn parse(b: &[u8]) -> Option<Message> {
    let mut r = Reader { b, pos: 0 };
    let id = r.u16()?;
    let flags = r.u16()?;
    let counts = [r.u16()?, r.u16()?, r.u16()?, r.u16()?].map(|c| c as usize);
    if counts[..3].iter().any(|&c| c > MAX_ENTRIES) {
        return None;
    }
    let mut m = Message {
        id,
        flags,
        ..Message::default()
    };
    for _ in 0..counts[0] {
        m.questions.push(r.question()?);
    }
    for _ in 0..counts[1] {
        m.answers.push(r.record()?);
    }
    for _ in 0..counts[2] {
        m.authority.push(r.record()?);
    }
    for _ in 0..counts[3].min(MAX_ENTRIES) {
        match r.record() {
            Some(x) => m.additional.push(x),
            None => break,
        }
    }
    Some(m)
}

pub fn put_name(out: &mut Vec<u8>, name: &Name) {
    for l in name {
        out.push(l.len() as u8);
        out.extend_from_slice(l);
    }
    out.push(0);
}

/// The rdata in the form two records are compared in: names in
/// lowercase (they compare without case), everything else as it is.
pub fn canonical(rtype: u16, rdata: &[u8]) -> Vec<u8> {
    let mut v = rdata.to_vec();
    // A name's length bytes are below 64, so lowercasing them is harmless.
    let name_len = |b: &[u8]| {
        let mut i = 0;
        while let Some(&l) = b.get(i) {
            i += 1 + l as usize;
            if l == 0 {
                break;
            }
        }
        i.min(b.len())
    };
    match rtype {
        TYPE_PTR | TYPE_CNAME => v.make_ascii_lowercase(),
        TYPE_SRV if v.len() > 6 => v[6..].make_ascii_lowercase(),
        TYPE_NSEC => {
            let n = name_len(&v);
            v[..n].make_ascii_lowercase();
        }
        _ => {}
    }
    v
}

/// A message being written.
pub struct Writer {
    pub buf: Vec<u8>,
    counts: [u16; 4],
}

impl Writer {
    pub fn new(id: u16, flags: u16) -> Writer {
        let mut buf = Vec::with_capacity(512);
        buf.extend_from_slice(&id.to_be_bytes());
        buf.extend_from_slice(&flags.to_be_bytes());
        buf.extend_from_slice(&[0; 8]);
        Writer { buf, counts: [0; 4] }
    }

    pub fn question(&mut self, q: &Question) {
        put_name(&mut self.buf, &q.name);
        self.buf.extend_from_slice(&q.qtype.to_be_bytes());
        let class = q.class | if q.unicast { TOP_BIT } else { 0 };
        self.buf.extend_from_slice(&class.to_be_bytes());
        self.counts[0] += 1;
    }

    /// `section`: 1 answers, 2 authority, 3 additional (in this order).
    pub fn record(&mut self, section: usize, r: &Record) {
        put_name(&mut self.buf, &r.name);
        self.buf.extend_from_slice(&r.rtype.to_be_bytes());
        let class = r.class | if r.flush { TOP_BIT } else { 0 };
        self.buf.extend_from_slice(&class.to_be_bytes());
        self.buf.extend_from_slice(&r.ttl.to_be_bytes());
        self.buf.extend_from_slice(&(r.rdata.len() as u16).to_be_bytes());
        self.buf.extend_from_slice(&r.rdata);
        self.counts[section] += 1;
    }

    pub fn finish(mut self) -> Vec<u8> {
        for (i, c) in self.counts.iter().enumerate() {
            self.buf[4 + 2 * i..6 + 2 * i].copy_from_slice(&c.to_be_bytes());
        }
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn n(s: &str) -> Name {
        s.split('.').map(|l| l.as_bytes().to_vec()).collect()
    }

    #[test]
    fn query_with_compression() {
        // A query for _http._tcp.local PTR (QU) with a known answer whose
        // name and target point back into the question.
        let mut b = vec![0x12, 0x34, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0];
        b.extend_from_slice(b"\x05_http\x04_tcp\x05local\x00"); // at 12
        b.extend_from_slice(&[0, 12, 0x80, 1]);
        b.extend_from_slice(&[0xc0, 12, 0, 12, 0, 1, 0, 0, 0x11, 0x94, 0, 10]);
        b.extend_from_slice(b"\x07Kitchen\xc0\x0c");
        let m = parse(&b).unwrap();
        assert_eq!(m.id, 0x1234);
        assert!(!m.is_response() && m.is_standard());
        assert_eq!(
            m.questions,
            vec![Question { name: n("_http._tcp.local"), qtype: TYPE_PTR, class: CLASS_IN, unicast: true }]
        );
        let a = &m.answers[0];
        assert_eq!(a.name, n("_http._tcp.local"));
        assert_eq!(a.ttl, 4500);
        let mut want = Vec::new();
        put_name(&mut want, &n("Kitchen._http._tcp.local"));
        assert_eq!(a.rdata, want);
    }

    #[test]
    fn broken_messages() {
        assert_eq!(parse(&[0; 11]), None);
        // A pointer to itself, and one forward.
        let mut b = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0xc0, 12, 0, 1, 0, 1];
        assert_eq!(parse(&b), None);
        b[13] = 14;
        assert_eq!(parse(&b), None);
        // A label running past the end.
        assert_eq!(parse(&[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 5, b'a']), None);
        // Counts that promise more than there is.
        assert_eq!(parse(&[0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1]), None);
        // Rdata longer than the packet.
        let mut b = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0];
        b.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 120, 0, 9, 1, 2, 3, 4]);
        assert_eq!(parse(&b), None);
        // Too many questions.
        assert_eq!(parse(&[0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn writes_what_it_parses() {
        let mut w = Writer::new(7, FLAGS_RESPONSE);
        let q = Question { name: n("a.local"), qtype: TYPE_A, class: CLASS_IN, unicast: false };
        w.question(&q);
        let mut srv = vec![0, 0, 0, 0, 0, 80];
        put_name(&mut srv, &n("a.local"));
        let r = Record { name: n("x._http._tcp.local"), rtype: TYPE_SRV, class: CLASS_IN, flush: true, ttl: 120, rdata: srv };
        w.record(1, &r);
        let m = parse(&w.finish()).unwrap();
        assert_eq!((m.id, m.flags), (7, FLAGS_RESPONSE));
        assert_eq!(m.questions, vec![q]);
        assert_eq!(m.answers, vec![r]);
    }

    #[test]
    fn canonical_lowercases_only_names() {
        let mut srv = vec![0, 0, 0, 0, b'A', b'B'];
        put_name(&mut srv, &n("Panel.local"));
        let c = canonical(TYPE_SRV, &srv);
        assert_eq!(&c[..6], &[0, 0, 0, 0, b'A', b'B']);
        assert_eq!(&c[6..], b"\x05panel\x05local\x00");
        assert_eq!(canonical(TYPE_TXT, b"\x03A=B"), b"\x03A=B");
        let mut nsec = Vec::new();
        put_name(&mut nsec, &n("P.local"));
        nsec.extend_from_slice(&[0, 1, b'A']);
        assert_eq!(canonical(TYPE_NSEC, &nsec), b"\x01p\x05local\x00\x00\x01A");
    }

    #[test]
    fn names_compare_without_case() {
        assert!(name_eq(&n("Touchos-1A2B.local"), &n("touchos-1a2b.LOCAL")));
        assert!(!name_eq(&n("touchos.local"), &n("touchos-2.local")));
    }
}
