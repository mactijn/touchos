//! The servers of the setup network (`wifi.setup_ap`): the panel is
//! 192.168.4.1 on its own open access point, hands out addresses with
//! [`dhcp::Server`], and answers every DNS name with itself
//! ([`dns::answer`]), so a phone that joins opens the settings page as a
//! captive portal. Without I/O, like `touchos_mdns`: `net` feeds them the
//! UDP payloads and sends what they return.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod dhcp;
pub mod dns;

/// The panel's address on the setup network.
pub const ADDR: [u8; 4] = [192, 168, 4, 1];
pub const PREFIX: u8 = 24;
