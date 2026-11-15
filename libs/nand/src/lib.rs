//! The Toon's NAND flash: the i.MX27's controller (NFC v1) and the layout
//! touchos keeps on the chip.
//!
//! - [`nfc`]: the controller (polled; a base address is all it needs, so
//!   the kernel and a user-space driver can both use it);
//! - [`layout`]: which blocks are whose, bad blocks, placement;
//! - [`image`]: the records and images on the chip, and the uImage header
//!   U-Boot loads;
//! - [`store`]: touchos.toml in the config ring, as the storage server saves
//!   it;
//! - [`system`]: the system copies and the state ring: writing a copy, and
//!   which copy starts (host-tested, these four).

#![no_std]

pub mod image;
pub mod layout;
pub mod nfc;
pub mod store;
pub mod system;
