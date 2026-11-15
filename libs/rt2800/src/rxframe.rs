//! The frames in a bulk-IN transfer from the chip (`run_rxeof` and
//! `run_rx_frame` in OpenBSD's if_run.c): pure parsing, tested on the
//! host. Everything in a transfer came over the air, so every length is
//! checked before it is used.
//!
//! One transfer can hold several frames (the chip aggregates, see
//! USB_DMA_CFG). Each is:
//!
//! | RXINFO (4) | RXWI (16) | 802.11 frame (+2 L2PAD) | pad to 4 | RXD (4) |
//!
//! RXINFO's low 16 bits are the length of RXWI + frame + padding; RXD
//! holds the flags (CRC error, L2PAD, ...). L2PAD: the chip put 2 bytes
//! between the 802.11 header and the body (to align the body).

use crate::regs::*;

const RXWI_SIZE: usize = 16;

/// A received frame, as handed to the caller.
pub struct Frame<'a> {
    /// The 802.11 frame (header and body, no FCS, L2PAD removed).
    pub data: &'a [u8],
    /// RXWI RSSI of antenna 0 (raw; see `Rt2800::rssi_dbm`).
    pub rssi: u8,
    /// RXWI PHY word: mode (CCK/OFDM/HT) and rate.
    pub phy: u16,
    /// RXD flags.
    pub flags: u32,
}

/// Counters, for the log.
#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub transfers: u32,
    pub frames: u32,
    pub crc_errors: u32,
    pub malformed: u32,
    pub usb_errors: u32,
}

/// The frames of one transfer.
pub(crate) fn parse(buf: &mut [u8], len: usize, st: &mut Stats, f: &mut impl FnMut(&Frame)) -> usize {
    let mut n = 0;
    let mut off = 0;
    let mut left = len;
    if left < 4 + RXWI_SIZE + 4 {
        if left > 0 {
            st.malformed += 1;
        }
        return 0;
    }
    while left > 8 {
        let dmalen = u16::from_le_bytes([buf[off], buf[off + 1]]) as usize;
        if dmalen == 0 || dmalen & 3 != 0 || dmalen + 8 > left {
            st.malformed += 1;
            break;
        }
        let at = off + 4;
        let rxd = u32::from_le_bytes(buf[at + dmalen..at + dmalen + 4].try_into().unwrap());
        if frame(&mut buf[at..at + dmalen], rxd, st, f) {
            n += 1;
        }
        off += dmalen + 8;
        left -= dmalen + 8;
    }
    n
}

/// One frame: `b` is RXWI + frame + padding.
fn frame(b: &mut [u8], rxd: u32, st: &mut Stats, f: &mut impl FnMut(&Frame)) -> bool {
    if b.len() < RXWI_SIZE {
        st.malformed += 1;
        return false;
    }
    let len = (u16::from_le_bytes([b[2], b[3]]) & 0xfff) as usize;
    let phy = u16::from_le_bytes([b[6], b[7]]);
    let rssi = b[8];
    if rxd & (RXD_CRCERR | RXD_ICVERR | RXD_MICERR) != 0 {
        st.crc_errors += 1;
        return false;
    }
    let pad = if rxd & RXD_L2PAD != 0 { 2 } else { 0 };
    if len < 10 || RXWI_SIZE + pad + len > b.len() {
        st.malformed += 1;
        return false;
    }
    let mut start = RXWI_SIZE;
    if pad != 0 {
        // Move the header up against the body, over the padding.
        let hdr = header_len(&b[RXWI_SIZE..]).min(len);
        b.copy_within(RXWI_SIZE..RXWI_SIZE + hdr, RXWI_SIZE + 2);
        start += 2;
    }
    st.frames += 1;
    f(&Frame { data: &b[start..start + len], rssi, phy, flags: rxd });
    true
}

/// The 802.11 header's length, for a frame of at least 10 bytes.
fn header_len(wh: &[u8]) -> usize {
    touchos_wlan::ieee80211::header_len(wh[0], wh[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// One aggregate entry: RXINFO, RXWI, the frame (with 2 bytes of
    /// L2PAD after the header when `l2pad`), padding, RXD.
    fn entry(frame: &[u8], rssi: u8, rxd: u32, l2pad: bool) -> Vec<u8> {
        let mut body = Vec::new();
        let mut rxwi = [0u8; RXWI_SIZE];
        rxwi[2..4].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        rxwi[8] = rssi;
        body.extend_from_slice(&rxwi);
        if l2pad {
            let hdr = header_len(frame);
            body.extend_from_slice(&frame[..hdr]);
            body.extend_from_slice(&[0xee, 0xee]);
            body.extend_from_slice(&frame[hdr..]);
        } else {
            body.extend_from_slice(frame);
        }
        while body.len() % 4 != 0 {
            body.push(0);
        }
        let mut e = Vec::new();
        e.extend_from_slice(&(body.len() as u32).to_le_bytes());
        e.extend_from_slice(&body);
        e.extend_from_slice(&(rxd | if l2pad { RXD_L2PAD } else { 0 }).to_le_bytes());
        e
    }

    /// A data frame: a 24-byte header, then `body`.
    fn data(body: &[u8]) -> Vec<u8> {
        let mut f = alloc::vec![0x08, 0x02];
        f.extend_from_slice(&[0; 22]);
        f.extend_from_slice(body);
        f
    }

    fn collect(buf: &mut [u8]) -> (usize, Vec<(Vec<u8>, u8)>, Stats) {
        let mut st = Stats::default();
        let mut got = Vec::new();
        let len = buf.len();
        let n = parse(buf, len, &mut st, &mut |f: &Frame| got.push((f.data.to_vec(), f.rssi)));
        (n, got, st)
    }

    #[test]
    fn an_aggregate() {
        let (a, b) = (data(b"hello"), data(b"world, longer"));
        let mut buf = entry(&a, 40, 0, false);
        buf.extend(entry(&b, 50, 0, true)); // with L2PAD: put back together
        let (n, got, st) = collect(&mut buf);
        assert_eq!(n, 2);
        assert_eq!(got, [(a, 40), (b, 50)]);
        assert_eq!((st.frames, st.malformed, st.crc_errors), (2, 0, 0));
    }

    #[test]
    fn bad_frames_are_counted_not_trusted() {
        let f = data(b"payload");
        let mut crc = entry(&f, 1, RXD_CRCERR, false);
        assert_eq!(collect(&mut crc).2.crc_errors, 1);
        // RXWI claims a longer frame than the entry holds.
        let mut long = entry(&f, 1, 0, false);
        long[4 + 2..4 + 4].copy_from_slice(&500u16.to_le_bytes());
        let (n, _, st) = collect(&mut long);
        assert_eq!((n, st.malformed), (0, 1));
        // RXINFO claims more than the transfer, or an odd length.
        let mut big = entry(&f, 1, 0, false);
        big[0..2].copy_from_slice(&4000u16.to_le_bytes());
        assert_eq!(collect(&mut big).2.malformed, 1);
        let mut odd = entry(&f, 1, 0, false);
        odd[0] += 1;
        assert_eq!(collect(&mut odd).2.malformed, 1);
        // Cut anywhere: no panic, and never more frames than whole ones.
        let mut two = entry(&f, 1, 0, true);
        two.extend(entry(&f, 2, 0, false));
        for cut in 0..two.len() {
            let mut b = two[..cut].to_vec();
            assert!(collect(&mut b).0 <= 1 || cut == two.len());
        }
    }
}
