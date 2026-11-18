//! Software CCMP (AES-128-CCM, 8-byte MIC). The Intel firmware doesn't
//! decrypt group-addressed frames for us (same approach as OpenBSD's iwm:
//! do it here); a soft-MAC leaves all of CCMP, both ways, to the host.

use aes::Aes128;
use aes::cipher::{BlockCipherEncrypt, KeyInit};
use alloc::vec::Vec;

use crate::ieee80211::Hdr;

/// Packet number from the CCMP header that follows the 802.11 header.
pub fn pn(frame: &[u8]) -> Option<u64> {
    let h = Hdr::parse(frame)?.len;
    let iv = frame.get(h..h + 8)?;
    Some(u64::from_le_bytes([
        iv[0], iv[1], iv[4], iv[5], iv[6], iv[7], 0, 0,
    ]))
}

fn nonce_and_aad(f: &[u8], h: &Hdr, pn: u64) -> ([u8; 13], Vec<u8>) {
    let tid = h.tid.unwrap_or(0);
    let mut nonce = [0u8; 13];
    nonce[0] = tid;
    nonce[1..7].copy_from_slice(&f[10..16]); // A2
    let pnb = pn.to_be_bytes();
    nonce[7..13].copy_from_slice(&pnb[2..8]);
    let mut aad = Vec::with_capacity(30);
    let fc0 = if (f[0] >> 2) & 3 == 2 {
        f[0] & 0x8f
    } else {
        f[0]
    }; // mask data subtype bits 4-6
    aad.push(fc0);
    // Clear retry, power management, more data; in QoS data also Order
    // (+HTC, 802.11-2016 12.5.3.3.3).
    aad.push(f[1] & if h.tid.is_some() { 0x47 } else { 0xc7 });
    aad.extend_from_slice(&f[4..22]); // A1 A2 A3
    aad.push(f[22] & 0x0f); // sequence control: fragment number only
    aad.push(0);
    if f[1] & 3 == 3 {
        aad.extend_from_slice(&f[24..30]);
    }
    if h.tid.is_some() {
        aad.push(tid);
        aad.push(0);
    }
    (nonce, aad)
}

struct Ccm {
    aes: Aes128,
}

impl Ccm {
    fn new(key: &[u8; 16]) -> Ccm {
        Ccm {
            aes: Aes128::new(&(*key).into()),
        }
    }
    fn enc(&self, b: &mut [u8; 16]) {
        let mut g = aes::Block::from(*b);
        self.aes.encrypt_block(&mut g);
        *b = g.into();
    }
    fn ctr_block(&self, nonce: &[u8; 13], i: u16) -> [u8; 16] {
        let mut a = [0u8; 16];
        a[0] = 0x01; // L - 1 = 1
        a[1..14].copy_from_slice(nonce);
        a[14..16].copy_from_slice(&i.to_be_bytes());
        self.enc(&mut a);
        a
    }
    fn cbc_mac(&self, nonce: &[u8; 13], aad: &[u8], msg: &[u8]) -> [u8; 16] {
        let mut x = [0u8; 16];
        x[0] = 0x59; // Adata | M=8 -> (8-2)/2 << 3 | L-1
        x[1..14].copy_from_slice(nonce);
        x[14..16].copy_from_slice(&(msg.len() as u16).to_be_bytes());
        self.enc(&mut x);
        let mut a = Vec::with_capacity(2 + aad.len());
        a.extend_from_slice(&(aad.len() as u16).to_be_bytes());
        a.extend_from_slice(aad);
        for chunk in a.chunks(16).chain(msg.chunks(16)) {
            for (i, &c) in chunk.iter().enumerate() {
                x[i] ^= c;
            }
            self.enc(&mut x);
        }
        x
    }
    fn crypt(&self, nonce: &[u8; 13], data: &mut [u8]) {
        for (i, chunk) in data.chunks_mut(16).enumerate() {
            let s = self.ctr_block(nonce, i as u16 + 1);
            for (j, c) in chunk.iter_mut().enumerate() {
                *c ^= s[j];
            }
        }
    }
}

/// Decrypt a protected frame (header + CCMP header + ciphertext + MIC).
/// Returns the frame with CCMP header and MIC removed and Protected cleared.
pub fn decrypt(key: &[u8; 16], frame: &[u8]) -> Option<Vec<u8>> {
    let hdr = Hdr::parse(frame)?;
    let h = hdr.len;
    if frame.len() < h + 16 || frame[h + 3] & 0x20 == 0 {
        return None;
    }
    let pn = pn(frame)?;
    let (nonce, aad) = nonce_and_aad(frame, &hdr, pn);
    let ccm = Ccm::new(key);
    let mut plain = frame[h + 8..frame.len() - 8].to_vec();
    ccm.crypt(&nonce, &mut plain);
    let t = ccm.cbc_mac(&nonce, &aad, &plain);
    let s0 = ccm.ctr_block(&nonce, 0);
    let mic = &frame[frame.len() - 8..];
    if (0..8).any(|i| t[i] ^ s0[i] != mic[i]) {
        return None;
    }
    let mut out = Vec::with_capacity(h + plain.len());
    out.extend_from_slice(&frame[..h]);
    out[1] &= !0x40;
    out.extend_from_slice(&plain);
    Some(out)
}

/// Encrypt a frame body: returns `hdr` (with Protected set) + CCMP header
/// + ciphertext + MIC. `pn` must grow with every frame sent with `key`.
pub fn encrypt(key: &[u8; 16], hdr: &[u8], pn: u64, keyid: u8, plain: &[u8]) -> Vec<u8> {
    let p = pn.to_le_bytes();
    let mut f = hdr.to_vec();
    f[1] |= 0x40;
    f.extend_from_slice(&[p[0], p[1], 0, 0x20 | (keyid << 6), p[2], p[3], p[4], p[5]]);
    let h = Hdr::parse(&f).expect("encrypt: a data frame's header");
    let (nonce, aad) = nonce_and_aad(&f, &h, pn);
    let ccm = Ccm::new(key);
    let t = ccm.cbc_mac(&nonce, &aad, plain);
    let mut c = plain.to_vec();
    ccm.crypt(&nonce, &mut c);
    let s0 = ccm.ctr_block(&nonce, 0);
    f.extend_from_slice(&c);
    f.extend((0..8).map(|i| t[i] ^ s0[i]));
    f
}

/// The work of sending `total` bytes in data frames of `frame` bytes:
/// encrypt them all, as the station does for each frame it sends (the
/// caller times it). Returns the number of frames.
pub fn bench(total: usize, frame: usize) -> usize {
    let key = [0x5a; 16];
    let mut hdr = [0u8; 24];
    hdr[0] = 0x08; // data, to-DS
    hdr[1] = 0x01;
    let body = alloc::vec![0xa5u8; frame];
    let mut frames = 0;
    let mut left = total;
    while left > 0 {
        let n = left.min(frame);
        let f = encrypt(&key, &hdr, frames as u64 + 1, 0, &body[..n]);
        core::hint::black_box(f);
        left -= n;
        frames += 1;
    }
    frames
}

/// IEEE 802.11-2016 J.6.4 CCMP test vector; plus a decrypt round trip.
pub fn self_test() -> bool {
    let key = [
        0xc9, 0x7c, 0x1f, 0x67, 0xce, 0x37, 0x11, 0x85, 0x51, 0x4a, 0x8a, 0x19, 0xf2, 0xbd, 0xd5,
        0x2f,
    ];
    let hdr = [
        0x08, 0x48, 0xc3, 0x2c, 0x0f, 0xd2, 0xe1, 0x28, 0xa5, 0x7c, 0x50, 0x30, 0xf1, 0x84, 0x44,
        0x08, 0xab, 0xae, 0xa5, 0xb8, 0xfc, 0xba, 0x80, 0x33,
    ];
    let plain = [
        0xf8, 0xba, 0x1a, 0x55, 0xd0, 0x2f, 0x85, 0xae, 0x96, 0x7b, 0xb6, 0x2f, 0xb6, 0xcd, 0xa8,
        0xeb, 0x7e, 0x78, 0xa0, 0x50,
    ];
    let want_tail = [0x78, 0x45, 0xce, 0x0b, 0x16, 0xf9, 0x76, 0x23]; // MIC
    let f = encrypt(&key, &hdr, 0xb503_9776_e70c, 0, &plain);
    let vector_ok = f[f.len() - 8..] == want_tail;
    let round_trip = decrypt(&key, &f).is_some_and(|d| d[24..] == plain);
    if !vector_ok {
        ::log::warn!(
            "ccmp: test vector MIC mismatch (got {:02x?})",
            &f[f.len() - 8..]
        );
    }
    vector_ok && round_trip
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors() {
        assert!(self_test());
    }

    #[test]
    fn bench_counts_frames() {
        assert_eq!(bench(64 * 1024, 1500), 44); // 43 whole frames and 1036 bytes
        assert_eq!(bench(3000, 1500), 2);
        assert_eq!(bench(0, 1500), 0);
    }

    #[test]
    fn data_frame_round_trip() {
        let key = [7u8; 16];
        let mut hdr = [0u8; 24];
        hdr[0] = 0x08; // data, to-DS, Protected not yet set
        hdr[1] = 0x01;
        hdr[4..10].copy_from_slice(&[2, 0, 0, 0, 0, 2]);
        hdr[10..16].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        let body = b"\xaa\xaa\x03\0\0\0\x08\x00hello, access point";
        let f = encrypt(&key, &hdr, 0x0102_0304_0506, 0, body);
        assert_eq!(f[1], 0x41);
        assert_eq!(f.len(), 24 + 8 + body.len() + 8);
        assert_eq!(pn(&f), Some(0x0102_0304_0506));
        let d = decrypt(&key, &f).unwrap();
        assert_eq!(&d[24..], &body[..]);
        assert_eq!(d[1], 0x01);
        let mut bad = f.clone();
        bad[40] ^= 1; // ciphertext
        assert!(decrypt(&key, &bad).is_none());
    }
}
