//! Update bundles: a new copy of the system in one signed file.
//!
//! ```text
//! "touchos-update-1"       16 bytes
//! manifest length          u32 LE
//! manifest                 UTF-8 lines:
//!                            build <id>
//!                            file <name> <length> <sha-256 hex>   (one per file)
//! signature                64 bytes: ed25519 over everything above
//! file contents            concatenated, in manifest order
//! ```
//!
//! The panel checks the signature against the public key it was built
//! with, then every file's length and hash, before it writes anything.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 16] = b"touchos-update-1";
/// Larger bundles are refused (today's is about 11 MB, 5 stripped).
pub const MAX_BUNDLE: usize = 64 << 20;
const MAX_FILES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not a bundle, truncated, or a malformed manifest.
    Format(&'static str),
    /// The signature doesn't match our key.
    Signature,
    /// A file's contents don't match its hash.
    Hash(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle<'a> {
    pub build: &'a str,
    pub files: Vec<File<'a>>,
}

impl Bundle<'_> {
    pub fn file(&self, name: &str) -> Option<&[u8]> {
        self.files.iter().find(|f| f.name == name).map(|f| f.data)
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A plain file name: letters, digits, '.', '-', '_' (no paths).
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('.')
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
}

/// Make a bundle and sign it with `secret_key` (64 bytes, ed25519).
pub fn build(build_id: &str, files: &[(&str, &[u8])], secret_key: &[u8; 64]) -> Vec<u8> {
    let mut manifest = format!("build {build_id}\n");
    for (name, data) in files {
        assert!(valid_name(name), "bad file name {name:?}");
        manifest.push_str(&format!(
            "file {name} {} {}\n",
            data.len(),
            hex(&Sha256::digest(data))
        ));
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    out.extend_from_slice(manifest.as_bytes());
    let sk = ed25519_compact::SecretKey::from_slice(secret_key).expect("secret key");
    let sig = sk.sign(&out, None);
    out.extend_from_slice(sig.as_ref());
    for (_, data) in files {
        out.extend_from_slice(data);
    }
    out
}

/// Check a bundle: signature by `public_key` (32 bytes), then every file.
pub fn verify<'a>(bundle: &'a [u8], public_key: &[u8; 32]) -> Result<Bundle<'a>, Error> {
    if bundle.len() > MAX_BUNDLE {
        return Err(Error::Format("too large"));
    }
    if bundle.get(..16) != Some(MAGIC) {
        return Err(Error::Format("not a touchos update"));
    }
    let mlen = u32::from_le_bytes(
        bundle
            .get(16..20)
            .ok_or(Error::Format("truncated"))?
            .try_into()
            .unwrap(),
    ) as usize;
    let signed_end = 20usize
        .checked_add(mlen)
        .filter(|&e| e + 64 <= bundle.len())
        .ok_or(Error::Format("truncated"))?;
    let signed = &bundle[..signed_end];
    let sig = &bundle[signed_end..signed_end + 64];
    let pk = ed25519_compact::PublicKey::from_slice(public_key).map_err(|_| Error::Signature)?;
    let sig = ed25519_compact::Signature::from_slice(sig).map_err(|_| Error::Signature)?;
    pk.verify(signed, &sig).map_err(|_| Error::Signature)?;

    // Signed: now the manifest can be trusted to describe the rest.
    let manifest =
        core::str::from_utf8(&bundle[20..signed_end]).map_err(|_| Error::Format("manifest"))?;
    let mut lines = manifest.lines();
    let build = lines
        .next()
        .and_then(|l| l.strip_prefix("build "))
        .filter(|b| !b.is_empty() && b.len() <= 64)
        .ok_or(Error::Format("no build line"))?;
    let mut at = signed_end + 64;
    let mut files = Vec::new();
    for line in lines {
        let mut w = line.split(' ');
        let (Some("file"), Some(name), Some(len), Some(hash), None) =
            (w.next(), w.next(), w.next(), w.next(), w.next())
        else {
            return Err(Error::Format("manifest line"));
        };
        let len: usize = len.parse().map_err(|_| Error::Format("file length"))?;
        if !valid_name(name) || files.iter().any(|f: &File| f.name == name) {
            return Err(Error::Format("file name"));
        }
        if files.len() == MAX_FILES {
            return Err(Error::Format("too many files"));
        }
        let data = at
            .checked_add(len)
            .and_then(|end| bundle.get(at..end))
            .ok_or(Error::Format("truncated"))?;
        if hex(&Sha256::digest(data)) != hash {
            return Err(Error::Hash(name.into()));
        }
        files.push(File { name, data });
        at += len;
    }
    if at != bundle.len() {
        return Err(Error::Format("trailing data"));
    }
    Ok(Bundle { build, files })
}

/// A new key pair from a 32-byte seed: (secret key, public key).
pub fn keypair(seed: &[u8; 32]) -> ([u8; 64], [u8; 32]) {
    let kp = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new(*seed));
    (*kp.sk, *kp.pk)
}

pub mod trial;

/// A copy of the system on the Toon's NAND (libs/nand's system copies):
/// the kernel as a flat binary, the initrd and the boot info block, which
/// the storage server writes into the copy that isn't running.
pub mod nand_copy {
    use alloc::format;
    use alloc::string::String;

    use super::Bundle;

    /// The kernel, a flat binary for its load address.
    pub const KERNEL: &str = "touchos";
    pub const INITRD: &str = "touchos.initrd";
    /// libs/abi's imx_boot block: the initrd's length and the command
    /// line (the boot stage adds the config's length, `slot=` and `build=`).
    pub const BOOTINFO: &str = "bootinfo.bin";
    pub const FILES: [&str; 3] = [KERNEL, INITRD, BOOTINFO];

    /// Whether a (verified) bundle holds a NAND copy: exactly those files.
    pub fn check(b: &Bundle) -> Result<(), String> {
        for name in FILES {
            if b.file(name).is_none() {
                return Err(format!("the update has no {name}"));
            }
        }
        if let Some(f) = b.files.iter().find(|f| !FILES.contains(&f.name)) {
            return Err(format!("unexpected file {} in the update", f.name));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nand_copy() {
        let (sk, pk) = keypair(&[7; 32]);
        let toon = build("t1", &[("touchos", b"k"), ("touchos.initrd", b"i"), ("bootinfo.bin", b"b")], &sk);
        assert_eq!(nand_copy::check(&verify(&toon, &pk).unwrap()), Ok(()));
        let short = build("t2", &[("touchos", b"k"), ("touchos.initrd", b"i")], &sk);
        assert_eq!(nand_copy::check(&verify(&short, &pk).unwrap()), Err("the update has no bootinfo.bin".into()));
        let extra = build("t3", &[("touchos", b"k"), ("touchos.initrd", b"i"), ("bootinfo.bin", b"b"), ("x", b"")], &sk);
        assert!(nand_copy::check(&verify(&extra, &pk).unwrap()).unwrap_err().contains("unexpected"));
    }

    fn sample() -> ([u8; 64], [u8; 32], Vec<u8>) {
        let (sk, pk) = keypair(&[7; 32]);
        let b = build(
            "abc123",
            &[("touchos", b"kernel"), ("touchos.initrd", &[1, 2, 3][..])],
            &sk,
        );
        (sk, pk, b)
    }

    #[test]
    fn round_trip() {
        let (_, pk, b) = sample();
        let v = verify(&b, &pk).unwrap();
        assert_eq!(v.build, "abc123");
        assert_eq!(v.file("touchos"), Some(&b"kernel"[..]));
        assert_eq!(v.file("touchos.initrd"), Some(&[1, 2, 3][..]));
    }

    #[test]
    fn wrong_key_and_tampering_are_refused() {
        let (_, _, b) = sample();
        let (_, other) = keypair(&[8; 32]);
        assert_eq!(verify(&b, &other), Err(Error::Signature));
        let (_, pk, b) = sample();
        // A changed manifest byte: signature.
        let mut m = b.clone();
        m[25] ^= 1;
        assert_eq!(verify(&m, &pk), Err(Error::Signature));
        // A changed file byte: that file's hash.
        let mut f = b.clone();
        let last = f.len() - 1;
        f[last] ^= 1;
        assert_eq!(verify(&f, &pk), Err(Error::Hash("touchos.initrd".into())));
        // Truncated, extended, not a bundle.
        assert!(matches!(
            verify(&b[..b.len() - 1], &pk),
            Err(Error::Format(_))
        ));
        let mut x = b.clone();
        x.push(0);
        assert_eq!(verify(&x, &pk), Err(Error::Format("trailing data")));
        assert!(matches!(verify(b"hello", &pk), Err(Error::Format(_))));
    }

    #[test]
    fn names() {
        assert!(valid_name("touchos.initrd"));
        assert!(valid_name("bootinfo.bin"));
        assert!(!valid_name("../x") && !valid_name("A/touchos") && !valid_name(".hidden"));
    }
}
