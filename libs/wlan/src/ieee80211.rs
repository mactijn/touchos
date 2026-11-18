//! 802.11 frame construction and parsing for a station: authentication,
//! association, block-ack refusals, and the data-frame <-> Ethernet
//! conversion (QoS data and A-MSDUs included).

use alloc::vec::Vec;

use crate::scan::Network;

pub const FC0_MGMT_AUTH: u8 = 0xb0;
pub const FC0_MGMT_ASSOC_REQ: u8 = 0x00;
pub const FC0_MGMT_ASSOC_RESP: u8 = 0x10;
pub const FC0_MGMT_DEAUTH: u8 = 0xc0;
pub const FC0_MGMT_DISASSOC: u8 = 0xa0;
pub const FC0_MGMT_ACTION: u8 = 0xd0;
pub const FC0_DATA: u8 = 0x08;
pub const FC0_QOS_DATA: u8 = 0x88;
pub const FC1_TO_DS: u8 = 0x01;
pub const FC1_FROM_DS: u8 = 0x02;
pub const FC1_PROTECTED: u8 = 0x40;

const LLC_SNAP: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00];

fn mgmt_header(fc0: u8, da: &[u8; 6], sa: &[u8; 6], bssid: &[u8; 6]) -> Vec<u8> {
    let mut f = Vec::with_capacity(64);
    f.extend_from_slice(&[fc0, 0, 0, 0]); // fc, duration (firmware or set_duration)
    f.extend_from_slice(da);
    f.extend_from_slice(sa);
    f.extend_from_slice(bssid);
    f.extend_from_slice(&[0, 0]); // sequence control (filled by firmware)
    f
}

/// Open-system authentication request (sequence 1).
pub fn auth_request(me: &[u8; 6], bssid: &[u8; 6]) -> Vec<u8> {
    let mut f = mgmt_header(FC0_MGMT_AUTH, bssid, me, bssid);
    f.extend_from_slice(&[0, 0, 1, 0, 0, 0]); // algorithm 0, seq 1, status 0
    f
}

/// Our HT Capabilities element (802.11n, one spatial stream, 20 MHz),
/// with what Linux's rt2800 advertises for a chip with one receive chain:
/// - capability info: SM power save disabled, short GI at 20 MHz, receive
///   STBC of one stream; no 40 MHz, greenfield, LDPC or 7935-byte A-MSDUs;
/// - A-MPDU parameters: 32 KiB, 2 µs spacing (unused while we refuse
///   block-ack agreements, see [`addba_refusal`]);
/// - MCS 0-7 received and sent.
pub const HT_CAPS: [u8; 28] = [
    45, 26, //
    0x2c, 0x01, // capability info
    0x02 | 4 << 2, // A-MPDU parameters
    0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, // RX MCS 0-7
    0, 0, // highest RX rate: not given
    0x01, 0, 0, 0, // TX MCS set defined, the same as RX
    0, 0, // extended capabilities
    0, 0, 0, 0, // beamforming
    0, // antenna selection
];

/// The WMM Information element (WMM 1.1, no U-APSD): asks for a QoS
/// association.
pub const WMM_INFO: [u8; 9] = [221, 7, 0x00, 0x50, 0xf2, 0x02, 0x00, 0x01, 0x00];

/// Association request: the BSS's legacy rates, `rsn`, and, when `bss`
/// says so, WMM and HT Capabilities (the station clears `bss.ht` and
/// `bss.wmm` to associate as a legacy station).
pub fn assoc_request(me: &[u8; 6], bss: &Network, rsn: &[u8]) -> Vec<u8> {
    let bssid = &bss.bssid;
    let mut f = mgmt_header(FC0_MGMT_ASSOC_REQ, bssid, me, bssid);
    // ESS + privacy + short preamble/slot as advertised by the AP.
    let cap = 0x0001 | (bss.capability & (0x0010 | 0x0020 | 0x0400));
    f.extend_from_slice(&cap.to_le_bytes());
    f.extend_from_slice(&10u16.to_le_bytes()); // listen interval (beacons)
    f.push(0);
    f.push(bss.ssid_raw.len() as u8);
    f.extend_from_slice(&bss.ssid_raw);
    let rates = &bss.rates;
    let (basic, ext) = rates.split_at(rates.len().min(8));
    f.push(1);
    f.push(basic.len() as u8);
    f.extend_from_slice(basic);
    if !ext.is_empty() {
        f.push(50);
        f.push(ext.len() as u8);
        f.extend_from_slice(ext);
    }
    f.extend_from_slice(rsn);
    if bss.ht.is_some() && bss.wmm {
        f.extend_from_slice(&HT_CAPS);
    }
    if bss.wmm {
        f.extend_from_slice(&WMM_INFO);
    }
    f
}

/// The information elements of an association response (after its
/// capability, status and AID).
pub fn assoc_response_ies(frame: &[u8]) -> &[u8] {
    frame.get(30..).unwrap_or(&[])
}

/// Whether information elements hold a WMM Parameter element: in an
/// association response, the access point took us as a QoS station.
pub fn has_wmm_param(ies_: &[u8]) -> bool {
    ies(ies_).any(|(id, b)| id == 221 && b.len() >= 5 && b[..5] == [0x00, 0x50, 0xf2, 0x02, 0x01])
}

/// An ADDBA request (Block Ack action frame) from the access point: its
/// dialog token, block-ack parameters and timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddbaRequest {
    pub token: u8,
    pub params: u16,
    pub timeout: u16,
}

impl AddbaRequest {
    /// The TID the agreement would be for.
    pub fn tid(&self) -> u8 {
        (self.params >> 2 & 0x0f) as u8
    }
}

/// An action frame's ADDBA request (category 3 Block Ack, action 0).
pub fn addba_request(frame: &[u8]) -> Option<AddbaRequest> {
    let b = frame.get(24..33)?;
    (b[0] == 3 && b[1] == 0).then(|| AddbaRequest {
        token: b[2],
        params: u16::from_le_bytes([b[3], b[4]]),
        timeout: u16::from_le_bytes([b[5], b[6]]),
    })
}

/// "Request declined" (status 37): the ADDBA response that refuses a
/// block-ack agreement. Without one the access point sends no A-MPDUs, so
/// frames arrive one by one, in order, and need no reordering.
pub fn addba_refusal(me: &[u8; 6], bssid: &[u8; 6], req: &AddbaRequest) -> Vec<u8> {
    let mut f = mgmt_header(FC0_MGMT_ACTION, bssid, me, bssid);
    f.extend_from_slice(&[3, 1, req.token]);
    f.extend_from_slice(&37u16.to_le_bytes());
    f.extend_from_slice(&req.params.to_le_bytes());
    f.extend_from_slice(&req.timeout.to_le_bytes());
    f
}

/// The 802.11 header's length, from the frame control bytes
/// (`ieee80211_get_hdrlen`): management, control and data frames, four
/// addresses, QoS, HT control.
pub fn header_len(fc0: u8, fc1: u8) -> usize {
    let order = fc1 & 0x80 != 0;
    match (fc0 >> 2) & 3 {
        0 => 24 + if order { 4 } else { 0 },
        // Control frames: CTS and ACK have one address, the rest two.
        1 => match fc0 >> 4 {
            12 | 13 => 10,
            _ => 16,
        },
        _ => {
            let mut n = 24;
            if fc1 & 3 == 3 {
                n += 6; // four addresses (WDS)
            }
            if fc0 & 0x80 != 0 {
                n += 2; // QoS
                if order {
                    n += 4; // HT control
                }
            }
            n
        }
    }
}

/// The header of a management or data frame, as received: the frame
/// control bytes, the three addresses, and where the body starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hdr {
    pub fc0: u8,
    pub fc1: u8,
    /// The receiver (a group address for broadcast and multicast).
    pub addr1: [u8; 6],
    /// The transmitter: the access point, for frames from it.
    pub addr2: [u8; 6],
    pub addr3: [u8; 6],
    /// A QoS data frame's TID.
    pub tid: Option<u8>,
    /// A QoS data frame whose body is an A-MSDU (several MSDUs).
    pub amsdu: bool,
    /// The header's length ([`header_len`]): the body starts here.
    pub len: usize,
}

impl Hdr {
    /// None for control frames (and the reserved type), and for frames
    /// shorter than their header.
    pub fn parse(frame: &[u8]) -> Option<Hdr> {
        let (&fc0, &fc1) = (frame.first()?, frame.get(1)?);
        let kind = (fc0 >> 2) & 3;
        if kind != 0 && kind != 2 {
            return None;
        }
        let len = header_len(fc0, fc1);
        if frame.len() < len {
            return None;
        }
        let addr = |o: usize| -> [u8; 6] { frame[o..o + 6].try_into().unwrap() };
        let qos = kind == 2 && fc0 & 0x80 != 0;
        let qos_at = 24 + if fc1 & 3 == 3 { 6 } else { 0 };
        Some(Hdr {
            fc0,
            fc1,
            addr1: addr(4),
            addr2: addr(10),
            addr3: addr(16),
            tid: qos.then(|| frame[qos_at] & 0x0f),
            amsdu: qos && frame[qos_at] & 0x80 != 0,
            len,
        })
    }

    pub fn is_data(&self) -> bool {
        (self.fc0 >> 2) & 3 == 2
    }

    /// A management frame's type/subtype byte (`FC0_MGMT_*`).
    pub fn mgmt_subtype(&self) -> Option<u8> {
        ((self.fc0 >> 2) & 3 == 0).then_some(self.fc0 & 0xf0)
    }

    pub fn protected(&self) -> bool {
        self.fc1 & FC1_PROTECTED != 0
    }

    /// Sent to a group address (broadcast or multicast).
    pub fn group(&self) -> bool {
        self.addr1[0] & 1 != 0
    }

    /// A group frame the station passes on: to broadcast or the mDNS group
    /// (the only multicast the network stack listens to; the chip has no
    /// per-group filter), and not one of our own that the access point
    /// sends back to the BSS (from-DS with us as the source).
    pub fn wanted_group(&self, me: [u8; 6]) -> bool {
        let ours = self.fc1 & 3 == FC1_FROM_DS && self.addr3 == me;
        (self.addr1 == BROADCAST || self.addr1 == MDNS_GROUP) && !ours
    }
}

pub const BROADCAST: [u8; 6] = [0xff; 6];
/// 224.0.0.251's Ethernet address.
pub const MDNS_GROUP: [u8; 6] = [0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb];

/// Management frame type/subtype byte, or None for non-management frames.
pub fn mgmt_subtype(frame: &[u8]) -> Option<u8> {
    Hdr::parse(frame)?.mgmt_subtype()
}

/// The little-endian u16 at `off`, if the frame is long enough. The fixed
/// fields after the 24-byte header come from the air: a frame may end
/// anywhere.
fn le16(frame: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(frame.get(off..off + 2)?.try_into().ok()?))
}

/// An authentication frame's status code (body: algorithm, sequence,
/// status).
pub fn auth_status(frame: &[u8]) -> Option<u16> {
    le16(frame, 28)
}

/// An association response's (status code, association ID) (body:
/// capability, status, AID; the AID's top two bits are always set).
pub fn assoc_response(frame: &[u8]) -> Option<(u16, u16)> {
    Some((le16(frame, 26)?, le16(frame, 28)? & 0x3fff))
}

/// A deauthentication or disassociation frame's reason code.
pub fn reason(frame: &[u8]) -> Option<u16> {
    le16(frame, 24)
}

/// The information elements in `body` (a management frame's after its
/// fixed fields, or EAPOL-Key data): (element id, contents). Stops at the
/// first element that runs past the end, so a truncated frame yields what
/// is whole.
pub fn ies(body: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut rest = body;
    core::iter::from_fn(move || {
        let (&id, &len) = (rest.first()?, rest.get(1)?);
        let contents = rest.get(2..2 + len as usize)?;
        rest = &rest[2 + len as usize..];
        Some((id, contents))
    })
}

/// Ethernet frame -> 802.11 data frame (to-DS) ready for the radio. `qos`:
/// a QoS data frame (a QoS association), TID 0 (best effort) with normal
/// acknowledgement: everything the panel sends is best effort.
pub fn eth_to_80211(eth: &[u8], me: &[u8; 6], bssid: &[u8; 6], protected: bool, qos: bool) -> Option<Vec<u8>> {
    if eth.len() < 14 {
        return None;
    }
    let (da, sa, ethertype, payload) = (&eth[0..6], &eth[6..12], &eth[12..14], &eth[14..]);
    let _ = sa; // a station always transmits with its own address
    let mut f = Vec::with_capacity(34 + payload.len());
    f.extend_from_slice(&[
        if qos { FC0_QOS_DATA } else { FC0_DATA },
        FC1_TO_DS | if protected { FC1_PROTECTED } else { 0 },
        0,
        0,
    ]);
    f.extend_from_slice(bssid);
    f.extend_from_slice(me);
    f.extend_from_slice(da);
    f.extend_from_slice(&[0, 0]);
    if qos {
        f.extend_from_slice(&[0, 0]); // QoS control: TID 0, normal ack
    }
    f.extend_from_slice(&LLC_SNAP);
    f.extend_from_slice(ethertype);
    f.extend_from_slice(payload);
    Some(f)
}

/// 802.11 data frame (from-DS) -> Ethernet frames: one, or one per
/// MSDU of an A-MSDU; none for a frame that isn't data, has no LLC/SNAP
/// header (a QoS Null), or is broken. `ccmp_hdr` and `ccmp_mic`: the
/// bytes of CCMP header and MIC still in the frame (0 if they were
/// stripped).
pub fn data_80211_to_eth(frame: &[u8], ccmp_hdr: usize, ccmp_mic: usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let Some(h) = Hdr::parse(frame).filter(Hdr::is_data) else {
        return out;
    };
    let (da, sa) = match h.fc1 & 3 {
        FC1_FROM_DS => (h.addr1, h.addr3),
        0 => (h.addr1, h.addr2),
        _ => return out,
    };
    let Some(body) = frame
        .len()
        .checked_sub(ccmp_mic)
        .and_then(|end| frame.get(h.len + ccmp_hdr..end))
    else {
        return out;
    };
    if !h.amsdu {
        out.extend(msdu_to_eth(&da, &sa, body));
        return out;
    }
    // Subframes: DA, SA, length (big-endian), the MSDU; each but the last
    // padded to 4 bytes.
    let mut rest = body;
    while rest.len() >= 14 {
        let len = u16::from_be_bytes([rest[12], rest[13]]) as usize;
        let Some(msdu) = rest.get(14..14 + len) else {
            break; // cut off: what came before stands
        };
        let (da, sa): ([u8; 6], [u8; 6]) = (rest[0..6].try_into().unwrap(), rest[6..12].try_into().unwrap());
        out.extend(msdu_to_eth(&da, &sa, msdu));
        rest = rest.get((14 + len).next_multiple_of(4)..).unwrap_or(&[]);
    }
    out
}

/// An MSDU (LLC/SNAP, ethertype, payload) as an Ethernet frame.
fn msdu_to_eth(da: &[u8; 6], sa: &[u8; 6], msdu: &[u8]) -> Option<Vec<u8>> {
    if msdu.len() < 8 || msdu[..6] != LLC_SNAP {
        return None;
    }
    let mut eth = Vec::with_capacity(14 + msdu.len() - 8);
    eth.extend_from_slice(da);
    eth.extend_from_slice(sa);
    eth.extend_from_slice(&msdu[6..]);
    Some(eth)
}

/// Is this Ethernet payload an EAPOL frame (ethertype 0x888e)?
pub fn is_eapol(eth: &[u8]) -> bool {
    eth.len() > 14 && eth[12] == 0x88 && eth[13] == 0x8e
}

/// The duration field (microseconds) of a unicast frame that fits in one
/// transmission: SIFS plus the ACK, sent at `ack_rate` (500 kbit/s units).
/// The Intel firmware fills it in itself; a soft-MAC needs the host to, or
/// other stations don't hold off for our ACK. Same arithmetic as Linux's
/// ieee80211_frame_duration() for a 14-byte ACK.
pub fn ack_duration(ack_rate: u8, short_preamble: bool) -> u16 {
    const ACK_BITS: u32 = 8 * 14;
    let rate = (ack_rate & 0x7f).max(2) as u32;
    if matches!(rate, 2 | 4 | 11 | 22) {
        // DSSS/CCK: SIFS 10, PLCP preamble + header (short only above 1 Mbit/s).
        let plcp = if short_preamble && rate != 2 { 96 } else { 192 };
        (10 + plcp + (2 * ACK_BITS).div_ceil(rate)) as u16
    } else {
        // OFDM: SIFS (+ signal extension on 2.4 GHz) 16, preamble 16, SIGNAL 4,
        // then 4 us symbols of 2 * rate data bits (service + ACK + tail).
        (16 + 16 + 4 + 4 * (16 + ACK_BITS + 6).div_ceil(2 * rate)) as u16
    }
}

/// Write the duration field of a frame.
pub fn set_duration(frame: &mut [u8], us: u16) {
    frame[2..4].copy_from_slice(&us.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn ack_durations() {
        assert_eq!(ack_duration(2, false), 314); // 1 Mbit/s
        assert_eq!(ack_duration(2, true), 314); // no short preamble at 1 Mbit/s
        assert_eq!(ack_duration(4, true), 162); // 2 Mbit/s short
        assert_eq!(ack_duration(12, false), 60); // 6 Mbit/s
        assert_eq!(ack_duration(48, false), 44); // 24 Mbit/s
        assert_eq!(ack_duration(0x8c, false), 60); // basic-rate bit ignored
    }

    /// A management frame: the 24-byte header, then `body`.
    fn mgmt(fc0: u8, body: &[u8]) -> Vec<u8> {
        let mut f = mgmt_header(fc0, &[1; 6], &[2; 6], &[2; 6]);
        f.extend_from_slice(body);
        f
    }

    #[test]
    fn fixed_fields() {
        let auth = mgmt(FC0_MGMT_AUTH, &[0, 0, 2, 0, 17, 0]);
        assert_eq!(auth_status(&auth), Some(17));
        let resp = mgmt(FC0_MGMT_ASSOC_RESP, &[0x11, 0x04, 0, 0, 0x05, 0xc0, 1, 8]);
        assert_eq!(assoc_response(&resp), Some((0, 5)));
        let deauth = mgmt(FC0_MGMT_DEAUTH, &[7, 0]);
        assert_eq!(reason(&deauth), Some(7));
    }

    #[test]
    fn fixed_fields_of_short_frames() {
        // Frames that end right after the header, or inside the fields: what
        // anyone in radio range can send. None, never a panic.
        for n in 0..6 {
            let body = &[0u8; 6][..n];
            assert_eq!(auth_status(&mgmt(FC0_MGMT_AUTH, body)), None);
            assert_eq!(assoc_response(&mgmt(FC0_MGMT_ASSOC_RESP, body)), None);
        }
        assert_eq!(reason(&mgmt(FC0_MGMT_DEAUTH, &[])), None);
        assert_eq!(reason(&mgmt(FC0_MGMT_DEAUTH, &[3])), None);
        assert_eq!(reason(&[]), None);
    }

    #[test]
    fn header_lengths() {
        assert_eq!(header_len(0x80, 0), 24); // beacon
        assert_eq!(header_len(0xd4, 0), 10); // ACK
        assert_eq!(header_len(0xb4, 0), 16); // RTS
        assert_eq!(header_len(0x08, 0x02), 24); // data from the AP
        assert_eq!(header_len(0x88, 0x02), 26); // QoS data
        assert_eq!(header_len(0x88, 0x83), 36); // QoS, four addresses, HT control
    }

    #[test]
    fn headers() {
        let auth = mgmt(FC0_MGMT_AUTH, &[0, 0, 2, 0, 0, 0]);
        let h = Hdr::parse(&auth).unwrap();
        assert_eq!((h.addr1, h.addr2, h.addr3), ([1; 6], [2; 6], [2; 6]));
        assert_eq!((h.mgmt_subtype(), h.is_data(), h.len, h.tid), (Some(FC0_MGMT_AUTH), false, 24, None));
        assert_eq!(mgmt_subtype(&auth), Some(FC0_MGMT_AUTH));

        // QoS data from the AP to a multicast group, protected, TID 5.
        let mut qos = [0u8; 26];
        qos[0] = FC0_QOS_DATA;
        qos[1] = FC1_FROM_DS | FC1_PROTECTED;
        qos[4] = 0x01;
        qos[24] = 0x15;
        let h = Hdr::parse(&qos).unwrap();
        assert!(h.is_data() && h.protected() && h.group());
        assert_eq!((h.mgmt_subtype(), h.tid, h.len), (None, Some(5), 26));
        assert_eq!(mgmt_subtype(&qos), None);

        // Cut anywhere inside the header; control frames.
        for n in 0..26 {
            assert_eq!(Hdr::parse(&qos[..n]), None);
        }
        assert_eq!(Hdr::parse(&auth[..23]), None);
        assert_eq!(Hdr::parse(&[0xd4, 0, 0, 0, 1, 2, 3, 4, 5, 6]), None);
    }

    #[test]
    fn group_frames() {
        let me = [0x02, 0, 0, 0, 0, 9];
        let frame = |to: [u8; 6], from: [u8; 6]| {
            let mut f = [0u8; 24];
            f[0] = FC0_DATA;
            f[1] = FC1_FROM_DS;
            f[4..10].copy_from_slice(&to);
            f[10..16].copy_from_slice(&[7; 6]);
            f[16..22].copy_from_slice(&from);
            Hdr::parse(&f).unwrap()
        };
        assert!(frame(BROADCAST, [3; 6]).wanted_group(me));
        assert!(frame(MDNS_GROUP, [3; 6]).wanted_group(me));
        // Other groups (SSDP, IPv6), and our own sent back.
        assert!(!frame([0x01, 0, 0x5e, 0x7f, 0xff, 0xfa], [3; 6]).wanted_group(me));
        assert!(!frame([0x33, 0x33, 0, 0, 0, 1], [3; 6]).wanted_group(me));
        assert!(!frame(MDNS_GROUP, me).wanted_group(me));
        assert!(!frame(BROADCAST, me).wanted_group(me));
    }

    #[test]
    fn ie_walk() {
        let b = [0, 2, b'h', b'i', 3, 1, 6, 221, 0, 48, 9, 1];
        let v: Vec<(u8, &[u8])> = ies(&b).collect();
        assert_eq!(v, [(0, &b"hi"[..]), (3, &[6][..]), (221, &[][..])]); // 48 is cut off
        assert_eq!(ies(&[]).count(), 0);
        assert_eq!(ies(&[5]).count(), 0);
    }

    #[test]
    fn eth_round_trip() {
        let me = [2, 0, 0, 0, 0, 1];
        let ap = [2, 0, 0, 0, 0, 2];
        let peer = [2, 0, 0, 0, 0, 3];
        let mut eth = Vec::new();
        eth.extend_from_slice(&peer);
        eth.extend_from_slice(&me);
        eth.extend_from_slice(&[0x08, 0x00, 1, 2, 3, 4]);
        for qos in [false, true] {
            let mut f = eth_to_80211(&eth, &me, &ap, false, qos).unwrap();
            assert_eq!(f[1], FC1_TO_DS);
            let h = Hdr::parse(&f).unwrap();
            assert_eq!((h.len, h.tid), if qos { (26, Some(0)) } else { (24, None) });
            assert_eq!(f[0], if qos { FC0_QOS_DATA } else { FC0_DATA });
            // Pretend the AP relays it back to us: from-DS, A1 = us, A3 = source.
            f[1] = FC1_FROM_DS;
            f[4..10].copy_from_slice(&me);
            f[10..16].copy_from_slice(&ap);
            f[16..22].copy_from_slice(&peer);
            let back = data_80211_to_eth(&f, 0, 0);
            assert_eq!(back.len(), 1);
            assert_eq!(&back[0][0..6], &me);
            assert_eq!(&back[0][6..12], &peer);
            assert_eq!(&back[0][12..], &eth[12..]);
        }
    }

    #[test]
    fn amsdu() {
        let me = [2, 0, 0, 0, 0, 1];
        let mut f = vec![FC0_QOS_DATA, FC1_FROM_DS, 0, 0];
        f.extend_from_slice(&me);
        f.extend_from_slice(&[2; 6]); // the access point
        f.extend_from_slice(&[3; 6]);
        f.extend_from_slice(&[0, 0, 0x80, 0]); // sequence, QoS: A-MSDU, TID 0
        let sub = |f: &mut Vec<u8>, src: u8, payload: &[u8], last: bool| {
            f.extend_from_slice(&me);
            f.extend_from_slice(&[src; 6]);
            f.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
            f.extend_from_slice(&LLC_SNAP);
            f.extend_from_slice(&[0x08, 0x00]);
            f.extend_from_slice(payload);
            if !last {
                while f.len() % 4 != 26 % 4 {
                    f.push(0);
                }
            }
        };
        sub(&mut f, 4, &[1, 2, 3], false);
        sub(&mut f, 5, &[9; 40], true);
        let eth = data_80211_to_eth(&f, 0, 0);
        assert_eq!(eth.len(), 2);
        assert_eq!((&eth[0][..6], &eth[0][6..12], &eth[0][12..]), (&me[..], &[4; 6][..], &[8, 0, 1, 2, 3][..]));
        assert_eq!((eth[1][6], eth[1].len()), (5, 14 + 40));
        // Cut inside the second subframe: the first one stands.
        assert_eq!(data_80211_to_eth(&f[..f.len() - 1], 0, 0).len(), 1);
        // A QoS Null (no body) is no Ethernet frame.
        assert!(data_80211_to_eth(&f[..26], 0, 0).is_empty());
    }

    #[test]
    fn association_request() {
        let mut bss = crate::scan::Network {
            ssid: "home".into(),
            bssid: [2; 6],
            channel: 6,
            rssi: -50,
            security: "WPA2",
            ssid_raw: b"home".to_vec(),
            capability: 0x0431,
            beacon_interval: 100,
            dtim_period: 1,
            rates: vec![0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48],
            group_cipher: None,
            ccmp_psk: true,
            ht: None,
            wmm: false,
            erp_protection: false,
        };
        let me = [1; 6];
        let legacy = assoc_request(&me, &bss, &[48, 0]);
        let body = &legacy[28..];
        let ids: Vec<u8> = ies(body).map(|(id, _)| id).collect();
        assert_eq!(ids, [0, 1, 50, 48]);
        assert_eq!(&legacy[24..26], &0x0431u16.to_le_bytes());
        bss.wmm = true;
        bss.ht = Some(crate::scan::Ht { sgi20: true, rx_mcs: 0xff, protection: 0, non_greenfield: false });
        let ht = assoc_request(&me, &bss, &[48, 0]);
        let ids: Vec<u8> = ies(&ht[28..]).map(|(id, _)| id).collect();
        assert_eq!(ids, [0, 1, 50, 48, 45, 221]);
        assert!(ht.ends_with(&[&HT_CAPS[..], &WMM_INFO[..]].concat()));
        assert_eq!(HT_CAPS[1] as usize, HT_CAPS.len() - 2);
        // WMM without HT.
        bss.ht = None;
        let ids: Vec<u8> = ies(&assoc_request(&me, &bss, &[48, 0])[28..]).map(|(id, _)| id).collect();
        assert_eq!(ids, [0, 1, 50, 48, 221]);
    }

    #[test]
    fn wmm_in_association_response() {
        let mut resp = mgmt(FC0_MGMT_ASSOC_RESP, &[0x11, 0x04, 0, 0, 0x05, 0xc0, 1, 1, 0x82]);
        assert!(!has_wmm_param(assoc_response_ies(&resp)));
        resp.extend_from_slice(&[221, 24, 0x00, 0x50, 0xf2, 0x02, 0x01, 0x01]);
        resp.extend_from_slice(&[0; 18]);
        assert!(has_wmm_param(assoc_response_ies(&resp)));
        assert!(assoc_response_ies(&resp[..10]).is_empty());
    }

    #[test]
    fn addba_is_refused() {
        // TID 5, immediate block ack, 64 buffers; timeout 0, start sequence 16.
        let params: u16 = 1 << 1 | 5 << 2 | 64 << 6;
        let mut req = mgmt(FC0_MGMT_ACTION, &[3, 0, 9]);
        req.extend_from_slice(&params.to_le_bytes());
        req.extend_from_slice(&[0, 0, 0x00, 0x01]);
        let r = addba_request(&req).unwrap();
        assert_eq!((r.token, r.tid(), r.timeout), (9, 5, 0));
        let resp = addba_refusal(&[1; 6], &[2; 6], &r);
        assert_eq!(&resp[..2], &[FC0_MGMT_ACTION, 0]);
        assert_eq!((&resp[4..10], &resp[10..16], &resp[16..22]), (&[2; 6][..], &[1; 6][..], &[2; 6][..]));
        assert_eq!(&resp[24..], &[3, 1, 9, 37, 0, params as u8, (params >> 8) as u8, 0, 0]);
        // Other actions, and short frames.
        assert_eq!(addba_request(&mgmt(FC0_MGMT_ACTION, &[3, 2, 9, 0, 0, 0, 0, 0, 0])), None);
        assert_eq!(addba_request(&req[..30]), None);
    }
}
