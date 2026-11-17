//! What one bulk-OUT transfer holds (`run_tx` in OpenBSD's if_run.c;
//! the padding after the header as Linux's rt2800 does it), without the
//! USB part, so it is tested on the host:
//!
//! | TXINFO (4) | TXWI (16) | 802.11 header | L2PAD | body | pad to 4 | 4 zero bytes |
//!
//! TXINFO holds the length of what follows it up to the last padding,
//! and the chip queue (EDCA); TXWI the PHY mode and rate, whether to wait
//! for an ACK, the station-table entry and the frame's length (without
//! the L2PAD). A frame the chip encrypts (hardware CCMP) names the access
//! point's entry, which holds the pairwise key, and brings its CCMP
//! header (the PN is the station's) in TXWI's IV words; the chip inserts
//! that header and appends the MIC. Every other frame names no entry and
//! goes as it is. The 4 zero bytes are an empty TXINFO: no further frame
//! in the transfer. The frame goes without FCS (the chip adds it).
//!
//! **L2PAD.** The chip wants the body 4-byte aligned: after a header
//! whose length isn't a multiple of 4 (QoS data: 26 bytes) come 2 bytes of
//! padding, which it drops on the air. Linux sets `REQUIRE_L2PAD` for the
//! whole rt2800 family and pads the same way; TXWI's length doesn't count
//! the padding.

use touchos_wlan::ieee80211::header_len;
use touchos_wlan::rates::{self, Rate};

use crate::regs::*;

pub const TXINFO_SIZE: usize = 4;
/// The RT5390's TXWI (the RT5592's is 4 bytes longer).
pub const TXWI_SIZE: usize = 16;

/// The PHY word (TXWI bytes 2-3: mode, MCS, bandwidth, guard interval):
/// a legacy rate's mode and index (`rt2860_rates` in rt2860reg.h), with
/// a short preamble for CCK when asked; an HT MCS in mixed mode at 20 MHz,
/// with a short guard interval when asked. None for a rate the chip
/// doesn't have.
pub fn phy_word(rate: Rate, short_preamble: bool, sgi: bool) -> Option<u16> {
    match rate {
        Rate::Legacy(r) => {
            // The chip's index is the rate's place in its modulation's list.
            let r = rates::value(r);
            if let Some(i) = rates::CCK.iter().position(|&x| x == r) {
                // Short preamble: not at 1 Mbit/s (it has none).
                let sp = if short_preamble && r != 2 { PHY_SHPRE } else { 0 };
                return Some(PHY_CCK | i as u16 | sp);
            }
            let i = rates::OFDM.iter().position(|&x| x == r)?;
            Some(PHY_OFDM | i as u16)
        }
        // One spatial stream: MCS 0-7.
        Rate::Mcs(m) if m < 8 => Some(PHY_HT_MIX | m as u16 | if sgi { PHY_SGI } else { 0 }),
        Rate::Mcs(_) => None,
    }
}

/// How one frame goes out.
#[derive(Debug, Clone, Copy)]
pub struct Desc {
    /// [`phy_word`].
    pub phy: u16,
    /// Unicast: the chip waits for the ACK and retries without one.
    pub ack: bool,
    /// Its sequence number (fragment 0), for a management or data frame.
    pub seq: u16,
    /// The chip encrypts it with the access point's pairwise key and this
    /// CCMP header.
    pub ccmp: Option<[u8; 8]>,
}

/// The bytes of padding after a header of `hdr` bytes.
fn l2pad(frame: &[u8], hdr: usize) -> usize {
    if frame.len() > hdr { hdr.wrapping_neg() & 3 } else { 0 }
}

/// The transfer's length for `frame` (`max_packet`: the endpoint's
/// packet size), or None if it doesn't fit in `room` bytes.
pub fn transfer_len(frame: &[u8], max_packet: usize, room: usize) -> Option<usize> {
    let hdr = frame_header_len(frame);
    let pkt = (TXWI_SIZE + frame.len() + l2pad(frame, hdr) + 3) & !3;
    let mut total = TXINFO_SIZE + pkt + 4;
    // A transfer of whole packets has no short packet to end it. OpenBSD
    // sends a zero-length packet then; the chip reads the lengths in
    // TXINFO and doesn't need one (rt2800usb sends none), and another 4
    // zero bytes (one more empty TXINFO) end it as well.
    if total.is_multiple_of(max_packet) {
        total += 4;
    }
    (total <= room).then_some(total)
}

/// A management or data frame's header length; a control frame has no
/// sequence number and needs no padding: its whole length.
fn frame_header_len(frame: &[u8]) -> usize {
    match frame {
        [fc0, fc1, ..] if (fc0 >> 2) & 3 != 1 => header_len(*fc0, *fc1).min(frame.len()),
        _ => frame.len(),
    }
}

/// Write the transfer for `frame` into `buf` (at least
/// [`transfer_len`] bytes); returns its length.
pub fn write(buf: &mut [u8], frame: &[u8], d: &Desc, max_packet: usize) -> Option<usize> {
    let total = transfer_len(frame, max_packet, buf.len())?;
    let hdr = frame_header_len(frame);
    let pad = l2pad(frame, hdr);
    let buf = &mut buf[..total];
    buf.fill(0);

    // TXINFO: TXWI + frame (with its L2PAD), rounded up to 4 bytes.
    let pkt_len = (TXWI_SIZE + frame.len() + pad + 3) & !3;
    buf[0..2].copy_from_slice(&(pkt_len as u16).to_le_bytes());
    // WIV: TXWI's IV words are ours: the CCMP header for a frame the
    // chip encrypts. OpenBSD leaves it clear and lets the chip take the
    // header from its IVEIV table, counting the PN there; this way the
    // PN stays the station's, one counter. (The header can't go in the
    // frame instead: tried on the Toon, the access point drops those.)
    buf[3] = TXINFO_QSEL_EDCA | TXINFO_WIV;

    // TXWI.
    let w = &mut buf[TXINFO_SIZE..TXINFO_SIZE + TXWI_SIZE];
    w[1] = TXWI_TXOP_BACKOFF;
    w[2..4].copy_from_slice(&d.phy.to_le_bytes());
    // No TXWI_NSEQ: we number the frames. OpenBSD lets the chip number
    // non-QoS frames, but Linux found that the chip counts on for
    // retransmissions of them, so the receiver can't spot duplicates.
    w[4] = if d.ack { TXWI_ACK } else { 0 };
    w[5] = if d.ccmp.is_some() { WCID_AP } else { WCID_NONE };
    w[6..8].copy_from_slice(&((frame.len() as u16) & 0xfff).to_le_bytes());
    if let Some(iv) = d.ccmp {
        w[8..16].copy_from_slice(&iv);
    }

    let at = TXINFO_SIZE + TXWI_SIZE;
    buf[at..at + hdr].copy_from_slice(&frame[..hdr]);
    buf[at + hdr + pad..at + frame.len() + pad].copy_from_slice(&frame[hdr..]);
    // Management and data frames (not control frames). CCMP leaves the
    // sequence number out of its MIC, so it can be set after the
    // encryption.
    if hdr >= 24 && (frame[0] >> 2) & 3 != 1 {
        buf[at + 22..at + 24].copy_from_slice(&((d.seq & 0xfff) << 4).to_le_bytes());
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phy_words() {
        assert_eq!(phy_word(Rate::Legacy(2), true, false), Some(PHY_CCK)); // no short preamble at 1M
        assert_eq!(phy_word(Rate::Legacy(22), true, false), Some(PHY_CCK | 3 | PHY_SHPRE));
        assert_eq!(phy_word(Rate::Legacy(0x8c), false, false), Some(PHY_OFDM)); // 6 Mbit/s basic
        assert_eq!(phy_word(Rate::Legacy(18), false, false), Some(PHY_OFDM | 1));
        assert_eq!(phy_word(Rate::Legacy(108), false, true), Some(PHY_OFDM | 7));
        assert_eq!(phy_word(Rate::Legacy(5), false, false), None);
        // HT mixed mode is 2 << 14, the short guard interval bit 8 (TXWI
        // word 0 bits 30-31 and 24 in Linux's rt2800.h).
        assert_eq!(phy_word(Rate::Mcs(7), false, true), Some(0x8000 | 0x0100 | 7));
        assert_eq!(phy_word(Rate::Mcs(0), true, false), Some(0x8000));
        assert_eq!(phy_word(Rate::Mcs(8), false, false), None);
    }

    fn data_frame(qos: bool, body: usize) -> Vec<u8> {
        let mut f = vec![if qos { 0x88 } else { 0x08 }, 0x41, 0, 0];
        f.extend_from_slice(&[0xaa; 18]);
        f.extend_from_slice(&[0, 0]);
        if qos {
            f.extend_from_slice(&[0, 0]);
        }
        f.extend((0..body).map(|i| i as u8));
        f
    }

    #[test]
    fn legacy_data_frame() {
        let f = data_frame(false, 10);
        let mut buf = [0xeeu8; 128];
        let d = Desc { phy: PHY_OFDM | 4, ack: true, seq: 0x123, ccmp: Some([1, 2, 0, 0x20, 3, 4, 5, 6]) };
        let n = write(&mut buf, &f, &d, 512).unwrap();
        // 4 + (16 + 34 -> 52) + 4.
        assert_eq!(n, 60);
        assert_eq!(&buf[..4], &[52, 0, 0, TXINFO_QSEL_EDCA | TXINFO_WIV]);
        assert_eq!(&buf[4..12], &[0, TXWI_TXOP_BACKOFF, 4, 0x40, TXWI_ACK, WCID_AP, 34, 0]);
        assert_eq!(&buf[12..20], &[1, 2, 0, 0x20, 3, 4, 5, 6]);
        assert_eq!(&buf[20..42], &f[..22]);
        assert_eq!(&buf[42..44], &(0x123u16 << 4).to_le_bytes());
        assert_eq!(&buf[44..54], &f[24..]); // no L2PAD after 24 bytes
        assert_eq!(&buf[54..60], &[0; 6]);
    }

    #[test]
    fn qos_data_frame_gets_l2pad() {
        let f = data_frame(true, 10);
        let mut buf = [0xeeu8; 128];
        let d = Desc { phy: phy_word(Rate::Mcs(5), false, true).unwrap(), ack: true, seq: 7, ccmp: None };
        let n = write(&mut buf, &f, &d, 512).unwrap();
        // TXWI 16 + header 26 + pad 2 + body 10 = 54 -> 56.
        assert_eq!(&buf[..2], &[56, 0]);
        assert_eq!(n, 4 + 56 + 4);
        // The length is the frame's, without the pad.
        assert_eq!(&buf[4 + 6..4 + 8], &[36, 0]);
        assert_eq!(buf[4 + 5], WCID_NONE);
        assert_eq!(&buf[4 + 2..4 + 4], &[5, 0x81]);
        let at = 20;
        assert_eq!(&buf[at..at + 22], &f[..22]);
        assert_eq!(&buf[at + 22..at + 24], &(7u16 << 4).to_le_bytes());
        assert_eq!(&buf[at + 24..at + 26], &[0, 0]); // QoS control
        assert_eq!(&buf[at + 26..at + 28], &[0, 0]); // L2PAD
        assert_eq!(&buf[at + 28..at + 38], &f[26..]);
        // A QoS Null (no body) gets no pad.
        let null = data_frame(true, 0);
        let n = write(&mut buf, &null, &d, 512).unwrap();
        assert_eq!((&buf[..2], n), (&[44u8, 0][..], 4 + 44 + 4));
    }

    #[test]
    fn whole_packets_get_four_more_bytes() {
        // 4 + 16 + 40 + 4 = 64: a whole packet of 64.
        let f = data_frame(false, 16);
        let mut buf = [0u8; 128];
        let d = Desc { phy: 0, ack: false, seq: 0, ccmp: None };
        assert_eq!(write(&mut buf, &f, &d, 64), Some(68));
        assert_eq!(write(&mut buf, &f, &d, 512), Some(64));
        assert_eq!(write(&mut buf[..63], &f, &d, 512), None);
    }
}
