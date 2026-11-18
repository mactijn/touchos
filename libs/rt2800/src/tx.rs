//! Transmitting: one frame per bulk-OUT transfer (`run_tx` and
//! `run_txeof` in OpenBSD's if_run.c); what a transfer holds is in
//! [`crate::txwi`].
//!
//! A transfer completes when the chip has taken it over USB, not when it
//! went out on the air; whether the access point ACKed it is in the TX
//! statistics registers (`Rt2800::tx_counters`).

use alloc::vec::Vec;

use touchos_drv::mem::{self, Dma};
use touchos_rt::time;
use touchos_usb::{Device, HostController, UsbError};
use touchos_wlan::rates::Rate;

use crate::txwi::{self, Desc};

/// Transfers in flight at most (OpenBSD keeps 8 per queue; the station
/// sends little, and a full queue only means waiting a moment).
pub const TX_SLOTS: usize = 4;
const TX_BUF: usize = 2048;
/// A transfer the chip hasn't taken for this long: it is stuck (OpenBSD's
/// RUN_TX_TIMEOUT).
pub const TX_TIMEOUT_MS: u64 = 5000;

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

    /// Queue one frame (`run_tx`). `ack`: the receiver must ACK it
    /// (unicast), else the chip retries. `seq` goes into a management or
    /// data frame's sequence control (fragment 0); the duration is sent as
    /// it is. `sgi`: an HT rate with the short guard interval. `ccmp`: the
    /// chip encrypts the frame (Protected set, no CCMP header in it) with
    /// the access point's pairwise key, sending this CCMP header.
    #[allow(clippy::too_many_arguments)]
    pub fn send<H: HostController>(
        &mut self,
        hc: &mut H,
        dev: &mut Device<H>,
        frame: &[u8],
        seq: u16,
        rate: Rate,
        short_preamble: bool,
        sgi: bool,
        ack: bool,
        ccmp: Option<[u8; 8]>,
    ) -> Result<(), TxError> {
        let phy = txwi::phy_word(rate, short_preamble, sgi).ok_or(TxError::Rate)?;
        if txwi::transfer_len(frame, self.max_packet, TX_BUF).is_none() {
            return Err(TxError::TooLong);
        }
        let slot = self.busy.iter().position(|b| b.is_none()).ok_or(TxError::Full)?;
        let d = Desc { phy, ack, seq, ccmp };
        let total = txwi::write(self.bufs[slot].bytes_mut(), frame, &d, self.max_packet).ok_or(TxError::TooLong)?;

        let token = dev.queue(hc, self.ep, self.bufs[slot].phys, total as u32);
        self.busy[slot] = Some((token, time::now_ms()));
        self.stats.frames += 1;
        Ok(())
    }
}
