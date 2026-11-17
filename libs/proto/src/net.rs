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
    /// Our name on the network (without `.local`): `net.hostname`, or
    /// [`default_hostname`], with `-2`, `-3`... after an mDNS conflict.
    /// Empty while no link is up.
    pub hostname: HostName,
}

/// A host name of at most 63 bytes (one DNS label), kept inline so that
/// [`Status`] stays `Copy`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HostName {
    len: u8,
    bytes: [u8; 63],
}

impl HostName {
    pub const EMPTY: HostName = HostName { len: 0, bytes: [0; 63] };

    /// `None` if `s` is longer than 63 bytes.
    pub fn new(s: &str) -> Option<HostName> {
        let mut h = HostName::EMPTY;
        h.bytes.get_mut(..s.len())?.copy_from_slice(s.as_bytes());
        h.len = s.len() as u8;
        Some(h)
    }

    pub fn as_str(&self) -> &str {
        // Only ever made from a &str.
        core::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Default for HostName {
    fn default() -> HostName {
        HostName::EMPTY
    }
}

impl core::fmt::Debug for HostName {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
}

impl core::fmt::Display for HostName {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for HostName {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HostName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<HostName, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = HostName;
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_str("a host name of at most 63 bytes")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<HostName, E> {
                HostName::new(s).ok_or_else(|| E::invalid_length(s.len(), &self))
            }
        }
        d.deserialize_str(V)
    }
}

/// The name a panel has when `net.hostname` isn't set: `touchos-` and the
/// last two bytes of its MAC address in hex (`touchos-1a2b`).
pub fn default_hostname(mac: [u8; 6]) -> HostName {
    let mut buf = *b"touchos-xxxx";
    let hex = b"0123456789abcdef";
    for (i, b) in mac[4..].iter().enumerate() {
        buf[8 + 2 * i] = hex[(b >> 4) as usize];
        buf[9 + 2 * i] = hex[(b & 15) as usize];
    }
    HostName::new(core::str::from_utf8(&buf).unwrap()).unwrap()
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
    /// Announce a service over mDNS (DNS-SD) while net runs. data: postcard
    /// [`super::Service`]. A service with the same type and port replaces
    /// the earlier one (its owner was restarted). → OK.
    pub const MDNS_REGISTER: u32 = 7;
    /// Stop announcing a service. data: postcard [`super::Service`]; only
    /// its type and port count. → OK.
    pub const MDNS_UNREGISTER: u32 = 8;
}

/// A service announced over mDNS (DNS-SD, RFC 6763).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    /// The service type: `_http._tcp`.
    pub kind: alloc::string::String,
    /// The instance name ("Kitchen panel"); empty = the host name, which
    /// follows its renames.
    pub instance: alloc::string::String,
    pub port: u16,
    /// TXT `key=value` strings (each at most 255 bytes).
    pub txt: alloc::vec::Vec<alloc::string::String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names() {
        assert_eq!(default_hostname([0, 0x0f, 0x11, 0x22, 0x1a, 0x2b]).as_str(), "touchos-1a2b");
        let h = HostName::new("touchos-1a2b-2").unwrap();
        let s = Status { ipv4: Some(([10, 0, 0, 5], 24)), generation: 3, hostname: h };
        assert_eq!(crate::decode::<Status>(&crate::encode(&s)), Some(s));
        assert!(HostName::new(&"a".repeat(64)).is_none());
        assert!(HostName::new(&"a".repeat(63)).is_some());
    }
}
