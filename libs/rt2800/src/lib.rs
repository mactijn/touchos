/*
 * Copyright (c) 2008-2010 Damien Bergamini <damien.bergamini@free.fr>
 * Copyright (c) 2013-2014 Kevin Lo
 *
 * Permission to use, copy, modify, and distribute this software for any
 * purpose with or without fee is hereby granted, provided that the above
 * copyright notice and this permission notice appear in all copies.
 *
 * THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
 * WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
 * MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
 * ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
 * WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
 * ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
 * OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
 */

//! The Ralink RT2800 USB Wi-Fi family, as the Toon's RT5370 needs it
//! (MAC/BBP RT5390, RF5370, 1T1R, 2.4 GHz): a Rust port of parts of
//! OpenBSD's run(4) driver (`sys/dev/usb/if_run.c`, `if_runvar.h` and
//! `sys/dev/ic/rt2860reg.h`, under the ISC license above). Linux's
//! rt2x00 was used only to check facts (register numbers, the RT5390's
//! values); none of its code is in here.
//!
//! **Receive only, or a station.** [`Rt2800::init`] brings the chip up
//! either way. Receive only (listening, or testing without an antenna):
//! MAC_SYS_CTRL's TX enable stays 0, no bulk-OUT endpoint is configured,
//! and the TX DMA, the automatic ACK/CTS responder and the PA enables
//! stay off. To transmit, all of those are on as OpenBSD sets them, and
//! [`Rt2800::send`] queues frames on the first bulk-OUT endpoint. The
//! station logic (authentication, WPA2, CCMP) is the caller's: the chip
//! sends and receives frames as they are, except that it can encrypt
//! the frames we send with the pairwise key (`Rt2800::set_pairwise_key`).
//!
//! **802.11g or 802.11n.** Frames go at a legacy rate or, joined as an HT
//! station, at an HT MCS (20 MHz, one stream; [`txwi::phy_word`]). Linux's
//! rt2800 is where the HT facts come from: the padding after a QoS header,
//! the HT fallback and protection registers' values.
//!
//! Everything is polled over USB vendor requests on EP0 (registers) and
//! bulk-IN transfers (frames); the chip has its own 8051 that needs the
//! `rt3071.bin` firmware.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

#[cfg(target_os = "none")]
mod chip;
#[cfg(target_os = "none")]
pub mod fw;
#[cfg(target_os = "none")]
mod io;
#[cfg(target_os = "none")]
pub mod radio;
pub mod regs;
#[cfg(target_os = "none")]
pub mod rom;
#[cfg(target_os = "none")]
pub mod rx;
// On the host only the tests call into it (the chip code that does is
// built for touchos only).
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
pub mod rxframe;
#[cfg(target_os = "none")]
pub mod tx;
pub mod txwi;

#[cfg(target_os = "none")]
pub use chip::*;
#[cfg(target_os = "none")]
pub use rom::Calib;
pub use rxframe::{Frame, Stats};
#[cfg(target_os = "none")]
pub use tx::TxStats;
