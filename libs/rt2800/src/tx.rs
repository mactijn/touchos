//! Transmitting: one frame per bulk-OUT transfer (`run_tx` and
//! `run_txeof` in OpenBSD's if_run.c). A transfer is:
//!
//! | TXINFO (4) | TXWI (16) | 802.11 frame | pad to 4 | 4 zero bytes |
//!
//! TXINFO holds the length of TXWI + frame + padding and the chip queue
//! (EDCA); TXWI the PHY mode and rate, whether to wait for an ACK, the
//! station-table entry and the frame's length. A frame the chip encrypts
//! (hardware CCMP) names the access point's entry, which holds the
//! pairwise key, and brings its CCMP header (the PN is the station's) in
//! TXWI's IV words; the chip inserts that header and appends the MIC.
//! Every other frame names no entry and goes as it is. The 4 zero bytes are an empty TXINFO: no further frame in the
//! transfer. The frame goes without FCS (the chip adds it).
//!
//! A transfer completes when the chip has taken it over USB, not when it
//! went out on the air; whether the access point ACKed it is in the TX
//! statistics registers (`Rt2800::tx_counters`).

use alloc::vec::Vec;

use touchos_drv::mem::{self, Dma};
use touchos_rt::time;
use touchos_usb::{Device, HostController, UsbError};
use touchos_wlan::rates;

use crate::regs::*;

/// Transfers in flight at most (OpenBSD keeps 8 per queue; the station
/// sends little, and a full queue only means waiting a moment).
pub const TX_SLOTS: usize = 4;
const TX_BUF: usize = 2048;
const TXINFO_SIZE: usize = 4;
/// The RT5390's TXWI (the RT5592's is 4 bytes longer).
const TXWI_SIZE: usize = 16;
/// The largest 802.11 frame that fits a buffer (a 1500-byte packet with
/// CCMP is 1548 bytes).
pub const MAX_FRAME: usize = TX_BUF - TXINFO_SIZE - TXWI_SIZE - 3 - 8;
/// A transfer the chip hasn't taken for this long: it is stuck (OpenBSD's
/// RUN_TX_TIMEOUT).
pub const TX_TIMEOUT_MS: u64 = 5000;

/// The PHY word (mode and rate index) for a legacy rate in 500 kbit/s
/// units (`rt2860_rates` in rt2860reg.h); None for a rate the chip has no
/// legacy index for.
pub fn phy_word(rate: u8, short_preamble: bool) -> Option<u16> {
    // The chip's index is the rate's place in its modulation's list.
    let r = rates::value(rate);
    let cck = rates::CCK.iter().position(|&x| x == r);
    if let Some(mcs) = cck {
        // Short preamble: not at 1 Mbit/s (it has none).
        let sp = if short_preamble && r != 2 { PHY_SHPRE } else { 0 };
        return Some(PHY_CCK | mcs as u16 | sp);
    }
    let mcs = rates::OFDM.iter().position(|&x| x == r)?;
    Some(PHY_OFDM | mcs as u16)
}

/// TX counters on the driver's side (the chip's are `TxCounters`).
#[derive(Default, Clone, Copy, Debug)]
pub struct TxStats {
    /// Frames handed to the chip.
    pub frames: u32,
    /// Transfers that failed on USB.
    pub usb_errors: u32,
}

pub struct Tx {
    pub ep: u8,
    max_packet: usize,
    bufs: Vec<Dma>,
    /// Each buffer's transfer in flight: (token, queued at, in ms).
    busy: [Option<(u64, u64)>; TX_SLOTS],
    /// A transfer ended with a stall: the chip's endpoint is halted.
    pub stalled: bool,
    pub stats: TxStats,
}

/// Why a frame wasn't queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxError {
    /// All buffers are in flight: reap and try again.
    Full,
    TooLong,
    /// A rate the chip doesn't know.
    Rate,
}

impl Tx {
    pub fn new(ep: u8, max_packet: u16) -> Tx {
        Tx {
            ep,
            max_packet: (max_packet as usize & 0x7ff).max(8),
            bufs: (0..TX_SLOTS).map(|_| mem::alloc_dma(TX_BUF)).collect(),
            busy: [None; TX_SLOTS],
            stalled: false,
            stats: TxStats::default(),
        }
    }

    /// Check the transfers in flight (`run_txeof`). Returns the first
    /// USB error among those that completed.
    pub fn reap<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) -> Option<UsbError> {
        let mut err = None;
        for slot in self.busy.iter_mut() {
            let Some((token, _)) = *slot else { continue };
            match dev.check(hc, self.ep, token) {
                None => {}
                Some(Ok(_)) => *slot = None,
                Some(Err(e)) => {
                    *slot = None;
                    self.stats.usb_errors += 1;
                    self.stalled |= e == UsbError::Stall;
                    err.get_or_insert(e);
                }
            }
        }
        err
    }

    /// Transfers the chip hasn't taken yet.
    pub fn in_flight(&self) -> usize {
        self.busy.iter().flatten().count()
    }

    /// How long the oldest transfer in flight has been waiting (ms).
    pub fn oldest_ms(&self) -> Option<u64> {
        let now = time::now_ms();
        self.busy.iter().flatten().map(|&(_, t)| now.saturating_sub(t)).max()
    }

    /// Queue one frame (`run_tx`). `rate` in 500 kbit/s units; `ack`: the
    /// receiver must ACK it (unicast), else the chip retries. `seq` goes
    /// into a management or data frame's sequence control (fragment 0);
    /// the duration is sent as it is. `ccmp`: the chip encrypts the frame
    /// (Protected set, no CCMP header in it) with the access point's
    /// pairwise key, sending this CCMP header.
    #[allow(clippy::too_many_arguments)]
    pub fn send<H: HostController>(
        &mut self,
        hc: &mut H,
        dev: &mut Device<H>,
        frame: &[u8],
        seq: u16,
        rate: u8,
        short_preamble: bool,
        ack: bool,
        ccmp: Option<[u8; 8]>,
    ) -> Result<(), TxError> {
        let len = frame.len();
        if len > MAX_FRAME {
            return Err(TxError::TooLong);
        }
        let phy = phy_word(rate, short_preamble).ok_or(TxError::Rate)?;
        let slot = self.busy.iter().position(|b| b.is_none()).ok_or(TxError::Full)?;
        let buf = self.bufs[slot].bytes_mut();

        // TXINFO: TXWI + frame, rounded up to 4 bytes.
        let pkt_len = (TXWI_SIZE + len + 3) & !3;
        buf[0..2].copy_from_slice(&(pkt_len as u16).to_le_bytes());
        buf[2] = 0;
        // WIV: TXWI's IV words are ours: the CCMP header for a frame the
        // chip encrypts. OpenBSD leaves it clear and lets the chip take the
        // header from its IVEIV table, counting the PN there; this way the
        // PN stays the station's, one counter. (The header can't go in the
        // frame instead: tried on the Toon, the access point drops those.)
        buf[3] = TXINFO_QSEL_EDCA | TXINFO_WIV;

        // TXWI.
        let w = &mut buf[TXINFO_SIZE..TXINFO_SIZE + TXWI_SIZE];
        w.fill(0);
        w[1] = TXWI_TXOP_BACKOFF;
        w[2..4].copy_from_slice(&phy.to_le_bytes());
        // No TXWI_NSEQ: we number the frames. OpenBSD lets the chip number
        // non-QoS frames, but Linux found that the chip counts on for
        // retransmissions of them, so the receiver can't spot duplicates.
        w[4] = if ack { TXWI_ACK } else { 0 };
        w[5] = if ccmp.is_some() { WCID_AP } else { WCID_NONE };
        w[6..8].copy_from_slice(&((len as u16) & 0xfff).to_le_bytes());
        if let Some(iv) = ccmp {
            w[8..16].copy_from_slice(&iv);
        }

        let at = TXINFO_SIZE + TXWI_SIZE;
        buf[at..at + len].copy_from_slice(frame);
        // Management and data frames (not control frames) with a 24-byte
        // header. CCMP leaves the sequence number out of its MIC, so it
        // can be set after the encryption.
        if frame.len() >= 24 && (frame[0] >> 2) & 3 != 1 {
            buf[at + 22..at + 24].copy_from_slice(&((seq & 0xfff) << 4).to_le_bytes());
        }
        let mut total = TXINFO_SIZE + pkt_len + 4;
        // A transfer of whole packets has no short packet to end it.
        // OpenBSD sends a zero-length packet then; the chip reads the
        // lengths in TXINFO and doesn't need one (rt2800usb sends none),
        // and another 4 zero bytes (one more empty TXINFO) end it as well.
        if total % self.max_packet == 0 {
            total += 4;
        }
        buf[at + len..total].fill(0);

        let token = dev.queue(hc, self.ep, self.bufs[slot].phys, total as u32);
        self.busy[slot] = Some((token, time::now_ms()));
        self.stats.frames += 1;
        Ok(())
    }
}
