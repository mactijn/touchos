//! Just enough DER to walk a certificate: one TLV at a time, single-byte
//! tags, definite lengths (DER has no other kind).

pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OID: u8 = 0x06;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;
/// `[0] EXPLICIT` (the certificate's version).
pub const CONTEXT_0: u8 = 0xa0;

/// One element: its tag, its contents, and the whole TLV (header included).
#[derive(Clone, Copy, Debug)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub value: &'a [u8],
    pub raw: &'a [u8],
}

/// Reads the elements of `data` in order.
#[derive(Clone)]
pub struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The next element, or None at the end or on malformed data (which
    /// also ends the reader).
    pub fn next(&mut self) -> Option<Tlv<'a>> {
        let r = tlv(self.0);
        match r {
            Some((t, rest)) => {
                self.0 = rest;
                Some(t)
            }
            None => {
                self.0 = &[];
                None
            }
        }
    }

    /// The next element, which must have tag `tag`.
    pub fn expect(&mut self, tag: u8) -> Option<Tlv<'a>> {
        self.next().filter(|t| t.tag == tag)
    }

    /// The next element if it has tag `tag`; otherwise nothing is consumed.
    pub fn optional(&mut self, tag: u8) -> Option<Tlv<'a>> {
        match tlv(self.0) {
            Some((t, rest)) if t.tag == tag => {
                self.0 = rest;
                Some(t)
            }
            _ => None,
        }
    }
}

/// The TLV at the start of `data`, and what follows it.
pub fn tlv(data: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let (&tag, rest) = data.split_first()?;
    if tag & 0x1f == 0x1f {
        return None; // multi-byte tag: not in a certificate's structure
    }
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None; // indefinite (not DER), or absurdly long
        }
        let len = rest[..n].iter().fold(0usize, |a, &b| (a << 8) | b as usize);
        (len, &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    let header = data.len() - rest.len();
    let t = Tlv {
        tag,
        value: &rest[..len],
        raw: &data[..header + len],
    };
    Some((t, &rest[len..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        let (t, rest) = tlv(&[0x02, 0x01, 0x05, 0xff]).unwrap();
        assert_eq!(
            (t.tag, t.value, t.raw.len(), rest),
            (INTEGER, &[5u8][..], 3, &[0xffu8][..])
        );
        let mut long = vec![0x04, 0x82, 0x01, 0x00];
        long.extend([0u8; 256]);
        let (t, rest) = tlv(&long).unwrap();
        assert_eq!((t.value.len(), t.raw.len(), rest.len()), (256, 260, 0));
    }

    #[test]
    fn malformed() {
        assert!(tlv(&[]).is_none());
        assert!(tlv(&[0x30]).is_none());
        assert!(tlv(&[0x30, 0x80, 0, 0]).is_none()); // indefinite length
        assert!(tlv(&[0x30, 0x05, 1, 2]).is_none()); // short
        assert!(tlv(&[0x1f, 0x01, 0x00]).is_none()); // multi-byte tag
    }
}
