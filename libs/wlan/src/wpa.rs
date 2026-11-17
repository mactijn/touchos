//! WPA2-PSK (RSN, CCMP, AKM PSK) station side: key derivation and the
//! EAPOL-Key 4-way handshake (IEEE 802.11-2016 12.7).

use aes::Aes128;
use aes::cipher::{BlockCipherDecrypt, KeyInit};
use alloc::vec;
use alloc::vec::Vec;
use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// The PMK from the configured PSK: 64 hex digits are the PMK itself; a
/// passphrase (8..63 characters) gives PBKDF2-HMAC-SHA1(passphrase, ssid,
/// 4096, 256 bits) (IEEE 802.11-2016 J.4.1).
pub fn pmk(psk: &str, ssid: &[u8]) -> [u8; 32] {
    if let Some(pmk) = hex_pmk(psk) {
        return pmk;
    }
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(psk.as_bytes(), ssid, 4096, &mut out);
    out
}

/// A PSK of exactly 64 hex digits, decoded (a passphrase is at most 63).
fn hex_pmk(psk: &str) -> Option<[u8; 32]> {
    if psk.len() != 64 {
        return None;
    }
    let digit = |c: u8| (c as char).to_digit(16);
    let mut out = [0u8; 32];
    for (o, pair) in out.iter_mut().zip(psk.as_bytes().chunks(2)) {
        *o = (digit(pair[0])? << 4 | digit(pair[1])?) as u8;
    }
    Some(out)
}

fn hmac_sha1(key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
    let mut m = HmacSha1::new_from_slice(key).expect("hmac key");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// IEEE 802.11 PRF-n based on HMAC-SHA1.
pub fn prf(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8]) {
    let mut i = 0u8;
    let mut pos = 0;
    while pos < out.len() {
        let h = hmac_sha1(key, &[label, &[0], data, &[i]]);
        let n = (out.len() - pos).min(20);
        out[pos..pos + n].copy_from_slice(&h[..n]);
        pos += n;
        i += 1;
    }
}

pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

pub fn derive_ptk(
    pmk: &[u8; 32],
    aa: &[u8; 6],
    spa: &[u8; 6],
    anonce: &[u8; 32],
    snonce: &[u8; 32],
) -> Ptk {
    let mut data = Vec::with_capacity(76);
    let (a1, a2) = if aa < spa { (aa, spa) } else { (spa, aa) };
    data.extend_from_slice(a1);
    data.extend_from_slice(a2);
    let (n1, n2) = if anonce < snonce {
        (anonce, snonce)
    } else {
        (snonce, anonce)
    };
    data.extend_from_slice(n1);
    data.extend_from_slice(n2);
    let mut out = [0u8; 48];
    prf(pmk, b"Pairwise key expansion", &data, &mut out);
    let mut p = Ptk {
        kck: [0; 16],
        kek: [0; 16],
        tk: [0; 16],
    };
    p.kck.copy_from_slice(&out[0..16]);
    p.kek.copy_from_slice(&out[16..32]);
    p.tk.copy_from_slice(&out[32..48]);
    p
}

/// RFC 3394 AES key unwrap (used for the encrypted Key Data in message 3).
pub fn aes_unwrap(kek: &[u8; 16], wrapped: &[u8]) -> Option<Vec<u8>> {
    if wrapped.len() < 24 || wrapped.len() % 8 != 0 {
        return None;
    }
    let n = wrapped.len() / 8 - 1;
    let cipher = Aes128::new(&(*kek).into());
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    let mut r: Vec<[u8; 8]> = wrapped[8..]
        .chunks(8)
        .map(|c| c.try_into().unwrap())
        .collect();
    for j in (0..6).rev() {
        for i in (1..=n).rev() {
            let t = (n * j + i) as u64;
            let mut b = [0u8; 16];
            for (k, x) in t.to_be_bytes().iter().enumerate() {
                b[k] = a[k] ^ x;
            }
            b[8..].copy_from_slice(&r[i - 1]);
            let mut block = aes::Block::from(b);
            cipher.decrypt_block(&mut block);
            a.copy_from_slice(&block[..8]);
            r[i - 1].copy_from_slice(&block[8..]);
        }
    }
    (a == [0xa6; 8]).then(|| r.concat())
}

// ---- EAPOL-Key frames ----

pub const KI_TYPE_HMAC_SHA1_AES: u16 = 2;
pub const KI_PAIRWISE: u16 = 1 << 3;
pub const KI_INSTALL: u16 = 1 << 6;
pub const KI_ACK: u16 = 1 << 7;
pub const KI_MIC: u16 = 1 << 8;
pub const KI_SECURE: u16 = 1 << 9;
pub const KI_ENC_DATA: u16 = 1 << 12;

/// Offsets inside the EAPOL frame (4-byte EAPOL header + key descriptor).
const OFF_INFO: usize = 5;
const OFF_KEYLEN: usize = 7;
const OFF_REPLAY: usize = 9;
const OFF_NONCE: usize = 17;
pub(crate) const OFF_RSC: usize = 65;
pub(crate) const OFF_MIC: usize = 81;
const OFF_DATALEN: usize = 97;
const OFF_DATA: usize = 99;

pub struct KeyFrame<'a> {
    pub raw: &'a [u8],
    pub info: u16,
    pub replay: [u8; 8],
    pub nonce: [u8; 32],
    /// Key RSC: the group key's receive sequence counter (the last packet
    /// number the access point sent with it), six bytes little-endian as a
    /// CCMP packet number. Meaningful in message 3 and the group message.
    pub rsc: u64,
    pub data: &'a [u8],
}

pub fn parse_key_frame(eapol: &[u8]) -> Option<KeyFrame<'_>> {
    if eapol.len() < OFF_DATA || eapol[1] != 3 || eapol[4] != 2 {
        return None; // not an RSN EAPOL-Key frame
    }
    let dlen = u16::from_be_bytes([eapol[OFF_DATALEN], eapol[OFF_DATALEN + 1]]) as usize;
    let data = eapol.get(OFF_DATA..OFF_DATA + dlen)?;
    Some(KeyFrame {
        raw: eapol,
        info: u16::from_be_bytes([eapol[OFF_INFO], eapol[OFF_INFO + 1]]),
        replay: eapol[OFF_REPLAY..OFF_REPLAY + 8].try_into().unwrap(),
        nonce: eapol[OFF_NONCE..OFF_NONCE + 32].try_into().unwrap(),
        rsc: {
            let mut b = [0u8; 8];
            b[..6].copy_from_slice(&eapol[OFF_RSC..OFF_RSC + 6]);
            u64::from_le_bytes(b)
        },
        data,
    })
}

pub fn verify_mic(kck: &[u8; 16], frame: &KeyFrame) -> bool {
    let mut copy = frame.raw.to_vec();
    let mic: [u8; 16] = copy[OFF_MIC..OFF_MIC + 16].try_into().unwrap();
    copy[OFF_MIC..OFF_MIC + 16].fill(0);
    let calc = hmac_sha1(kck, &[&copy]);
    calc[..16] == mic
}

/// Build an EAPOL-Key frame from the station (message 2 or 4), MIC included.
pub fn build_key_frame(
    kck: &[u8; 16],
    info: u16,
    replay: &[u8; 8],
    nonce: &[u8; 32],
    data: &[u8],
) -> Vec<u8> {
    let body_len = 95 + data.len();
    let mut f = vec![0u8; 4 + body_len];
    f[0] = 1; // EAPOL version 1 (802.1X-2001), widely accepted
    f[1] = 3; // EAPOL-Key
    f[2..4].copy_from_slice(&(body_len as u16).to_be_bytes());
    f[4] = 2; // RSN key descriptor
    f[OFF_INFO..OFF_INFO + 2].copy_from_slice(&info.to_be_bytes());
    f[OFF_KEYLEN..OFF_KEYLEN + 2].copy_from_slice(&0u16.to_be_bytes());
    f[OFF_REPLAY..OFF_REPLAY + 8].copy_from_slice(replay);
    f[OFF_NONCE..OFF_NONCE + 32].copy_from_slice(nonce);
    f[OFF_DATALEN..OFF_DATALEN + 2].copy_from_slice(&(data.len() as u16).to_be_bytes());
    f[OFF_DATA..].copy_from_slice(data);
    sign(kck, &mut f);
    f
}

/// Fill in an EAPOL-Key frame's MIC (computed with the MIC field zero).
pub fn sign(kck: &[u8; 16], frame: &mut [u8]) {
    frame[OFF_MIC..OFF_MIC + 16].fill(0);
    let mic = hmac_sha1(kck, &[frame]);
    frame[OFF_MIC..OFF_MIC + 16].copy_from_slice(&mic[..16]);
}

/// Our RSN element: CCMP group + pairwise, AKM PSK.
pub fn rsn_ie(group_cipher: [u8; 4]) -> Vec<u8> {
    let mut ie = vec![48, 20, 1, 0];
    ie.extend_from_slice(&group_cipher);
    ie.extend_from_slice(&[1, 0, 0x00, 0x0f, 0xac, 4]); // pairwise: CCMP
    ie.extend_from_slice(&[1, 0, 0x00, 0x0f, 0xac, 2]); // AKM: PSK
    ie.extend_from_slice(&[0, 0]); // capabilities
    ie
}

/// Find the GTK KDE (00-0f-ac:1) in decrypted Key Data: returns (key id, GTK).
pub fn find_gtk(data: &[u8]) -> Option<(u8, Vec<u8>)> {
    for (id, body) in crate::ieee80211::ies(data) {
        if id == 0xdd && body.len() >= 6 && body[..4] == [0x00, 0x0f, 0xac, 0x01] {
            return Some((body[4] & 3, body[6..].to_vec()));
        }
        if id == 0xdd && body.is_empty() {
            break; // padding
        }
    }
    None
}

/// Self-test against the IEEE 802.11 test vectors (logged at boot).
pub fn self_test() -> bool {
    // IEEE 802.11-2016 J.4.2: passphrase "password", SSID "IEEE".
    let p = pmk("password", b"IEEE");
    let want: [u8; 8] = [0xf4, 0x2c, 0x6f, 0xc5, 0x2d, 0xf0, 0xeb, 0xef];
    // RFC 3394 4.1: 128-bit key wrap test.
    let kek: [u8; 16] = core::array::from_fn(|i| i as u8);
    let wrapped = [
        0x1f, 0xa6, 0x8b, 0x0a, 0x81, 0x12, 0xb4, 0x47, 0xae, 0xf3, 0x4b, 0xd8, 0xfb, 0x5a, 0x7b,
        0x82, 0x9d, 0x3e, 0x86, 0x23, 0x71, 0xd2, 0xcf, 0xe5,
    ];
    let plain: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
    p[..8] == want && aes_unwrap(&kek, &wrapped).as_deref() == Some(&plain[..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors() {
        assert!(self_test());
    }

    #[test]
    fn hex_psk_is_the_pmk() {
        let hex = "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e";
        let want = pmk("password", b"IEEE");
        assert_eq!(pmk(hex, b"any ssid"), want);
        assert_eq!(pmk(&hex.to_uppercase(), b"IEEE"), want);
        // 64 characters that aren't all hex: a (non-standard) passphrase.
        let odd = "g".repeat(64);
        assert_ne!(pmk(&odd, b"IEEE"), [0; 32]);
        assert!(hex_pmk(&odd).is_none());
        assert!(hex_pmk(&hex[..62]).is_none());
    }

    #[test]
    fn key_frame_fields() {
        let kck = [3u8; 16];
        let mut f = build_key_frame(&kck, KI_ACK | KI_INSTALL, &[0, 0, 0, 0, 0, 0, 0, 2], &[9; 32], &[1, 2, 3]);
        // The fields build_key_frame leaves zero: the IV (49..65) and the
        // Key RSC (65..73). An RSC of 0x0605_0403_0201 behind a nonzero IV.
        f[49..65].fill(0xee);
        f[65..73].copy_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]);
        let k = parse_key_frame(&f).unwrap();
        assert_eq!(k.info, KI_ACK | KI_INSTALL);
        assert_eq!(k.replay, [0, 0, 0, 0, 0, 0, 0, 2]);
        assert_eq!(k.nonce, [9; 32]);
        assert_eq!(k.rsc, 0x0605_0403_0201);
        assert_eq!(k.data, &[1, 2, 3]);
        assert!(parse_key_frame(&f[..f.len() - 1]).is_none()); // data cut short
        assert!(parse_key_frame(&f[..OFF_DATA - 1]).is_none());
    }

    #[test]
    fn mic() {
        let kck = [3u8; 16];
        let f = build_key_frame(&kck, KI_PAIRWISE | KI_MIC, &[0; 8], &[1; 32], &[]);
        assert!(verify_mic(&kck, &parse_key_frame(&f).unwrap()));
        assert!(!verify_mic(&[4; 16], &parse_key_frame(&f).unwrap()));
        let mut bad = f.clone();
        bad[20] ^= 1; // the nonce
        assert!(!verify_mic(&kck, &parse_key_frame(&bad).unwrap()));
    }

    #[test]
    fn gtk_kde() {
        // The RSN element, the GTK KDE (key id 2), then padding.
        let mut d = rsn_ie([0x00, 0x0f, 0xac, 4]);
        d.extend_from_slice(&[0xdd, 22, 0x00, 0x0f, 0xac, 0x01, 2, 0]);
        d.extend_from_slice(&[7; 16]);
        d.extend_from_slice(&[0xdd, 0, 0, 0]);
        assert_eq!(find_gtk(&d), Some((2, vec![7; 16])));
        assert_eq!(find_gtk(&d[..d.len() - 8]), None); // the KDE cut short
        assert_eq!(find_gtk(&[0xdd, 0, 0xdd, 6, 0x00, 0x0f, 0xac, 0x01, 1, 0]), None); // after padding
    }
}
