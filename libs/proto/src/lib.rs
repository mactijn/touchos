//! Data that crosses process boundaries in touchos, and the shared-memory
//! structures that carry it. `no_std` + `alloc`; used by the servers.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod ha;
pub mod input;
pub mod net;
pub mod shm;
pub mod storage;
pub mod ui;
pub mod update;

/// Encode a message (postcard).
pub fn encode<T: serde::Serialize>(v: &T) -> alloc::vec::Vec<u8> {
    postcard::to_allocvec(v).unwrap_or_default()
}

/// Decode a message (postcard).
pub fn decode<'a, T: serde::Deserialize<'a>>(b: &'a [u8]) -> Option<T> {
    postcard::from_bytes(b).ok()
}
