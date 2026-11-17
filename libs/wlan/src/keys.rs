//! The station's keys: the WPA2 4-way handshake that makes them
//! ([`Handshake`]), the group key handshake that renews the group key
//! ([`group_message`]), and their use on received data frames ([`RxKeys`]:
//! which key, and the replay check).
//!
//! No waiting and no I/O: [`crate::station`] receives the EAPOL-Key frames,
//! hands them here, and sends what comes back.

use alloc::vec::Vec;

use crate::ieee80211::{self as w, Hdr};
use crate::radio::{KeyMode, RxCrypto};
use crate::wpa::{self, KeyFrame};
use crate::{Error, Result, ccmp};

/// A group key: its key id, the key, and the receive sequence counter the
/// access point gave with it (the last packet number it sent with it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gtk {
    pub id: u8,
    pub key: [u8; 16],
    pub rsc: u64,
}

/// What a completed 4-way handshake gives.
#[derive(Clone)]
pub struct Keys {
    /// Key confirmation and key encryption: for the EAPOL-Key frames that
    /// follow (group key renewals).
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    /// The pairwise key: unicast data frames, both ways.
    pub tk: [u8; 16],
    /// The group key, if message 3 carried one.
    pub gtk: Option<Gtk>,
}

/// The station's side of the 4-way handshake (IEEE 802.11-2016 12.7.6):
/// message 1 gives the access point's nonce, from which both sides derive
/// the pairwise keys; message 2 sends ours; message 3 proves the access
/// point has the same keys and brings the group key; message 4 confirms.
pub struct Handshake {
    pmk: [u8; 32],
    aa: [u8; 6],
    spa: [u8; 6],
    snonce: [u8; 32],
    rsn: Vec<u8>,
    /// From message 1: the PTK, and the key descriptor version the access
    /// point uses (messages 2 and 4 use the same).
    ptk: Option<(wpa::Ptk, u16)>,
}

impl Handshake {
    /// `aa`: the access point's address (the BSSID); `spa`: ours; `snonce`:
    /// fresh randomness; `rsn`: the RSN element sent in the association
    /// request (message 2 repeats it).
    pub fn new(pmk: [u8; 32], aa: [u8; 6], spa: [u8; 6], snonce: [u8; 32], rsn: Vec<u8>) -> Handshake {
        Handshake { pmk, aa, spa, snonce, rsn, ptk: None }
    }

    /// Message 1: from the access point (ACK), without a MIC.
    pub fn is_message1(k: &KeyFrame) -> bool {
        k.info & wpa::KI_ACK != 0 && k.info & wpa::KI_MIC == 0
    }

    /// Answer message 1: the pairwise keys from its nonce and ours; returns
    /// message 2.
    pub fn message2(&mut self, m1: &KeyFrame) -> Vec<u8> {
        let ptk = wpa::derive_ptk(&self.pmk, &self.aa, &self.spa, &m1.nonce, &self.snonce);
        let ver = m1.info & 7;
        let m2 = wpa::build_key_frame(
            &ptk.kck,
            ver | wpa::KI_PAIRWISE | wpa::KI_MIC,
            &m1.replay,
            &self.snonce,
            &self.rsn,
        );
        self.ptk = Some((ptk, ver));
        m2
    }

    /// Message 3: Install set, and a MIC made with our KCK (so false before
    /// message 2, and for an access point with another passphrase).
    pub fn is_message3(&self, k: &KeyFrame) -> bool {
        self.ptk
            .as_ref()
            .is_some_and(|(p, _)| k.info & wpa::KI_INSTALL != 0 && wpa::verify_mic(&p.kck, k))
    }

    /// Answer message 3 (one [`Self::is_message3`] accepted): the group key
    /// from its key data; returns message 4 and the keys.
    pub fn message4(&self, m3: &KeyFrame) -> Result<(Vec<u8>, Keys)> {
        let (ptk, ver) = self.ptk.as_ref().ok_or(Error::Hw("message 3 before message 1"))?;
        let keydata = if m3.info & wpa::KI_ENC_DATA != 0 {
            wpa::aes_unwrap(&ptk.kek, m3.data).ok_or(Error::Hw("could not decrypt key data"))?
        } else {
            m3.data.to_vec()
        };
        let gtk = wpa::find_gtk(&keydata)
            .and_then(|(id, key)| Some(Gtk { id, key: key.try_into().ok()?, rsc: m3.rsc }));
        let m4 = wpa::build_key_frame(
            &ptk.kck,
            ver | wpa::KI_PAIRWISE | wpa::KI_MIC | wpa::KI_SECURE,
            &m3.replay,
            &[0; 32],
            &[],
        );
        Ok((m4, Keys { kck: ptk.kck, kek: ptk.kek, tk: ptk.tk, gtk }))
    }
}

/// The group key handshake, message 1 of 2, while connected: a group (not
/// pairwise) EAPOL-Key frame with our MIC and a new group key in its
/// encrypted key data. Returns the new key and message 2.
pub fn group_message(keys: &Keys, k: &KeyFrame) -> Option<(Gtk, Vec<u8>)> {
    if k.info & wpa::KI_PAIRWISE != 0 || !wpa::verify_mic(&keys.kck, k) {
        return None;
    }
    let data = wpa::aes_unwrap(&keys.kek, k.data)?;
    let (id, key) = wpa::find_gtk(&data)?;
    let gtk = Gtk { id, key: key.try_into().ok()?, rsc: k.rsc };
    let reply = wpa::build_key_frame(
        &keys.kck,
        (k.info & 7) | wpa::KI_MIC | wpa::KI_SECURE,
        &k.replay,
        &[0; 32],
        &[],
    );
    Some((gtk, reply))
}

/// The keys for received data frames, and the last packet number seen with
/// each: a frame whose packet number isn't higher is a replay.
pub struct RxKeys {
    mode: KeyMode,
    tk: Option<[u8; 16]>,
    gtk: Option<[u8; 16]>,
    pairwise_pn: u64,
    group_pn: u64,
}

impl RxKeys {
    pub fn new(mode: KeyMode) -> RxKeys {
        RxKeys { mode, tk: None, gtk: None, pairwise_pn: 0, group_pn: 0 }
    }

    /// The keys of a completed handshake.
    pub fn install(&mut self, keys: &Keys) {
        self.tk = Some(keys.tk);
        self.pairwise_pn = 0;
        if let Some(g) = &keys.gtk {
            self.set_gtk(g);
        }
    }

    /// A new group key: frames with it count from its RSC.
    pub fn set_gtk(&mut self, g: &Gtk) {
        self.gtk = Some(g.key);
        self.group_pn = g.rsc;
    }

    /// A data frame for us (the caller has checked the addresses) as an
    /// Ethernet frame: plain, decrypted by the chip (`crypto`), or
    /// decrypted here: group frames, and everything unless the chip
    /// decrypts unicast frames ([`KeyMode::Hardware`]). None for
    /// a frame that fails its MIC, has no key, or is a replay; and, once the
    /// keys are installed, for a plain frame other than EAPOL (anyone in
    /// range can send those; EAPOL stays allowed, as Linux allows it, and
    /// carries its own MIC).
    pub fn open(&mut self, frame: &[u8], h: &Hdr, crypto: RxCrypto) -> Option<Vec<u8>> {
        if !h.protected() {
            let eth = w::data_80211_to_eth(frame, 0, 0)?;
            return (self.tk.is_none() || w::is_eapol(&eth)).then_some(eth);
        }
        let group = h.group();
        match crypto {
            RxCrypto::Failed => None,
            RxCrypto::Decrypted { mic_len } => {
                // The CCMP header is still there: check the PN against replays.
                let pn = ccmp::pn(frame)?;
                let last = if group { &mut self.group_pn } else { &mut self.pairwise_pn };
                if pn <= *last {
                    return None;
                }
                *last = pn;
                w::data_80211_to_eth(frame, 8, mic_len)
            }
            RxCrypto::None => {
                let (key, last) = if group {
                    (self.gtk.as_ref()?, &mut self.group_pn)
                } else if self.mode != KeyMode::Hardware {
                    (self.tk.as_ref()?, &mut self.pairwise_pn)
                } else {
                    return None; // the chip should have decrypted it
                };
                let pn = ccmp::pn(frame)?;
                if pn <= *last {
                    return None; // replay
                }
                let plain = ccmp::decrypt(key, frame)?;
                *last = pn;
                w::data_80211_to_eth(&plain, 0, 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wpa::{OFF_MIC, OFF_RSC};
    use aes::Aes128;
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    use alloc::vec;

    const AP: [u8; 6] = [2, 0, 0, 0, 0, 0xa0];
    const ME: [u8; 6] = [2, 0, 0, 0, 0, 0x01];
    const VER: u16 = wpa::KI_TYPE_HMAC_SHA1_AES;

    /// RFC 3394 key wrap: what the access point does to the key data.
    fn aes_wrap(kek: &[u8; 16], plain: &[u8]) -> Vec<u8> {
        let cipher = Aes128::new(&(*kek).into());
        let n = plain.len() / 8;
        let mut a = [0xa6u8; 8];
        let mut r: Vec<[u8; 8]> = plain.chunks(8).map(|c| c.try_into().unwrap()).collect();
        for j in 0..6 {
            for i in 1..=n {
                let mut b = [0u8; 16];
                b[..8].copy_from_slice(&a);
                b[8..].copy_from_slice(&r[i - 1]);
                let mut block = aes::Block::from(b);
                cipher.encrypt_block(&mut block);
                let t = ((n * j + i) as u64).to_be_bytes();
                for k in 0..8 {
                    a[k] = block[k] ^ t[k];
                }
                r[i - 1].copy_from_slice(&block[8..]);
            }
        }
        let mut out = a.to_vec();
        out.extend(r.concat());
        out
    }

    #[test]
    fn wrap_matches_rfc3394() {
        let kek: [u8; 16] = core::array::from_fn(|i| i as u8);
        let plain: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        let w = aes_wrap(&kek, &plain);
        assert_eq!(&w[..4], &[0x1f, 0xa6, 0x8b, 0x0a]);
        assert_eq!(wpa::aes_unwrap(&kek, &w).as_deref(), Some(&plain[..]));
    }

    /// Key data with a GTK KDE (key id `id`), padded as an access point
    /// does before wrapping it.
    fn gtk_key_data(id: u8, gtk: &[u8; 16]) -> Vec<u8> {
        let mut d = wpa::rsn_ie([0x00, 0x0f, 0xac, 4]);
        d.extend_from_slice(&[0xdd, 22, 0x00, 0x0f, 0xac, 0x01, id, 0]);
        d.extend_from_slice(gtk);
        d.push(0xdd);
        while d.len() % 8 != 0 {
            d.push(0);
        }
        d
    }

    /// An EAPOL-Key frame from the access point: signed with `kck`, or
    /// with no MIC at all.
    fn ap_frame(kck: Option<&[u8; 16]>, info: u16, replay: u64, nonce: &[u8; 32], rsc: u64, data: &[u8]) -> Vec<u8> {
        let mut f = wpa::build_key_frame(&[0; 16], info, &replay.to_be_bytes(), nonce, data);
        f[OFF_RSC..OFF_RSC + 8].copy_from_slice(&rsc.to_le_bytes());
        match kck {
            Some(k) => wpa::sign(k, &mut f),
            None => f[OFF_MIC..OFF_MIC + 16].fill(0),
        }
        f
    }

    fn parse(f: &[u8]) -> KeyFrame<'_> {
        wpa::parse_key_frame(f).unwrap()
    }

    /// A 4-way handshake against an access point with `ap_passphrase`.
    /// Returns the station's keys and the access point's PTK.
    fn handshake(ap_passphrase: &str) -> (Option<Keys>, wpa::Ptk, Vec<u8>) {
        let pmk = wpa::pmk("correct horse", b"home");
        let ap_pmk = wpa::pmk(ap_passphrase, b"home");
        let anonce = [0x5a; 32];
        let rsn = wpa::rsn_ie([0x00, 0x0f, 0xac, 4]);
        let mut hs = Handshake::new(pmk, AP, ME, [0x33; 32], rsn.clone());

        let m1 = ap_frame(None, VER | wpa::KI_PAIRWISE | wpa::KI_ACK, 1, &anonce, 0, &[]);
        assert!(Handshake::is_message1(&parse(&m1)));
        let m2 = hs.message2(&parse(&m1));

        // The access point derives the same keys from message 2's nonce.
        let k2 = parse(&m2);
        assert_eq!(k2.nonce, [0x33; 32]);
        assert_eq!(k2.replay, 1u64.to_be_bytes());
        assert_eq!(k2.data, &rsn[..]);
        assert_eq!(k2.info, VER | wpa::KI_PAIRWISE | wpa::KI_MIC);
        let ap = wpa::derive_ptk(&ap_pmk, &AP, &ME, &anonce, &k2.nonce);
        if ap_passphrase == "correct horse" {
            assert!(wpa::verify_mic(&ap.kck, &k2));
        } else {
            assert!(!wpa::verify_mic(&ap.kck, &k2));
        }

        let gtk = [0x77; 16];
        let wrapped = aes_wrap(&ap.kek, &gtk_key_data(1, &gtk));
        let info = VER | wpa::KI_PAIRWISE | wpa::KI_ACK | wpa::KI_MIC | wpa::KI_INSTALL | wpa::KI_SECURE | wpa::KI_ENC_DATA;
        let m3 = ap_frame(Some(&ap.kck), info, 2, &anonce, 0x0102_0304, &wrapped);
        assert!(!Handshake::is_message1(&parse(&m3)));
        if !hs.is_message3(&parse(&m3)) {
            return (None, ap, m3);
        }
        let (m4, keys) = hs.message4(&parse(&m3)).unwrap();
        let k4 = parse(&m4);
        assert!(wpa::verify_mic(&ap.kck, &k4));
        assert_eq!(k4.info, VER | wpa::KI_PAIRWISE | wpa::KI_MIC | wpa::KI_SECURE);
        assert_eq!(k4.replay, 2u64.to_be_bytes());
        assert_eq!(k4.nonce, [0; 32]);
        (Some(keys), ap, m3)
    }

    #[test]
    fn four_way_handshake() {
        let (keys, ap, _) = handshake("correct horse");
        let keys = keys.unwrap();
        assert_eq!((keys.kck, keys.kek, keys.tk), (ap.kck, ap.kek, ap.tk));
        assert_eq!(keys.gtk, Some(Gtk { id: 1, key: [0x77; 16], rsc: 0x0102_0304 }));
    }

    #[test]
    fn another_passphrase_never_gets_to_message_4() {
        let (keys, _, _) = handshake("wrong horse");
        assert!(keys.is_none());
    }

    #[test]
    fn message_3_needs_message_1_first() {
        let (_, _, m3) = handshake("correct horse");
        let hs = Handshake::new([0; 32], AP, ME, [0x33; 32], vec![]);
        assert!(!hs.is_message3(&parse(&m3)));
        assert!(hs.message4(&parse(&m3)).is_err());
    }

    #[test]
    fn group_key_renewal() {
        let (keys, ap, _) = handshake("correct horse");
        let keys = keys.unwrap();
        let wrapped = aes_wrap(&ap.kek, &gtk_key_data(2, &[0x88; 16]));
        let info = VER | wpa::KI_ACK | wpa::KI_MIC | wpa::KI_SECURE | wpa::KI_ENC_DATA;
        let g1 = ap_frame(Some(&ap.kck), info, 3, &[0; 32], 42, &wrapped);
        let (gtk, reply) = group_message(&keys, &parse(&g1)).unwrap();
        assert_eq!(gtk, Gtk { id: 2, key: [0x88; 16], rsc: 42 });
        let r = parse(&reply);
        assert!(wpa::verify_mic(&ap.kck, &r));
        assert_eq!(r.info, VER | wpa::KI_MIC | wpa::KI_SECURE);
        assert_eq!(r.replay, 3u64.to_be_bytes());

        // Not signed with our KCK; a pairwise frame; key data that doesn't unwrap.
        let forged = ap_frame(Some(&[9; 16]), info, 3, &[0; 32], 0, &wrapped);
        assert!(group_message(&keys, &parse(&forged)).is_none());
        let pairwise = ap_frame(Some(&ap.kck), info | wpa::KI_PAIRWISE, 3, &[0; 32], 0, &wrapped);
        assert!(group_message(&keys, &parse(&pairwise)).is_none());
        let garbled = ap_frame(Some(&ap.kck), info, 3, &[0; 32], 0, &[0x11; 32]);
        assert!(group_message(&keys, &parse(&garbled)).is_none());
    }

    /// A data frame from the access point: to us, or to the broadcast
    /// address; carrying an IPv4 payload.
    fn from_ap(group: bool) -> (Vec<u8>, Vec<u8>) {
        let mut hdr = vec![0u8; 24];
        hdr[0] = w::FC0_DATA;
        hdr[1] = w::FC1_FROM_DS;
        hdr[4..10].copy_from_slice(if group { &[0xff; 6] } else { &ME });
        hdr[10..16].copy_from_slice(&AP);
        hdr[16..22].copy_from_slice(&[2, 0, 0, 0, 0, 0x99]);
        let body = b"\xaa\xaa\x03\0\0\0\x08\x00hello".to_vec();
        (hdr, body)
    }

    fn encrypted(key: &[u8; 16], group: bool, pn: u64) -> Vec<u8> {
        let (hdr, body) = from_ap(group);
        ccmp::encrypt(key, &hdr, pn, if group { 1 } else { 0 }, &body)
    }

    fn open(rx: &mut RxKeys, f: &[u8], crypto: RxCrypto) -> Option<Vec<u8>> {
        rx.open(f, &Hdr::parse(f).unwrap(), crypto)
    }

    fn keys(gtk_rsc: u64) -> Keys {
        Keys { kck: [1; 16], kek: [2; 16], tk: [3; 16], gtk: Some(Gtk { id: 1, key: [4; 16], rsc: gtk_rsc }) }
    }

    #[test]
    fn software_keys_and_replays() {
        let mut rx = RxKeys::new(KeyMode::Software);
        let f5 = encrypted(&[3; 16], false, 5);
        assert_eq!(open(&mut rx, &f5, RxCrypto::None), None); // no keys yet
        rx.install(&keys(10));
        let eth = open(&mut rx, &f5, RxCrypto::None).unwrap();
        assert_eq!(&eth[0..6], &ME);
        assert_eq!(&eth[6..12], &[2, 0, 0, 0, 0, 0x99]);
        assert_eq!(&eth[12..], b"\x08\x00hello");
        assert_eq!(open(&mut rx, &f5, RxCrypto::None), None); // the same again
        assert_eq!(open(&mut rx, &encrypted(&[3; 16], false, 4), RxCrypto::None), None);
        assert!(open(&mut rx, &encrypted(&[3; 16], false, 6), RxCrypto::None).is_some());
        // A frame that fails its MIC doesn't move the counter.
        let mut bad = encrypted(&[3; 16], false, 9);
        bad[40] ^= 1;
        assert_eq!(open(&mut rx, &bad, RxCrypto::None), None);
        assert!(open(&mut rx, &encrypted(&[3; 16], false, 7), RxCrypto::None).is_some());
        assert_eq!(open(&mut rx, &f5, RxCrypto::Failed), None);

        // Group frames count from the RSC message 3 gave, apart from unicast.
        assert_eq!(open(&mut rx, &encrypted(&[4; 16], true, 10), RxCrypto::None), None);
        assert!(open(&mut rx, &encrypted(&[4; 16], true, 11), RxCrypto::None).is_some());
        assert!(open(&mut rx, &encrypted(&[3; 16], false, 8), RxCrypto::None).is_some());
        // A renewed group key starts from its own RSC.
        rx.set_gtk(&Gtk { id: 2, key: [5; 16], rsc: 0 });
        assert!(open(&mut rx, &encrypted(&[5; 16], true, 1), RxCrypto::None).is_some());
        assert_eq!(open(&mut rx, &encrypted(&[4; 16], true, 12), RxCrypto::None), None); // the old key
    }

    #[test]
    fn hardware_keys() {
        let mut rx = RxKeys::new(KeyMode::Hardware);
        rx.install(&keys(0));
        // A unicast frame the chip left encrypted: not ours to decrypt.
        assert_eq!(open(&mut rx, &encrypted(&[3; 16], false, 1), RxCrypto::None), None);
        // Decrypted by the chip: CCMP header and MIC still in place.
        let chip = |pn: u64| {
            let f = encrypted(&[3; 16], false, pn);
            let (_, body) = from_ap(false);
            let mut d = f[..32].to_vec();
            d.extend_from_slice(&body);
            d.extend_from_slice(&[0; 8]);
            d
        };
        let eth = open(&mut rx, &chip(3), RxCrypto::Decrypted { mic_len: 8 }).unwrap();
        assert_eq!(&eth[12..], b"\x08\x00hello");
        assert_eq!(open(&mut rx, &chip(3), RxCrypto::Decrypted { mic_len: 8 }), None);
        assert!(open(&mut rx, &chip(4), RxCrypto::Decrypted { mic_len: 8 }).is_some());
        // Group frames are still decrypted here.
        assert!(open(&mut rx, &encrypted(&[4; 16], true, 1), RxCrypto::None).is_some());
    }

    #[test]
    fn hardware_tx_keys_decrypt_here() {
        // The chip only encrypts: unicast frames arrive as on the air,
        // and are decrypted and replay-checked here.
        let mut rx = RxKeys::new(KeyMode::HardwareTx);
        rx.install(&keys(0));
        let eth = open(&mut rx, &encrypted(&[3; 16], false, 5), RxCrypto::None).unwrap();
        assert_eq!(&eth[12..], b"\x08\x00hello");
        assert_eq!(open(&mut rx, &encrypted(&[3; 16], false, 5), RxCrypto::None), None);
        assert!(open(&mut rx, &encrypted(&[3; 16], false, 6), RxCrypto::None).is_some());
        assert!(open(&mut rx, &encrypted(&[4; 16], true, 1), RxCrypto::None).is_some());
    }

    #[test]
    fn plain_frames_only_before_the_keys() {
        let mut rx = RxKeys::new(KeyMode::Software);
        let (mut ip, body) = from_ap(false);
        ip.extend_from_slice(&body);
        let mut eapol = ip.clone();
        eapol[30..32].copy_from_slice(&[0x88, 0x8e]); // the ethertype after LLC/SNAP
        assert!(open(&mut rx, &ip, RxCrypto::None).is_some());
        assert!(open(&mut rx, &eapol, RxCrypto::None).is_some());
        rx.install(&keys(0));
        assert_eq!(open(&mut rx, &ip, RxCrypto::None), None);
        assert!(open(&mut rx, &eapol, RxCrypto::None).is_some());
        let mut group = ip.clone();
        group[4..10].fill(0xff);
        assert_eq!(open(&mut rx, &group, RxCrypto::None), None);
    }
}
