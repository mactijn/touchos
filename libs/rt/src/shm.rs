//! Shared memory that this process uses as a ring or a board
//! (touchos_proto::shm), held together with the mapping that keeps it in
//! our address space: the view can't outlive the memory it points into,
//! and the `unsafe` of making a view lives here only.
//!
//! Waking the other side stays with the caller (`mem()` is the capability
//! for MEM_WAIT/MEM_NOTIFY): some producers notify after every push,
//! others once per batch, and some consumers poll.

use core::ops::Deref;

pub use touchos_proto::shm::{Board, Pushed, Ring, ring_size};

use crate::caps::{Mapping, cap};

pub struct Shared<V> {
    // (Dropped before the mapping: fields drop in order.)
    view: V,
    mem: u32,
    _map: Mapping,
}

// One thread per process: a Shared may sit in a static.
unsafe impl<V> Sync for Shared<V> {}
unsafe impl<V> Send for Shared<V> {}

impl<V> Shared<V> {
    /// The memory object's capability (for MEM_WAIT and MEM_NOTIFY).
    pub fn mem(&self) -> u32 {
        self.mem
    }
}

impl<V> Deref for Shared<V> {
    type Target = V;
    fn deref(&self) -> &V {
        &self.view
    }
}

/// The memory object `mem` as a board; the writer maps it `writable`,
/// readers read-only.
pub fn board_of(mem: u32, writable: bool) -> Option<Shared<Board>> {
    let map = Mapping::map(mem, writable).ok()?;
    let view = unsafe { Board::at(map.ptr(), map.len) };
    Some(Shared { view, mem, _map: map })
}

/// The board we were given as `name`.
pub fn board(name: &str, writable: bool) -> Option<Shared<Board>> {
    board_of(cap(name)?, writable)
}

/// The memory object `mem` as a ring (always mapped writable: the
/// consumer moves the tail). Its producer sets it up (`Ring::init`).
pub fn ring_of(mem: u32) -> Option<Shared<Ring>> {
    let map = Mapping::map(mem, true).ok()?;
    let view = unsafe { Ring::at(map.ptr()) };
    Some(Shared { view, mem, _map: map })
}

/// The ring we were given as `name`.
pub fn ring(name: &str) -> Option<Shared<Ring>> {
    ring_of(cap(name)?)
}
