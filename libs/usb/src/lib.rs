//! USB host support: the host-controller driver (the ChipIdea EHCI of the
//! i.MX boards), descriptors, enumeration, control transfers, and
//! the interface a controller offers ([`HostController`]). Class drivers
//! (HID, CDC-ECM and mass storage in servers/usb, with [`msc`]'s wire
//! formats; the RT5370 Wi-Fi chip in libs/rt2800,
//! which servers/wifi/src/rt5370 binds to the station) are written
//! against it.
//!
//! Everything is polled (no interrupts), as the drivers in this system are:
//! a transfer is queued, then checked until it has completed.

#![cfg_attr(not(test), no_std)]
#![allow(async_fn_in_trait)] // implemented and used only with static dispatch

extern crate alloc;

pub mod desc;
pub mod msc;
#[cfg(target_os = "none")]
pub mod device;
#[cfg(target_os = "none")]
pub mod ehci;
#[cfg(target_os = "none")]
mod hc;

use core::fmt;

pub use desc::Endpoint;
#[cfg(target_os = "none")]
pub use device::{Device, wait_for_device};
#[cfg(target_os = "none")]
pub use hc::{HostController, PortStatus};
#[cfg(target_os = "none")]
use hc::spin_until;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbError {
    Timeout,
    /// The device stalled the request (or the endpoint is halted).
    Stall,
    /// A controller-specific completion code (xHCI) or error bits (EHCI).
    Completion(u32),
    Protocol(&'static str),
}

impl fmt::Display for UsbError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UsbError::Timeout => write!(f, "timeout"),
            UsbError::Stall => write!(f, "stall"),
            UsbError::Completion(4) => write!(f, "USB transaction error"),
            UsbError::Completion(c) => write!(f, "completion code {c}"),
            UsbError::Protocol(s) => write!(f, "{s}"),
        }
    }
}

pub type Result<T> = core::result::Result<T, UsbError>;

/// Device speeds, numbered as xHCI's PORTSC speed field.
pub const SPEED_FULL: u8 = 1;
pub const SPEED_LOW: u8 = 2;
pub const SPEED_HIGH: u8 = 3;
pub const SPEED_SUPER: u8 = 4;

pub fn speed_name(s: u8) -> &'static str {
    match s {
        SPEED_FULL => "full-speed",
        SPEED_LOW => "low-speed",
        SPEED_HIGH => "high-speed",
        SPEED_SUPER => "SuperSpeed",
        5 => "SuperSpeed+",
        _ => "unknown-speed",
    }
}
