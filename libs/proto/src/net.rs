//! The network server: status, its IPC protocol and the shared memory of a
//! TCP connection.

use serde::{Deserialize, Serialize};

use crate::shm::ByteRing;
use core::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Our IPv4 address and prefix length, once DHCP has given us one.
    pub ipv4: Option<([u8; 4], u8)>,
    /// Bumped whenever the interface is (re)created.
    pub generation: u32,
}

/// Bytes of the boards for a `Status` (net writes) and a driver's `Link`
/// (the driver writes, net reads); init makes them.
pub const STATUS_BYTES: u64 = 4096;
pub const LINK_BYTES: u64 = 4096;

/// Link state published by each network driver.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// MAC address of the active adapter; None = no link.
    pub mac: Option<[u8; 6]>,
}

pub const FRAME_SLOTS: u32 = 64;
pub const FRAME_SLOT: u32 = 1536;

/// Request labels of the net server's endpoint.
pub mod req {
    /// data: ip [4], port u16 LE, bits u64 LE; caps[0]: our notification.
    /// → OK: data: socket id u32; caps[0]: the socket memory, caps[1]: the
    /// server's notification (signal it after writing or closing).
    pub const CONNECT: u32 = 1;
    /// data: host name. → OK: data: query id u32 | ADDRESS (data: ip [4]).
    pub const RESOLVE: u32 = 2;
    /// data: query id u32. → ADDRESS | PENDING | FAILED.
    pub const RESOLVE_RESULT: u32 = 3;
    /// → OK: data: postcard `Status`.
    pub const STATUS: u32 = 4;
    /// data: port u16 LE, backlog u16 LE. → OK: data: listener id u32.
    pub const LISTEN: u32 = 5;
    /// data: listener id u32, bits u64 LE; caps[0]: our notification.
    /// → OK like CONNECT (a new connection) | PENDING (we'll be signalled).
    pub const ACCEPT: u32 = 6;
}

pub const OK: u32 = 0;
pub const ADDRESS: u32 = 1;
pub const PENDING: u32 = 2;
pub const FAILED: u32 = 3;
pub const LINK_DOWN: u32 = 4;
pub const BAD_REQUEST: u32 = 5;

/// Connection states (written by the server).
pub mod state {
    pub const CONNECTING: u32 = 0;
    pub const ESTABLISHED: u32 = 1;
    /// The peer closed its side; what's left in the receive ring is the end.
    pub const PEER_CLOSED: u32 = 2;
    /// Connecting failed, the connection was reset, or the link went down.
    pub const FAILED: u32 = 3;
}

/// Shared memory of one TCP connection: a header page, then the receive
/// ring (server → client) and the transmit ring (client → server).
pub const SOCK_RX: u32 = 64 * 1024;
pub const SOCK_TX: u32 = 16 * 1024;
pub const SOCK_SIZE: usize = 4096 + SOCK_RX as usize + SOCK_TX as usize;

#[repr(C)]
struct SockHeader {
    state: AtomicU32,
    /// Set by the client: close the connection.
    close: AtomicU32,
    _pad: [u32; 2],
    rx: crate::shm::ByteRingHdr,
    tx: crate::shm::ByteRingHdr,
}

/// A view of a connection's shared memory.
pub struct Sock {
    base: *mut u8,
    pub rx: ByteRing,
    pub tx: ByteRing,
}

unsafe impl Send for Sock {}

impl Sock {
    /// # Safety
    /// `base` must point to `SOCK_SIZE` bytes of mapped shared memory.
    pub unsafe fn at(base: *mut u8) -> Sock {
        let h = base as *mut SockHeader;
        unsafe {
            Sock {
                base,
                rx: ByteRing::at(
                    core::ptr::addr_of_mut!((*h).rx).cast(),
                    base.add(4096),
                    SOCK_RX,
                ),
                tx: ByteRing::at(
                    core::ptr::addr_of_mut!((*h).tx).cast(),
                    base.add(4096 + SOCK_RX as usize),
                    SOCK_TX,
                ),
            }
        }
    }

    fn h(&self) -> &SockHeader {
        unsafe { &*(self.base as *const SockHeader) }
    }

    pub fn state(&self) -> u32 {
        self.h().state.load(Ordering::Acquire)
    }

    pub fn set_state(&self, s: u32) {
        self.h().state.store(s, Ordering::Release);
    }

    pub fn close_requested(&self) -> bool {
        self.h().close.load(Ordering::Acquire) != 0
    }

    pub fn request_close(&self) {
        self.h().close.store(1, Ordering::Release);
    }
}
