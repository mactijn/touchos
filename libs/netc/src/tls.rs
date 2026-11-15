//! TLS 1.3 client (embedded-tls) over our TCP streams, with certificate
//! verification against ISRG Root X1 (Let's Encrypt) and the wall clock.

use alloc::vec::Vec;
use embedded_io::ErrorKind;
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, TlsClock, TlsConfig, TlsConnection, TlsContext,
    TlsError, TlsVerifier,
};
use rand_core::{CryptoRng, RngCore};

use super::{NetError, TcpStream};
use touchos_rt::time;

/// Let's Encrypt's root. Our server chains: leaf <- YE2 <- Root YE <- ISRG Root X2 <- ISRG Root X1.
pub static ISRG_ROOT_X1: &[u8] = include_bytes!("../certs/isrg-root-x1.der");

/// TCP stream as an embedded-io-async transport.
pub struct TcpIo(pub TcpStream);

impl embedded_io::ErrorType for TcpIo {
    type Error = ErrorKind;
}

impl embedded_io_async::Read for TcpIo {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorKind> {
        self.0
            .read(buf)
            .await
            .map_err(|_| ErrorKind::ConnectionReset)
    }
}

impl embedded_io_async::Write for TcpIo {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, ErrorKind> {
        self.0
            .write_all(buf)
            .await
            .map_err(|_| ErrorKind::ConnectionReset)?;
        Ok(buf.len())
    }
    async fn flush(&mut self) -> Result<(), ErrorKind> {
        Ok(())
    }
}

/// The kernel's entropy pool as an RNG (see `touchos_rt::rand`).
pub struct Rng;

impl RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        touchos_rt::rand::fill(dest)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}
impl CryptoRng for Rng {}

/// Certificate validity is checked against the wall clock (RTC at boot).
pub struct WallClock;

impl TlsClock for WallClock {
    fn now() -> Option<u64> {
        match time::unix_now() {
            0 => None,
            t => Some(t),
        }
    }
}

pub struct Provider {
    rng: Rng,
    verifier: CertVerifier<'static, Aes128GcmSha256, WallClock, 4096>,
}

impl Provider {
    pub fn new() -> Provider {
        Provider {
            rng: Rng,
            verifier: CertVerifier::new(Certificate::X509(ISRG_ROOT_X1)),
        }
    }
}

impl CryptoProvider for Provider {
    type CipherSuite = Aes128GcmSha256;
    type Signature = Vec<u8>;

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

/// Bytes of each of the two record buffers a session needs (a TLS record
/// of up to 16 KiB, plus its header and tag).
pub const RECORD_BUF: usize = 16 * 1024 + 256;

/// An open TLS session. Record buffers ([`RECORD_BUF`] bytes each) are
/// borrowed from the caller.
pub struct TlsStream<'a> {
    conn: TlsConnection<'a, TcpIo, Aes128GcmSha256>,
    probe: super::Probe,
}

impl<'a> TlsStream<'a> {
    /// The handshake with `server_name` (its certificate is checked against
    /// that name and our root).
    pub async fn connect(
        tcp: TcpStream,
        server_name: &str,
        rx_buf: &'a mut [u8],
        tx_buf: &'a mut [u8],
    ) -> Result<TlsStream<'a>, NetError> {
        let probe = tcp.probe();
        let config = TlsConfig::new().with_server_name(server_name);
        let mut conn = TlsConnection::new(TcpIo(tcp), rx_buf, tx_buf);
        conn.open(TlsContext::new(&config, Provider::new()))
            .await
            .map_err(|e| {
                ::log::warn!("tls: handshake failed: {e:?}");
                NetError::ConnectFailed
            })?;
        Ok(TlsStream { conn, probe })
    }

    pub async fn write_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        let mut off = 0;
        while off < data.len() {
            off += self
                .conn
                .write(&data[off..])
                .await
                .map_err(|_| NetError::Closed)?;
        }
        self.conn.flush().await.map_err(|_| NetError::Closed)
    }

    /// Blocking read of the next decrypted data.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        self.conn.read(buf).await.map_err(|_| NetError::Closed)
    }

    /// The bit signalled when data arrives (see `TcpStream::bit`).
    pub fn bit(&self) -> usize {
        self.probe.bit()
    }

    /// Read only if the socket has data waiting (a TLS record arrives as a
    /// unit, so the read completes promptly). `buf` should be >= 16 KiB.
    pub async fn try_read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, NetError> {
        if !self.probe.readable() {
            return Ok(None);
        }
        self.read(buf).await.map(Some)
    }
}
