//! The root certificates TLS trusts, and finding the one a server's chain
//! ends at.
//!
//! The roots are one file, `cacerts.der` (next to this crate's Cargo.toml):
//! the certificates' DER one after the other. Each is a SEQUENCE with its
//! length in its header, so the next one starts where it ends and the file
//! needs no index. `cargo xtask cacerts` makes it from a Linux distro's
//! bundle and xtask puts it in the initrd; init shares it read-only (the
//! "cacerts" capability) with every program that speaks TLS.
//!
//! No dependencies besides `touchos-civil`: netc (TLS) and xtask both use it.

#![cfg_attr(not(test), no_std)]

pub mod der;

use der::{Reader, Tlv};

/// The roots' file name in the initrd.
pub const INITRD_FILE: &str = "cacerts.der";

/// The key of a certificate's subject (what its signatures need).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyType {
    Rsa,
    P256,
    P384,
    /// Anything else (P-521, Ed25519, ...): TLS here can't use it.
    Other,
}

const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const SECP256R1: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const SECP384R1: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];

/// Name attribute types (2.5.4.x).
pub const CN: &[u8] = &[0x55, 0x04, 0x03];
pub const O: &[u8] = &[0x55, 0x04, 0x0a];
pub const OU: &[u8] = &[0x55, 0x04, 0x0b];

/// The parts of an X.509 certificate that finding and checking a root needs.
#[derive(Clone, Copy, Debug)]
pub struct Cert<'a> {
    /// The whole certificate.
    pub der: &'a [u8],
    /// The issuer's and the subject's Name, as whole TLVs (compared byte
    /// for byte, as webpki does).
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    /// Validity, in UNIX seconds.
    pub not_before: i64,
    pub not_after: i64,
    pub key_type: KeyType,
    /// The subject's public key: the contents of the SPKI's BIT STRING (an
    /// RSAPublicKey, or an EC point).
    pub key: &'a [u8],
}

impl<'a> Cert<'a> {
    /// Parse a certificate (None if it isn't one we can read).
    pub fn parse(der_bytes: &'a [u8]) -> Option<Cert<'a>> {
        let (cert, _) = der::tlv(der_bytes).filter(|(t, _)| t.tag == der::SEQUENCE)?;
        let tbs = Reader(cert.value).expect(der::SEQUENCE)?;
        let mut r = Reader(tbs.value);
        r.optional(der::CONTEXT_0); // version
        r.expect(der::INTEGER)?; // serial number
        r.expect(der::SEQUENCE)?; // signature algorithm
        let issuer = r.expect(der::SEQUENCE)?;
        let validity = r.expect(der::SEQUENCE)?;
        let subject = r.expect(der::SEQUENCE)?;
        let spki = r.expect(der::SEQUENCE)?;

        let mut v = Reader(validity.value);
        let not_before = time(v.next()?)?;
        let not_after = time(v.next()?)?;

        let mut s = Reader(spki.value);
        let alg = s.expect(der::SEQUENCE)?;
        let bits = s.expect(der::BIT_STRING)?;
        let (&unused, key) = bits.value.split_first()?;
        if unused != 0 {
            return None;
        }
        let mut a = Reader(alg.value);
        let alg_oid = a.expect(der::OID)?.value;
        let key_type = if alg_oid == RSA_ENCRYPTION {
            KeyType::Rsa
        } else if alg_oid == EC_PUBLIC_KEY {
            match a.optional(der::OID).map(|t| t.value) {
                Some(SECP256R1) => KeyType::P256,
                Some(SECP384R1) => KeyType::P384,
                _ => KeyType::Other,
            }
        } else {
            KeyType::Other
        };

        Some(Cert {
            der: cert.raw,
            issuer: issuer.raw,
            subject: subject.raw,
            not_before,
            not_after,
            key_type,
            key,
        })
    }

    /// Whether `now` (UNIX seconds) is within the validity period.
    pub fn valid_at(&self, now: i64) -> bool {
        self.not_before <= now && now <= self.not_after
    }

    /// Whether issuer and subject are the same name (a root).
    pub fn self_issued(&self) -> bool {
        self.issuer == self.subject
    }
}

/// The attributes of a Name (a whole TLV, like [`Cert::subject`]):
/// (attribute type OID, value bytes), in order.
pub fn name_attrs(name: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    let rdns = der::tlv(name)
        .filter(|(t, _)| t.tag == der::SEQUENCE)
        .map(|(t, _)| t.value)
        .unwrap_or(&[]);
    let mut rdns = Reader(rdns);
    let mut attrs = Reader(&[]);
    core::iter::from_fn(move || {
        loop {
            if let Some(atv) = attrs.next() {
                let mut a = Reader(atv.value);
                let (Some(oid), Some(value)) = (a.expect(der::OID), a.next()) else {
                    continue;
                };
                return Some((oid.value, value.value));
            }
            attrs = Reader(rdns.expect(der::SET)?.value);
        }
    })
}

/// The first attribute `oid` of a Name, as text (if it is UTF-8).
pub fn name_attr<'a>(name: &'a [u8], oid: &[u8]) -> Option<&'a str> {
    name_attrs(name)
        .find(|(o, _)| *o == oid)
        .and_then(|(_, v)| core::str::from_utf8(v).ok())
}

/// A short name for logs: the CN, else the OU, else the O.
pub fn short_name(name: &[u8]) -> &str {
    name_attr(name, CN)
        .or_else(|| name_attr(name, OU))
        .or_else(|| name_attr(name, O))
        .unwrap_or("?")
}

/// UTCTime (YYMMDDHHMMSSZ) or GeneralizedTime (YYYYMMDDHHMMSSZ) -> UNIX seconds.
fn time(t: Tlv) -> Option<i64> {
    let s = t.value;
    let (year, rest) = match (t.tag, s.len()) {
        (der::UTC_TIME, 13) => {
            let yy = digits(&s[..2])? as i32;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &s[2..])
        }
        (der::GENERALIZED_TIME, 15) => (digits(&s[..4])? as i32, &s[4..]),
        _ => return None,
    };
    if rest[10] != b'Z' {
        return None;
    }
    let f = |i: usize| digits(&rest[i..i + 2]);
    let (month, day, hour, minute, second) = (f(0)?, f(2)?, f(4)?, f(6)?, f(8)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(touchos_civil::unix_from_civil(
        year, month, day, hour, minute, second,
    ))
}

fn digits(s: &[u8]) -> Option<u32> {
    s.iter().try_fold(0u32, |a, &c| {
        c.is_ascii_digit().then(|| a * 10 + (c - b'0') as u32)
    })
}

/// A set of roots: the contents of `cacerts.der`.
#[derive(Clone, Copy)]
pub struct Roots<'a>(pub &'a [u8]);

impl<'a> Roots<'a> {
    /// Each certificate's DER, in order. Stops at the first thing that isn't
    /// a whole SEQUENCE (the zero padding of a page-sized memory object,
    /// or a damaged file: then the roots after it are missing, and chains
    /// that need them fail).
    pub fn iter(&self) -> impl Iterator<Item = &'a [u8]> + 'a {
        let mut rest = self.0;
        core::iter::from_fn(move || {
            let (t, r) = der::tlv(rest).filter(|(t, _)| t.tag == der::SEQUENCE)?;
            rest = r;
            Some(t.raw)
        })
    }

    /// The roots we can read.
    pub fn certs(&self) -> impl Iterator<Item = Cert<'a>> + 'a {
        self.iter().filter_map(Cert::parse)
    }

    /// The roots named `subject`. (Usually one; a re-keyed root can share
    /// its name with its predecessor.)
    pub fn by_subject<'s>(&self, subject: &'s [u8]) -> impl Iterator<Item = Cert<'a>> + 's
    where
        'a: 's,
    {
        self.certs().filter(move |c| c.subject == subject)
    }

    /// Where a server's chain (leaf first, as TLS sends it) meets our roots:
    /// the index of the first certificate a root of ours issued. The chain
    /// is cut there; what the server sent after it (cross-signs by roots we
    /// don't have, or the root itself) is not needed. Each certificate up to
    /// that one must be issued by the next (by name; the signatures are
    /// checked by the TLS verifier).
    pub fn anchor<'c>(&self, chain: &[&'c [u8]]) -> Result<usize, NoAnchor<'c>> {
        let mut issuer: &[u8] = &[];
        for (i, der) in chain.iter().enumerate() {
            let c = Cert::parse(der).ok_or(NoAnchor::Unreadable(i))?;
            if i > 0 && c.subject != issuer {
                return Err(NoAnchor::Unordered(i));
            }
            if self.by_subject(c.issuer).next().is_some() {
                return Ok(i);
            }
            issuer = c.issuer;
        }
        let last = chain.last().and_then(|d| Cert::parse(d));
        Err(NoAnchor::UnknownIssuer(
            last.map(|c| c.issuer).unwrap_or(&[]),
        ))
    }
}

/// Why a chain doesn't lead to one of our roots.
#[derive(Debug, PartialEq, Eq)]
pub enum NoAnchor<'a> {
    /// Certificate `i` isn't one we can read.
    Unreadable(usize),
    /// Certificate `i` isn't the issuer of the one before it.
    Unordered(usize),
    /// No root of ours issued any of them; the last one's issuer (a Name).
    UnknownIssuer(&'a [u8]),
}

impl core::fmt::Display for NoAnchor<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            NoAnchor::Unreadable(i) => write!(f, "certificate {i} unreadable"),
            NoAnchor::Unordered(i) => write!(f, "certificate {i} did not issue the one before it"),
            NoAnchor::UnknownIssuer(n) => write!(f, "no trusted root for \"{}\"", short_name(n)),
        }
    }
}

#[cfg(test)]
mod tests;
