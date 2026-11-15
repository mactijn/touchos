//! The interface a host controller offers ([`HostController`]), and the
//! short register waits the controllers share. (On touchos only: it needs
//! DMA memory and the clock.)

use alloc::vec::Vec;
use touchos_drv::mem::Dma;
use touchos_rt::time;

use crate::{Endpoint, Result, UsbError};

/// Busy-wait up to `ms` for a register condition of the controller `hc`
/// ("ehci": for the log). For the steps that take micro- to
/// milliseconds (reset, start, the ULPI viewport); too short to yield.
pub(crate) fn spin_until(hc: &str, what: &str, ms: u64, mut cond: impl FnMut() -> bool) -> Result<()> {
    touchos_drv::mmio::poll_until(time::now_us, ms * 1000, || cond().then_some(())).ok_or_else(|| {
        ::log::error!("{hc}: timeout waiting for {what}");
        UsbError::Timeout
    })
}


/// A root port's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortStatus {
    pub connected: bool,
    pub enabled: bool,
}

/// A USB host controller, as the generic layers and class drivers use it.
/// Ports are numbered from 1. Endpoints are named by their address (0x81 =
/// endpoint 1 IN); endpoint 0 is reached only through [`control`].
///
/// [`control`]: HostController::control
pub trait HostController {
    /// The controller's per-device state (an xHCI slot, an EHCI address
    /// and its queue heads).
    type Dev;

    /// Number of root ports.
    fn ports(&self) -> u8;
    /// Process the controller's events; returns the ports whose status
    /// changed (each once). Call often: transfers complete through it too.
    fn poll(&mut self) -> Vec<u8>;
    fn port_status(&self, port: u8) -> PortStatus;
    /// Acknowledge a port's status changes.
    fn ack_port(&mut self, port: u8);
    /// Reset a port so its device is enabled; returns the device's speed
    /// (`SPEED_*`). `force`: reset even if it is already enabled (retries).
    async fn reset_port(&mut self, port: u8, force: bool) -> Result<u8>;
    /// Give the device on a freshly reset port an address. On the way the
    /// first bytes of its device descriptor (at least 8) land in `buf`.
    async fn address(&mut self, port: u8, speed: u8, buf: &mut Dma) -> Result<Self::Dev>;
    /// A control transfer on endpoint 0. `setup` is the 8-byte SETUP
    /// packet; its wLength bytes of data go through `buf` (in or out, by
    /// bmRequestType). Returns the number of bytes transferred.
    async fn control(&mut self, dev: &mut Self::Dev, setup: [u8; 8], buf: &mut Dma) -> Result<usize>;
    /// Make the endpoints usable (after SET_CONFIGURATION/SET_INTERFACE).
    async fn configure_endpoints(&mut self, dev: &mut Self::Dev, eps: &[Endpoint]) -> Result<()>;
    /// Queue a bulk or interrupt transfer of `len` bytes at `buf_phys` on
    /// endpoint `ep`; returns a token for [`check`](Self::check).
    fn queue(&mut self, dev: &mut Self::Dev, ep: u8, buf_phys: u64, len: u32) -> u64;
    /// Has the transfer `token` on `ep` completed? On success: the number
    /// of bytes *not* transferred (a short read).
    fn check(&mut self, dev: &mut Self::Dev, ep: u8, token: u64) -> Option<Result<u32>>;
    /// The device is gone (or given up): free its resources.
    async fn disable(&mut self, dev: Self::Dev);
    /// Interrupt on completed transfers and port changes (for a driver
    /// that gets the controller's interrupt). Default: nothing (polled).
    fn enable_interrupts(&mut self) {}
    /// Clear the controller's interrupt status. Call before looking at
    /// transfers and ports: what completes after that interrupts again.
    fn clear_interrupts(&mut self) {}
}
