//! A Wi-Fi station (WPA2-PSK, CCMP) shared by the chip drivers.
//!
//! - [`ieee80211`]: frame builders and parsers, 802.3 <-> 802.11;
//! - [`wpa`]: key derivation and the EAPOL-Key frames;
//! - [`keys`]: the 4-way and group key handshakes, and the receive keys
//!   with their replay check;
//! - [`ccmp`]: software AES-CCMP;
//! - [`scan`]: beacons -> [`scan::Network`], and a host-driven scan;
//! - [`rates`]: the legacy rates and which one to send at;
//! - [`amrr`]: rate control for chips that report TX statistics;
//! - [`radio`]: what the station needs from a chip ([`radio::Radio`]);
//! - [`station`]: scan, join, the handshake's waits and the data pump;
//! - [`backoff`]: how long it waits before trying again.
//!
//! The chips differ in how much they do themselves: the Intel 3165's
//! firmware scans, fills the duration field and does CCMP for unicast
//! frames; a soft-MAC such as the RT5370 leaves all of that to the host.
//! [`radio::Caps`] says which, and the station fills in the rest.
//!
//! Everything but the station is plain `no_std` + `alloc` and is tested on
//! the host; the station needs the runtime and is built only for touchos.

#![cfg_attr(not(test), no_std)]
#![allow(async_fn_in_trait)] // implemented and used only with static dispatch

extern crate alloc;

pub mod amrr;
pub mod ap;
pub mod backoff;
pub mod ccmp;
pub mod ieee80211;
pub mod keys;
pub mod radio;
pub mod rates;
pub mod scan;
#[cfg(target_os = "none")]
pub mod station;
pub mod wpa;

use core::fmt;

#[derive(Debug, Clone, Copy)]
pub enum Error {
    Timeout(&'static str),
    CmdFailed(u32),
    Hw(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Timeout(w) => write!(f, "timeout: {w}"),
            Error::CmdFailed(id) => write!(f, "firmware rejected command {id:#x}"),
            Error::Hw(w) => write!(f, "{w}"),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// A MAC address (a station's, a BSSID) as aa:bb:cc:dd:ee:ff, for the log.
pub struct Mac<'a>(pub &'a [u8; 6]);

impl fmt::Display for Mac<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let m = self.0;
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
    }
}

/// The WPA2 and CCMP test vectors; true if all pass.
pub fn self_test() -> bool {
    wpa::self_test() && ccmp::self_test()
}

#[cfg(test)]
mod tests {
    #[test]
    fn self_test_passes() {
        assert!(super::self_test());
    }

    #[test]
    fn mac_display() {
        let m = [0x00, 0x1b, 0x2c, 0xa0, 0xff, 0x09];
        assert_eq!(format!("{}", super::Mac(&m)), "00:1b:2c:a0:ff:09");
    }
}
