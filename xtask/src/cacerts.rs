//! `cargo xtask cacerts [PEM] [--check]`: the roots TLS trusts
//! (`libs/certs/cacerts.der`, see the touchos-certs crate), made from a
//! Linux distro's CA bundle (Mozilla's root store).
//!
//! It keeps the roots that are valid today and whose key TLS here can use
//! (RSA, P-256, P-384), sorted by subject, so the same bundle on the same
//! day gives the same files. `cacerts.txt` lists them for review: a
//! regenerated bundle shows up in git as lines added and removed.
//!
//! `--check` writes nothing; it says whether the files are what the
//! bundle gives today. `cargo xtask check` runs that and warns, since an
//! out-of-date list is something to look at, not a broken build.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use touchos_certs::{Cert, KeyType};

use crate::root;

/// Where Debian and Ubuntu (and most others) keep the bundle.
pub const DEFAULT_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// Warn when a root runs out within this long.
const EXPIRY_WARNING_SECS: i64 = 365 * 86_400;

fn der_path() -> PathBuf {
    root().join("libs/certs").join(touchos_certs::INITRD_FILE)
}

fn txt_path() -> PathBuf {
    root().join("libs/certs/cacerts.txt")
}

/// The checked-in roots file (for the initrd).
pub fn file() -> PathBuf {
    der_path()
}

pub fn run(bundle: Option<&str>, check: bool) -> Result<()> {
    let bundle = Path::new(bundle.unwrap_or(DEFAULT_BUNDLE));
    let made = make(bundle, now())?;
    for s in &made.skipped {
        println!("skipped: {s}");
    }
    if check {
        for w in expiring()? {
            println!("warning: {w}");
        }
        let stale = compare(&made)?;
        if stale.is_empty() {
            println!("libs/certs/cacerts.der is up to date with {} ({} roots)", bundle.display(), made.count);
            return Ok(());
        }
        for w in &stale {
            println!("{w}");
        }
        bail!("libs/certs/cacerts.der is not up to date; run `cargo xtask cacerts`");
    }
    std::fs::write(der_path(), &made.der)?;
    std::fs::write(txt_path(), &made.txt)?;
    println!("wrote libs/certs/cacerts.der ({} roots, {} bytes) and cacerts.txt", made.count, made.der.len());
    Ok(())
}

/// For `cargo xtask check`: what is worth a warning (nothing if the host
/// has no bundle to compare with).
pub fn warnings() -> Vec<String> {
    let bundle = Path::new(DEFAULT_BUNDLE);
    if !bundle.exists() {
        return vec![format!("note: no {DEFAULT_BUNDLE}; cacerts.der not compared")];
    }
    let mut w = expiring().unwrap_or_else(|e| vec![format!("cacerts: {e:#}")]);
    match make(bundle, now()).and_then(|m| compare(&m)) {
        Ok(stale) => w.extend(stale),
        Err(e) => w.push(format!("cacerts: {e:#}")),
    }
    w
}

/// What the checked-in files lack against `made` (nothing if they match).
fn compare(made: &Made) -> Result<Vec<String>> {
    let mut w = Vec::new();
    let current = std::fs::read(der_path()).context("reading libs/certs/cacerts.der")?;
    if current != made.der {
        let old: Vec<String> = std::fs::read_to_string(txt_path()).unwrap_or_default().lines().map(str::to_string).collect();
        let new: Vec<&str> = made.txt.lines().collect();
        for l in new.iter().filter(|l| !l.starts_with('#') && !old.iter().any(|o| o == *l)) {
            w.push(format!("cacerts: new in the bundle: {l}"));
        }
        for l in old.iter().filter(|l| !l.starts_with('#') && !new.contains(&l.as_str())) {
            w.push(format!("cacerts: gone from the bundle (or expired): {l}"));
        }
        if w.is_empty() {
            w.push("cacerts: cacerts.der differs from what the bundle gives".to_string());
        }
        w.push("cacerts: run `cargo xtask cacerts` to update it".to_string());
    }
    Ok(w)
}

/// The checked-in roots that run out within [`EXPIRY_WARNING_SECS`].
fn expiring() -> Result<Vec<String>> {
    let current = std::fs::read(der_path()).context("reading libs/certs/cacerts.der")?;
    let soon = now() + EXPIRY_WARNING_SECS;
    Ok(touchos_certs::Roots(&current)
        .certs()
        .filter(|c| c.not_after < soon)
        .map(|c| format!("cacerts: a root expires {}: {}", date(c.not_after), name(c.subject)))
        .collect())
}

struct Made {
    der: Vec<u8>,
    txt: String,
    count: usize,
    skipped: Vec<String>,
}

fn make(bundle: &Path, now: i64) -> Result<Made> {
    let pem = std::fs::read_to_string(bundle).with_context(|| format!("reading {}", bundle.display()))?;
    let ders = pem_certificates(&pem)?;
    let mut skipped = Vec::new();
    let mut keep: Vec<(String, String, Vec<u8>)> = Vec::new(); // (subject, fingerprint, der)
    for der in ders {
        let Some(c) = Cert::parse(&der) else {
            skipped.push(format!("a certificate we can't read ({} bytes)", der.len()));
            continue;
        };
        let (subject, fp) = (name(c.subject), hex(&Sha256::digest(&der)));
        if c.key_type == KeyType::Other {
            skipped.push(format!("{subject}: key type TLS here can't use"));
        } else if !c.valid_at(now) {
            skipped.push(format!("{subject}: not valid today ({} to {})", date(c.not_before), date(c.not_after)));
        } else if keep.iter().any(|k| k.1 == fp) {
            skipped.push(format!("{subject}: twice in the bundle"));
        } else {
            keep.push((subject, fp, der));
        }
    }
    if keep.is_empty() {
        bail!("no usable certificates in {}", bundle.display());
    }
    keep.sort_by(|a, b| (a.0.to_lowercase(), &a.1).cmp(&(b.0.to_lowercase(), &b.1)));

    let mut txt = format!(
        "# The roots in cacerts.der, made by `cargo xtask cacerts` from {}{}.\n\
         # {} roots. SHA-256 fingerprint, valid from, valid until, subject.\n",
        bundle.display(),
        package_version().map(|v| format!(" (ca-certificates {v})")).unwrap_or_default(),
        keep.len(),
    );
    let mut der = Vec::new();
    for (subject, fp, d) in &keep {
        let c = Cert::parse(d).expect("parsed above");
        txt += &format!("{fp} {} {} {subject}\n", date(c.not_before), date(c.not_after));
        der.extend_from_slice(d);
    }
    Ok(Made { der, txt, count: keep.len(), skipped })
}

/// The distro package the bundle comes from, if dpkg knows it.
fn package_version() -> Option<String> {
    let out = std::process::Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", "ca-certificates"])
        .output()
        .ok()?;
    let v = String::from_utf8(out.stdout).ok()?;
    (out.status.success() && !v.is_empty()).then_some(v)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// UNIX seconds -> YYYY-MM-DD.
fn date(t: i64) -> String {
    let c = touchos_civil::local(t.max(0) as u64, 0, false);
    format!("{:04}-{:02}-{:02}", c.year, c.month, c.day)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A Name as "C=.., O=.., CN=.." (the attributes people know, in order).
fn name(n: &[u8]) -> String {
    const KNOWN: &[(&[u8], &str)] = &[
        (&[0x55, 0x04, 0x06], "C"),
        (&[0x55, 0x04, 0x08], "ST"),
        (&[0x55, 0x04, 0x07], "L"),
        (touchos_certs::O, "O"),
        (touchos_certs::OU, "OU"),
        (touchos_certs::CN, "CN"),
    ];
    let parts: Vec<String> = touchos_certs::name_attrs(n)
        .filter_map(|(oid, v)| KNOWN.iter().find(|k| k.0 == oid).map(|k| format!("{}={}", k.1, text(v))))
        .collect();
    parts.join(", ")
}

/// A string attribute's bytes as text: UTF-8 (and ASCII), else UTF-16BE (BMPString).
fn text(v: &[u8]) -> String {
    match std::str::from_utf8(v) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let u: Vec<u16> = v.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
            String::from_utf16_lossy(&u)
        }
    }
}

/// The DER of each CERTIFICATE block in a PEM file.
fn pem_certificates(pem: &str) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut b64: Option<String> = None;
    for line in pem.lines().map(str::trim) {
        match (line, &mut b64) {
            ("-----BEGIN CERTIFICATE-----", None) => b64 = Some(String::new()),
            ("-----END CERTIFICATE-----", Some(s)) => {
                out.push(base64(s).context("a CERTIFICATE block isn't base64")?);
                b64 = None;
            }
            (l, Some(s)) => s.push_str(l),
            _ => {}
        }
    }
    Ok(out)
}

fn base64(s: &str) -> Result<Vec<u8>> {
    let val = |c: u8| -> Result<u32> {
        Ok(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => bail!("bad base64 character {:?}", c as char),
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8h").unwrap(), b"hello!");
        assert_eq!(base64("aGk=").unwrap(), b"hi");
        assert!(base64("a$==").is_err());
    }

    #[test]
    fn pem_blocks() {
        let pem = "junk\n-----BEGIN CERTIFICATE-----\naGVs\nbG8=\n-----END CERTIFICATE-----\n\
                   # comment\n-----BEGIN CERTIFICATE-----\naGk=\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_certificates(pem).unwrap(), vec![b"hello".to_vec(), b"hi".to_vec()]);
    }

    #[test]
    fn checked_in_list_matches_the_roots() {
        let der = std::fs::read(der_path()).unwrap();
        let txt = std::fs::read_to_string(txt_path()).unwrap();
        let fps: Vec<String> = touchos_certs::Roots(&der).iter().map(|d| hex(&Sha256::digest(d))).collect();
        let listed: Vec<&str> = txt.lines().filter(|l| !l.starts_with('#')).map(|l| &l[..64]).collect();
        assert_eq!(fps, listed);
    }
}
