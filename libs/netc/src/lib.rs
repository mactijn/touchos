//! Client of the net server (the "net" capability): TCP connections whose
//! data flows through shared memory, DNS, and network status. With the
//! `tls` feature also TLS 1.2 and 1.3 (rustls) on top of those connections;
//! a program that uses it also needs the "cacerts" capability (the roots).

#![no_std]

extern crate alloc;

#[cfg(feature = "tls")]
pub mod tls;

use touchos_proto::net::{self as proto, Sock, Status, req, state};
use touchos_rt::{caps::Mapping, exec, ipc, sys, time};

#[cfg(feature = "tls")]
pub use tls::TlsStream;

pub type IpAddress = [u8; 4];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    LinkDown,
    NotConfigured,
    DnsFailed,
    ConnectFailed,
    Closed,
    Timeout,
}

impl core::fmt::Display for NetError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

fn ep() -> Result<u32, NetError> {
    touchos_rt::cap("net").ok_or(NetError::LinkDown)
}

/// The network status (None if the net server can't be reached).
pub fn status() -> Option<Status> {
    let m = ipc::call(ep().ok()?, req::STATUS, &[], &[]).ok()?;
    (m.label == proto::OK)
        .then(|| touchos_proto::decode(m.data()))
        .flatten()
}

/// Announce a service over mDNS (DNS-SD) as `<instance>.<kind>.local`
/// on `port`, while the net server runs: a restarted net server forgets
/// it, so register again with each `Listener::bind`. `kind` is like
/// `_http._tcp`; an empty `instance` is the panel's host name. A service
/// with the same type and port replaces the earlier one.
pub fn mdns_register(kind: &str, instance: &str, port: u16, txt: &[&str]) -> Result<(), NetError> {
    mdns_call(req::MDNS_REGISTER, kind, instance, port, txt)
}

/// Stop announcing the service of this type on this port.
pub fn mdns_unregister(kind: &str, port: u16) -> Result<(), NetError> {
    mdns_call(req::MDNS_UNREGISTER, kind, "", port, &[])
}

fn mdns_call(label: u32, kind: &str, instance: &str, port: u16, txt: &[&str]) -> Result<(), NetError> {
    let s = proto::Service {
        kind: kind.into(),
        instance: instance.into(),
        port,
        txt: txt.iter().map(|t| (*t).into()).collect(),
    };
    let m = ipc::call(ep()?, label, &touchos_proto::encode(&s), &[]).map_err(|_| NetError::LinkDown)?;
    match m.label {
        proto::OK => Ok(()),
        _ => Err(NetError::ConnectFailed),
    }
}

/// Wait until there is a way out: an address from DHCP, not the setup
/// network's (which reaches nothing but the phone being set up).
pub async fn wait_online() {
    loop {
        if let Some(Status { ipv4: Some(_), setup_ap: None, .. }) = status() {
            return;
        }
        exec::sleep_ms(500).await;
    }
}

/// Wait until we have an address (from DHCP, or the setup network's);
/// returns it.
pub async fn wait_configured() -> (IpAddress, u8) {
    loop {
        if let Some(Status { ipv4: Some(ip), .. }) = status() {
            return ip;
        }
        exec::sleep_ms(200).await;
    }
}

fn parse_ipv4(s: &str) -> Option<IpAddress> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for b in &mut out {
        *b = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// Resolve a host name (or parse a dotted IPv4 address).
pub async fn resolve(name: &str) -> Result<IpAddress, NetError> {
    if let Some(ip) = parse_ipv4(name) {
        return Ok(ip);
    }
    let ep = ep()?;
    let m = ipc::call(ep, req::RESOLVE, name.as_bytes(), &[]).map_err(|_| NetError::LinkDown)?;
    let id = match m.label {
        proto::ADDRESS => return m.data().try_into().map_err(|_| NetError::DnsFailed),
        proto::OK => m.data().get(..4).ok_or(NetError::DnsFailed)?.to_vec(),
        proto::LINK_DOWN => return Err(NetError::LinkDown),
        _ => return Err(NetError::DnsFailed),
    };
    let end = time::now_ms() + 5000;
    loop {
        exec::sleep_ms(20).await;
        let m = ipc::call(ep, req::RESOLVE_RESULT, &id, &[]).map_err(|_| NetError::LinkDown)?;
        match m.label {
            proto::ADDRESS => return m.data().try_into().map_err(|_| NetError::DnsFailed),
            proto::PENDING if time::now_ms() < end => {}
            _ => return Err(NetError::DnsFailed),
        }
    }
}

/// A TCP connection. Data goes through rings in memory shared with the net
/// server; each side signals the other's notification.
pub struct TcpStream {
    sock: Sock,
    _map: Mapping,
    mem: u32,
    /// The server's notification, and the bits it wants.
    server: u32,
    server_bits: usize,
    /// Our bit (the server signals it on new data or a state change).
    bit: usize,
}

impl TcpStream {
    /// Set up a connection from a CONNECT/ACCEPT reply.
    fn from_reply(m: &ipc::Msg, bit: usize) -> Result<TcpStream, NetError> {
        // On failure, the capabilities the reply brought are dropped.
        let fail = || {
            for &c in m.caps() {
                let _ = sys::cap_drop(c);
            }
            NetError::ConnectFailed
        };
        if m.label != proto::OK || m.caps().len() != 2 || m.data().len() < 12 {
            return Err(fail());
        }
        let (mem, server) = (m.caps()[0], m.caps()[1]);
        let server_bits = u64::from_le_bytes(m.data()[4..12].try_into().unwrap()) as usize;
        let map = Mapping::map(mem, true).map_err(|_| fail())?;
        let sock = unsafe { Sock::at(map.ptr()) };
        Ok(TcpStream {
            sock,
            _map: map,
            mem,
            server,
            server_bits,
            bit,
        })
    }

    fn request(label: u32, mut data: alloc::vec::Vec<u8>) -> Result<(ipc::Msg, usize), NetError> {
        let bit = exec::alloc_bit();
        data.extend_from_slice(&(bit as u64).to_le_bytes()); // u64 on the wire
        let notif = sys::notif_create().map_err(|_| NetError::LinkDown)?;
        let r = ipc::call(ep()?, label, &data, &[notif]);
        let _ = sys::cap_drop(notif);
        match r {
            Ok(m) => Ok((m, bit)),
            Err(_) => {
                exec::free_bit(bit);
                Err(NetError::LinkDown)
            }
        }
    }

    pub async fn connect(ip: IpAddress, port: u16) -> Result<TcpStream, NetError> {
        let mut data = alloc::vec::Vec::from(ip);
        data.extend_from_slice(&port.to_le_bytes());
        let (m, bit) = Self::request(req::CONNECT, data)?;
        if m.label == proto::LINK_DOWN {
            exec::free_bit(bit);
            return Err(NetError::LinkDown);
        }
        let s = Self::from_reply(&m, bit).inspect_err(|_| exec::free_bit(bit))?;
        let end = time::now_ms() + 10_000;
        loop {
            match s.sock.state() {
                state::ESTABLISHED => return Ok(s),
                state::CONNECTING if time::now_ms() < end => {
                    exec::wait_bits_or_ms(s.bit, 100).await;
                }
                state::CONNECTING => return Err(NetError::Timeout),
                _ => return Err(NetError::ConnectFailed),
            }
        }
    }

    fn wake_server(&self) {
        let _ = sys::signal(self.server, self.server_bits);
    }

    pub async fn write_all(&mut self, mut data: &[u8]) -> Result<(), NetError> {
        while !data.is_empty() {
            if self.sock.state() != state::ESTABLISHED {
                return Err(NetError::Closed);
            }
            let n = self.sock.tx.write(data);
            if n > 0 {
                self.wake_server();
                data = &data[n..];
            } else {
                exec::wait_bits_or_ms(self.bit, 20).await;
            }
        }
        Ok(())
    }

    /// Take bytes from the receive ring. If it was over half full, the net
    /// server may be holding more (and the peer waiting for window): wake
    /// it, so it refills the ring without waiting for its next look.
    fn take(&mut self, buf: &mut [u8]) -> usize {
        let was = self.sock.rx.len();
        let n = self.sock.rx.read(buf);
        if n > 0 && was >= proto::SOCK_RX as usize / 2 {
            self.wake_server();
        }
        n
    }

    /// Read some bytes; 0 means the peer closed the connection.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        loop {
            let n = self.take(buf);
            if n > 0 {
                return Ok(n);
            }
            match self.sock.state() {
                state::PEER_CLOSED => return Ok(0),
                state::FAILED => return Err(NetError::Closed),
                _ => {
                    exec::wait_bits_or_ms(self.bit, 100).await;
                }
            }
        }
    }

    /// Non-blocking read: Ok(None) if nothing is available right now.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        let n = self.take(buf);
        if n > 0 {
            return Ok(Some(n));
        }
        match self.sock.state() {
            state::PEER_CLOSED | state::FAILED => Err(NetError::Closed),
            _ => Ok(None),
        }
    }

    /// Is data waiting (or the connection closed, so a read returns at once)?
    pub fn readable(&self) -> bool {
        !self.sock.rx.is_empty() || self.sock.state() != state::ESTABLISHED
    }

    /// The notification bit the net server signals on new data or a state
    /// change (to wait for it together with others).
    pub fn bit(&self) -> usize {
        self.bit
    }

    /// A second view of the connection's state, for a layer (TLS) that owns
    /// the stream. Valid while the stream exists.
    pub fn probe(&self) -> Probe {
        Probe {
            sock: unsafe { Sock::at(self._map.ptr()) },
            bit: self.bit,
        }
    }
}

/// See [`TcpStream::probe`].
pub struct Probe {
    sock: Sock,
    bit: usize,
}

impl Probe {
    /// See [`TcpStream::bit`].
    pub fn bit(&self) -> usize {
        self.bit
    }

    pub fn readable(&self) -> bool {
        !self.sock.rx.is_empty() || self.sock.state() != state::ESTABLISHED
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        self.sock.request_close();
        self.wake_server();
        let _ = sys::cap_drop(self.server);
        let _ = sys::cap_drop(self.mem);
        exec::free_bit(self.bit);
        // The mapping is removed when `_map` drops.
    }
}

/// A listening TCP port (servers, e.g. the web interface).
pub struct Listener {
    id: u32,
}

impl Listener {
    pub fn bind(port: u16, backlog: u16) -> Result<Listener, NetError> {
        let mut data = alloc::vec::Vec::new();
        data.extend_from_slice(&port.to_le_bytes());
        data.extend_from_slice(&backlog.to_le_bytes());
        let m = ipc::call(ep()?, req::LISTEN, &data, &[]).map_err(|_| NetError::LinkDown)?;
        match m.label {
            proto::OK if m.data().len() >= 4 => Ok(Listener {
                id: u32::from_le_bytes(m.data()[..4].try_into().unwrap()),
            }),
            _ => Err(NetError::ConnectFailed),
        }
    }

    /// Wait for the next connection.
    pub async fn accept(&self) -> Result<TcpStream, NetError> {
        loop {
            let (m, bit) = TcpStream::request(req::ACCEPT, self.id.to_le_bytes().to_vec())?;
            match m.label {
                proto::OK => {
                    return TcpStream::from_reply(&m, bit).inspect_err(|_| exec::free_bit(bit));
                }
                proto::PENDING => {
                    exec::wait_bits_or_ms(bit, 1000).await;
                    exec::free_bit(bit);
                }
                _ => {
                    exec::free_bit(bit);
                    return Err(NetError::LinkDown);
                }
            }
        }
    }
}

/// A byte stream: plain TCP or TLS.
pub enum Conn<'a> {
    Plain(TcpStream),
    #[cfg(feature = "tls")]
    Tls(TlsStream<'a>),
    #[doc(hidden)]
    _Phantom(core::marker::PhantomData<&'a ()>),
}

impl Conn<'_> {
    pub async fn write_all(&mut self, d: &[u8]) -> Result<(), NetError> {
        match self {
            Conn::Plain(t) => t.write_all(d).await,
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.write_all(d).await,
            Conn::_Phantom(_) => unreachable!(),
        }
    }
    pub async fn read(&mut self, b: &mut [u8]) -> Result<usize, NetError> {
        match self {
            Conn::Plain(t) => t.read(b).await,
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.read(b).await,
            Conn::_Phantom(_) => unreachable!(),
        }
    }
    /// The bit signalled when data arrives (see [`TcpStream::bit`]).
    pub fn bit(&self) -> usize {
        match self {
            Conn::Plain(t) => t.bit(),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.bit(),
            Conn::_Phantom(_) => unreachable!(),
        }
    }

    /// Non-blocking read: Ok(None) when nothing is waiting.
    pub async fn try_read(&mut self, b: &mut [u8]) -> Result<Option<usize>, NetError> {
        match self {
            Conn::Plain(t) => t.try_read(b),
            #[cfg(feature = "tls")]
            Conn::Tls(t) => t.try_read(b).await,
            Conn::_Phantom(_) => unreachable!(),
        }
    }
}
