//! 802.11 frame construction and parsing for a station: authentication,
//! association, and the data-frame <-> Ethernet conversion.

use alloc::vec::Vec;

pub const FC0_MGMT_AUTH: u8 = 0xb0;
pub const FC0_MGMT_ASSOC_REQ: u8 = 0x00;
pub const FC0_MGMT_ASSOC_RESP: u8 = 0x10;
pub const FC0_MGMT_DEAUTH: u8 = 0xc0;
pub const FC0_MGMT_DISASSOC: u8 = 0xa0;
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

/// Association request without HT/VHT (legacy rates only).
pub fn assoc_request(
    me: &[u8; 6],
    bssid: &[u8; 6],
    ssid: &[u8],
    capability: u16,
    rates: &[u8],
    rsn: &[u8],
) -> Vec<u8> {
    let mut f = mgmt_header(FC0_MGMT_ASSOC_REQ, bssid, me, bssid);
    // ESS + privacy + short preamble/slot as advertised by the AP.
    let cap = 0x0001 | (capability & (0x0010 | 0x0020 | 0x0400));
    f.extend_from_slice(&cap.to_le_bytes());
    f.extend_from_slice(&10u16.to_le_bytes()); // listen interval (beacons)
    f.push(0);
    f.push(ssid.len() as u8);
    f.extend_from_slice(ssid);
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
}

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

/// Reason code 3: deauthenticated because the sending station is leaving.
pub const REASON_LEAVING: u16 = 3;

/// Deauthentication: we are leaving, so the access point forgets us at
/// once instead of after its inactivity timeout. Some access points ignore
/// a new authentication from a station they still count as associated.
pub fn deauth(me: &[u8; 6], bssid: &[u8; 6], reason: u16) -> Vec<u8> {
    let mut f = mgmt_header(FC0_MGMT_DEAUTH, bssid, me, bssid);
    f.extend_from_slice(&reason.to_le_bytes());
    f
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

/// Ethernet frame -> 802.11 data frame (to-DS) ready for the radio.
pub fn eth_to_80211(eth: &[u8], me: &[u8; 6], bssid: &[u8; 6], protected: bool) -> Option<Vec<u8>> {
    if eth.len() < 14 {
        return None;
    }
    let (da, sa, ethertype, payload) = (&eth[0..6], &eth[6..12], &eth[12..14], &eth[14..]);
    let _ = sa; // a station always transmits with its own address
    let mut f = Vec::with_capacity(32 + payload.len());
    f.extend_from_slice(&[
        FC0_DATA,
        FC1_TO_DS | if protected { FC1_PROTECTED } else { 0 },
        0,
        0,
    ]);
    f.extend_from_slice(bssid);
    f.extend_from_slice(me);
    f.extend_from_slice(da);
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&LLC_SNAP);
    f.extend_from_slice(ethertype);
    f.extend_from_slice(payload);
    Some(f)
}

/// 802.11 data frame (from-DS) -> Ethernet frame. `ccmp_hdr` and
/// `ccmp_mic`: the bytes of CCMP header and MIC still in the frame (0 if
/// they were stripped).
pub fn data_80211_to_eth(frame: &[u8], ccmp_hdr: usize, ccmp_mic: usize) -> Option<Vec<u8>> {
    let h = Hdr::parse(frame).filter(Hdr::is_data)?;
    let (da, sa) = match h.fc1 & 3 {
        FC1_FROM_DS => (h.addr1, h.addr3),
        0 => (h.addr1, h.addr2),
        _ => return None,
    };
    let body = frame.get(h.len + ccmp_hdr..frame.len().checked_sub(ccmp_mic)?)?;
    if body.len() < 8 || body[..6] != LLC_SNAP {
        return None;
    }
    let mut eth = Vec::with_capacity(14 + body.len() - 8);
    eth.extend_from_slice(&da);
    eth.extend_from_slice(&sa);
    eth.extend_from_slice(&body[6..]);
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
    fn deauth_frame() {
        let (me, ap) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 9]);
        let f = deauth(&me, &ap, REASON_LEAVING);
        assert_eq!(f.len(), 26);
        let h = Hdr::parse(&f).unwrap();
        assert_eq!(h.mgmt_subtype(), Some(FC0_MGMT_DEAUTH));
        assert_eq!((h.addr1, h.addr2, h.addr3), (ap, me, ap));
        assert_eq!(reason(&f), Some(3));
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
        let mut f = eth_to_80211(&eth, &me, &ap, false).unwrap();
        assert_eq!(f[1], FC1_TO_DS);
        // Pretend the AP relays it back to us: from-DS, A1 = us, A3 = source.
        f[1] = FC1_FROM_DS;
        f[4..10].copy_from_slice(&me);
        f[10..16].copy_from_slice(&ap);
        f[16..22].copy_from_slice(&peer);
        let back = data_80211_to_eth(&f, 0, 0).unwrap();
        assert_eq!(&back[0..6], &me);
        assert_eq!(&back[6..12], &peer);
        assert_eq!(&back[12..], &eth[12..]);
    }
}
