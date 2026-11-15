//! Receiving: bulk-IN transfers kept queued on the chip's RX endpoint
//! (`run_rxeof` in OpenBSD's if_run.c). The frames in them: rxframe.

use alloc::vec::Vec;

use touchos_drv::mem::{self, Dma};
use touchos_usb::{Device, HostController};

use crate::rxframe::{Frame, Stats, parse};

/// Transfers kept queued.
pub const RX_TRANSFERS: usize = 4;
/// Bytes per transfer (OpenBSD's RUN_MAX_RXSZ; aggregates stay below it
/// with the aggregation limit set in `start_rx`).
pub const RX_SIZE: usize = 4096;

pub struct Rx {
    ep: u8,
    bufs: Vec<Dma>,
    /// Each buffer's transfer token, in queue order (oldest first).
    queued: Vec<(usize, u64)>,
    pub stats: Stats,
}

impl Rx {
    pub fn new(ep: u8) -> Rx {
        Rx { ep, bufs: (0..RX_TRANSFERS).map(|_| mem::alloc_dma(RX_SIZE)).collect(), queued: Vec::new(), stats: Stats::default() }
    }

    /// Queue all transfers.
    pub fn start<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        for i in 0..self.bufs.len() {
            if !self.queued.iter().any(|&(b, _)| b == i) {
                let t = dev.queue(hc, self.ep, self.bufs[i].phys, RX_SIZE as u32);
                self.queued.push((i, t));
            }
        }
    }

    /// Handle the completed transfers (in order), call `f` for each good
    /// frame, and queue them again. Returns the frames handed out.
    pub fn poll<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, mut f: impl FnMut(&Frame)) -> usize {
        let mut n = 0;
        while let Some(&(b, token)) = self.queued.first() {
            let Some(r) = dev.check(hc, self.ep, token) else { break };
            self.queued.remove(0);
            match r {
                Ok(short) => {
                    self.stats.transfers += 1;
                    let len = RX_SIZE - (short as usize).min(RX_SIZE);
                    n += parse(self.bufs[b].bytes_mut(), len, &mut self.stats, &mut f);
                }
                // A halted endpoint: the controller resets its queue on
                // the next queue() below.
                Err(e) => {
                    self.stats.usb_errors += 1;
                    if self.stats.usb_errors <= 5 {
                        ::log::warn!("rt2800: receive transfer failed: {e}");
                    }
                }
            }
            let t = dev.queue(hc, self.ep, self.bufs[b].phys, RX_SIZE as u32);
            self.queued.push((b, t));
        }
        n
    }
}
