//! TLS 1.2 and 1.3 client (rustls, with the RustCrypto provider) over our TCP
//! streams. The server's certificate is checked by rustls-webpki against
//! the roots in the "cacerts" capability (libs/certs/cacerts.der in the
//! initrd, shared by init) and the wall clock.
//!
//! Without `std`, rustls has no I/O of its own: its unbuffered API takes the
//! bytes we received and says what to do next (send these bytes, wait for
//! more, here is application data). [`TlsStream`] drives it over a
//! [`TcpStream`].

use alloc::sync::Arc;
use alloc::vec::Vec;
use rustls::client::UnbufferedClientConnection;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};

use super::{NetError, TcpStream};
use touchos_certs::Roots;
use touchos_rt::caps::Mapping;
use touchos_rt::{exec, time};

/// Bytes of each of the two record buffers a session needs: the largest
/// TLS record (16 KiB of data plus 2 KiB of TLS 1.2 overhead, and its
/// header) and some of the next one.
pub const RECORD_BUF: usize = 18 * 1024 + 512;

/// Application data per record we send (the protocol's maximum).
const MAX_FRAGMENT: usize = 16 * 1024;

/// How long a handshake waits for the wall clock (NTP, on a board without
/// a battery-backed RTC) before trying without it, which fails.
const CLOCK_WAIT_MS: u64 = 15_000;

/// The roots, from the "cacerts" capability: mapped on first use and kept
/// for the life of the process. Without the capability there are none,
/// and every handshake fails.
fn roots() -> Roots<'static> {
    static ROOTS: spin::Once<&'static [u8]> = spin::Once::new();
    Roots(ROOTS.call_once(|| {
        match touchos_rt::cap("cacerts").and_then(|c| Mapping::map(c, false).ok()) {
            Some(m) => {
                let bytes: &'static [u8] =
                    unsafe { core::slice::from_raw_parts(m.va as *const u8, m.len) };
                core::mem::forget(m);
                bytes
            }
            None => {
                ::log::error!("tls: no \"cacerts\" capability: no server can be trusted");
                &[]
            }
        }
    }))
}

/// One client configuration per process: the trust anchors point into the
/// mapped roots, so they are parsed once and never copied.
fn config() -> Arc<ClientConfig> {
    static CONFIG: spin::Once<Arc<ClientConfig>> = spin::Once::new();
    CONFIG
        .call_once(|| {
            let mut store = RootCertStore::empty();
            let mut bad = 0;
            for der in roots().iter() {
                // The anchor borrows from the certificate for as long as
                // the process runs (a small header each; the bytes stay
                // in the mapping).
                let der: &'static CertificateDer<'static> =
                    alloc::boxed::Box::leak(alloc::boxed::Box::new(CertificateDer::from(der)));
                match webpki::anchor_from_trusted_cert(der) {
                    Ok(a) => store.roots.push(a),
                    Err(_) => bad += 1,
                }
            }
            if bad > 0 {
                ::log::warn!("tls: {bad} roots unreadable");
            }
            let provider = Arc::new(rustls_rustcrypto::provider());
            let config = ClientConfig::builder_with_details(provider, Arc::new(WallClock))
                .with_safe_default_protocol_versions()
                .expect("the provider supports TLS 1.2 and 1.3")
                .with_root_certificates(store)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

/// Certificates are checked against the wall clock (the RTC at boot, then
/// NTP). Before it is known, rustls refuses every certificate.
#[derive(Debug)]
struct WallClock;

impl rustls::time_provider::TimeProvider for WallClock {
    fn current_time(&self) -> Option<UnixTime> {
        match time::unix_now() {
            0 => None,
            t => Some(UnixTime::since_unix_epoch(core::time::Duration::from_secs(
                t,
            ))),
        }
    }
}

/// The RustCrypto provider draws its randomness (handshake nonces, key
/// shares) through getrandom, whose "custom" backend (see
/// .cargo/config.toml) is the kernel's entropy pool.
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    touchos_rt::rand::fill(unsafe { core::slice::from_raw_parts_mut(dest, len) });
    Ok(())
}

/// What processing the received records came to.
enum Step {
    /// Handshake done and nothing (more) to process: we may send, or wait
    /// for more data.
    Ready,
    /// The handshake needs more data from the server.
    NeedData,
    /// The server closed the session.
    Closed,
}

/// An open TLS session. Record buffers ([`RECORD_BUF`] bytes each) are
/// borrowed from the caller.
pub struct TlsStream<'a> {
    conn: UnbufferedClientConnection,
    tcp: TcpStream,
    /// Received bytes not yet processed, `incoming[..in_len]`.
    incoming: &'a mut [u8],
    in_len: usize,
    /// Records to send, `outgoing[..out_len]`.
    outgoing: &'a mut [u8],
    out_len: usize,
    /// Decrypted application data not yet read, from `plain_at`.
    plain: Vec<u8>,
    plain_at: usize,
    closed: bool,
}

impl<'a> TlsStream<'a> {
    /// The handshake with `server_name` (its certificate is checked against
    /// that name and our roots).
    pub async fn connect(
        tcp: TcpStream,
        server_name: &str,
        rx_buf: &'a mut [u8],
        tx_buf: &'a mut [u8],
    ) -> Result<TlsStream<'a>, NetError> {
        let name = ServerName::try_from(server_name)
            .map_err(|_| {
                ::log::warn!("tls: {server_name:?} is not a server name");
                NetError::ConnectFailed
            })?
            .to_owned();
        // Certificates can't be checked without the date.
        let t0 = time::now_ms();
        while time::unix_now() == 0 && time::now_ms() - t0 < CLOCK_WAIT_MS {
            exec::sleep_ms(250).await;
        }
        let conn = UnbufferedClientConnection::new(config(), name).map_err(|e| {
            ::log::warn!("tls: {e}");
            NetError::ConnectFailed
        })?;
        let mut s = TlsStream {
            conn,
            tcp,
            incoming: rx_buf,
            in_len: 0,
            outgoing: tx_buf,
            out_len: 0,
            plain: Vec::new(),
            plain_at: 0,
            closed: false,
        };
        loop {
            let step = s.process(None).await.map_err(|e| {
                ::log::warn!("tls: handshake failed: {e}");
                NetError::ConnectFailed
            })?;
            match step {
                Step::Ready => return Ok(s),
                Step::NeedData => {
                    if !s.receive().await? {
                        ::log::warn!("tls: handshake failed: the server closed the connection");
                        return Err(NetError::ConnectFailed);
                    }
                }
                Step::Closed => return Err(NetError::ConnectFailed),
            }
        }
    }

    /// Run rustls over what we have received: send what it wants sent,
    /// keep the application data, and (if `send` is given and the
    /// handshake is done) encrypt and send `send`.
    async fn process(&mut self, mut send: Option<&[u8]>) -> Result<Step, rustls::Error> {
        let TlsStream {
            conn,
            tcp,
            incoming,
            in_len,
            outgoing,
            out_len,
            plain,
            ..
        } = self;
        loop {
            let UnbufferedStatus { discard, state } =
                conn.process_tls_records(&mut incoming[..*in_len]);
            let step = match state? {
                ConnectionState::ReadTraffic(mut r) => {
                    while let Some(record) = r.next_record() {
                        plain.extend_from_slice(record?.payload);
                    }
                    None
                }
                ConnectionState::EncodeTlsData(mut e) => {
                    *out_len += e.encode(&mut outgoing[*out_len..]).map_err(encode_error)?;
                    None
                }
                ConnectionState::TransmitTlsData(t) => {
                    transmit(tcp, &outgoing[..*out_len]).await?;
                    *out_len = 0;
                    t.done();
                    None
                }
                ConnectionState::WriteTraffic(mut w) => match send.take() {
                    Some(data) => {
                        for chunk in data.chunks(MAX_FRAGMENT) {
                            let n = w.encrypt(chunk, outgoing).map_err(encrypt_error)?;
                            transmit(tcp, &outgoing[..n]).await?;
                        }
                        Some(Step::Ready)
                    }
                    None => Some(Step::Ready),
                },
                ConnectionState::BlockedHandshake => Some(Step::NeedData),
                ConnectionState::PeerClosed | ConnectionState::Closed => Some(Step::Closed),
                _ => return Err(rustls::Error::General("unexpected TLS state".into())),
            };
            incoming.copy_within(discard..*in_len, 0);
            *in_len -= discard;
            if let Some(step) = step {
                return Ok(step);
            }
        }
    }

    /// Append what the server sent to `incoming`, waiting for it. False if
    /// the connection is closed.
    async fn receive(&mut self) -> Result<bool, NetError> {
        if self.in_len == self.incoming.len() {
            ::log::warn!("tls: a record larger than the buffer");
            return Err(NetError::Closed);
        }
        let n = self.tcp.read(&mut self.incoming[self.in_len..]).await?;
        self.in_len += n;
        Ok(n > 0)
    }

    /// Copy decrypted data out: how much was copied.
    fn take_plain(&mut self, buf: &mut [u8]) -> usize {
        let avail = &self.plain[self.plain_at..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.plain_at += n;
        if self.plain_at == self.plain.len() {
            self.plain.clear();
            self.plain_at = 0;
        }
        n
    }

    /// Process received records; `closed` if the server said goodbye.
    async fn pump(&mut self) -> Result<(), NetError> {
        match self.process(None).await {
            Ok(Step::Closed) => self.closed = true,
            Ok(_) => {}
            Err(e) => {
                ::log::warn!("tls: {e}");
                return Err(NetError::Closed);
            }
        }
        Ok(())
    }

    pub async fn write_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        match self.process(Some(data)).await {
            Ok(Step::Ready) => Ok(()),
            Ok(_) => Err(NetError::Closed),
            Err(e) => {
                ::log::warn!("tls: {e}");
                Err(NetError::Closed)
            }
        }
    }

    /// Blocking read of the next decrypted data (0: the session is closed).
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        loop {
            if self.plain_at < self.plain.len() {
                return Ok(self.take_plain(buf));
            }
            if self.closed {
                return Ok(0);
            }
            self.pump().await?;
            if self.plain_at < self.plain.len() || self.closed {
                continue;
            }
            if !self.receive().await? {
                self.closed = true;
            }
        }
    }

    /// The bit signalled when data arrives (see `TcpStream::bit`).
    pub fn bit(&self) -> usize {
        self.tcp.bit()
    }

    /// Read only if data is waiting, decrypted or on the socket: Ok(None)
    /// when there is nothing. A TLS record arrives as a unit, so a read
    /// completes promptly.
    pub async fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        loop {
            if self.plain_at < self.plain.len() {
                return Ok(Some(self.take_plain(buf)));
            }
            if self.closed {
                return Err(NetError::Closed);
            }
            self.pump().await?;
            if self.plain_at < self.plain.len() || self.closed {
                continue;
            }
            if !self.tcp.readable() {
                return Ok(None);
            }
            if !self.receive().await? {
                self.closed = true;
            }
        }
    }
}

async fn transmit(tcp: &mut TcpStream, data: &[u8]) -> Result<(), rustls::Error> {
    tcp.write_all(data)
        .await
        .map_err(|_| rustls::Error::General("the connection closed".into()))
}

fn encode_error(e: EncodeError) -> rustls::Error {
    rustls::Error::General(alloc::format!("encoding a handshake record: {e}"))
}

fn encrypt_error(e: EncryptError) -> rustls::Error {
    rustls::Error::General(alloc::format!("encrypting: {e}"))
}
