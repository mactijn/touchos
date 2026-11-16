//! TLS 1.3 client (embedded-tls) over our TCP streams. The server's
//! certificate is checked against the roots in the "cacerts" capability
//! (libs/certs/cacerts.der in the initrd, shared by init) and the wall clock.
//!
//! embedded-tls's own verifier takes one root, and needs the last
//! certificate the server sends to be issued by it. Servers often end their
//! chain with a cross-sign by a root the distros have since dropped (GTS
//! Root R4 by GlobalSign Root CA, for google.com and home-assistant.io), so
//! [`RootStore`] finds where the chain meets one of our roots, cuts it
//! there, and runs embedded-tls's verifier on that part with that root.
//!
//! That verifier has limits: at most 7 RDNs in a name (some EV certificates
//! have more), 12 extensions, the host matched against the first 3 SAN
//! names, and the curve taken from the signature's hash. Checking the chain
//! ourselves would lift them (#32).

use alloc::string::String;
use alloc::vec::Vec;
use embedded_io::ErrorKind;
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateEntryRef, CertificateRef, CertificateVerifyRef,
    CryptoProvider, SignatureScheme, TlsClock, TlsConfig, TlsConnection, TlsContext, TlsError,
    TlsVerifier,
};
use rand_core::{CryptoRng, RngCore};
use sha2::{Digest, Sha256};
use touchos_certs::{Cert, KeyType, Roots, short_name};

use super::{NetError, TcpStream};
use touchos_rt::caps::Mapping;
use touchos_rt::time;

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
    verifier: RootStore,
}

impl Provider {
    pub fn new() -> Provider {
        Provider {
            rng: Rng,
            verifier: RootStore::default(),
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

/// Checks the server's certificate against our roots (see the top of this
/// file), then its CertificateVerify signature with the leaf's key.
#[derive(Default)]
pub struct RootStore {
    host: Option<String>,
    /// From a verified chain, for `verify_signature`: the leaf, and the
    /// transcript up to the certificate.
    leaf: Option<Vec<u8>>,
    transcript: Option<Sha256>,
}

impl TlsVerifier<Aes128GcmSha256> for RootStore {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), TlsError> {
        self.host = Some(hostname.into());
        Ok(())
    }

    fn verify_certificate(
        &mut self,
        transcript: &Sha256,
        cert: CertificateRef,
    ) -> Result<(), TlsError> {
        let chain: Vec<&[u8]> = cert
            .entries
            .iter()
            .filter_map(|e| match e {
                CertificateEntryRef::X509(d) => Some(*d),
                CertificateEntryRef::RawPublicKey(_) => None,
            })
            .collect();
        if chain.is_empty() || chain.len() != cert.entries.len() {
            return Err(TlsError::InvalidCertificate);
        }
        let roots = roots();
        let k = roots.anchor(&chain).map_err(|e| {
            ::log::warn!("tls: {e}");
            TlsError::InvalidCertificate
        })?;
        let issuer = Cert::parse(chain[k]).ok_or(TlsError::DecodeError)?.issuer;
        let now = WallClock::now();
        let mut result = Err(TlsError::InvalidCertificate);
        for root in roots.by_subject(issuer) {
            // embedded-tls checks the dates of the chain, not of the root.
            if let Some(now) = now
                && !root.valid_at(now as i64)
            {
                ::log::warn!(
                    "tls: root \"{}\" is not valid now",
                    short_name(root.subject)
                );
                continue;
            }
            // The chain up to the certificate our root issued: entries
            // after it are not needed (and may be signed by roots we lack).
            let mut part = CertificateRef::with_context(&[]);
            for d in &chain[..=k] {
                part.add(CertificateEntryRef::X509(d))?;
            }
            let mut v =
                CertVerifier::<Aes128GcmSha256, WallClock, 0>::new(Certificate::X509(root.der));
            if let Some(h) = &self.host {
                v.set_hostname_verification(h)?;
            }
            result = v.verify_certificate(transcript, part);
            if result.is_ok() {
                self.leaf = Some(chain[0].to_vec());
                self.transcript = Some(transcript.clone());
                return Ok(());
            }
        }
        if let Err(e) = &result {
            ::log::warn!("tls: chain to \"{}\" not valid: {e:?}", short_name(issuer));
        }
        result
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef) -> Result<(), TlsError> {
        let (Some(leaf), Some(transcript)) = (self.leaf.take(), self.transcript.take()) else {
            return Err(TlsError::InvalidCertificate);
        };
        // RFC 8446 4.4.3: 64 spaces, the context string, the transcript hash.
        let mut msg = Vec::with_capacity(64 + 34 + 32);
        msg.resize(64, 0x20);
        msg.extend_from_slice(b"TLS 1.3, server CertificateVerify\x00");
        msg.extend_from_slice(&transcript.finalize());
        let cert = Cert::parse(&leaf).ok_or(TlsError::DecodeError)?;
        let ok = signature_ok(verify.signature_scheme, &cert, &msg, verify.signature)?;
        if ok {
            Ok(())
        } else {
            Err(TlsError::InvalidSignature)
        }
    }
}

/// Whether `sig` (scheme `scheme`) over `msg` is by `cert`'s key.
fn signature_ok(
    scheme: SignatureScheme,
    cert: &Cert,
    msg: &[u8],
    sig: &[u8],
) -> Result<bool, TlsError> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::signature::Verifier as _;
    fn pss<D>(key: &[u8], msg: &[u8], sig: &[u8]) -> Result<bool, TlsError>
    where
        D: Digest + rsa::sha2::digest::FixedOutputReset,
    {
        let key = rsa::RsaPublicKey::from_pkcs1_der(key).map_err(|_| TlsError::DecodeError)?;
        let sig = rsa::pss::Signature::try_from(sig).map_err(|_| TlsError::DecodeError)?;
        Ok(rsa::pss::VerifyingKey::<D>::new(key)
            .verify(msg, &sig)
            .is_ok())
    }
    match (scheme, cert.key_type) {
        (SignatureScheme::EcdsaSecp256r1Sha256, KeyType::P256) => {
            use p256::ecdsa::{Signature, VerifyingKey};
            let key = VerifyingKey::from_sec1_bytes(cert.key).map_err(|_| TlsError::DecodeError)?;
            let sig = Signature::from_der(sig).map_err(|_| TlsError::DecodeError)?;
            Ok(key.verify(msg, &sig).is_ok())
        }
        (SignatureScheme::EcdsaSecp384r1Sha384, KeyType::P384) => {
            use p384::ecdsa::{Signature, VerifyingKey};
            let key = VerifyingKey::from_sec1_bytes(cert.key).map_err(|_| TlsError::DecodeError)?;
            let sig = Signature::from_der(sig).map_err(|_| TlsError::DecodeError)?;
            Ok(key.verify(msg, &sig).is_ok())
        }
        (SignatureScheme::RsaPssRsaeSha256, KeyType::Rsa) => pss::<Sha256>(cert.key, msg, sig),
        (SignatureScheme::RsaPssRsaeSha384, KeyType::Rsa) => {
            pss::<sha2::Sha384>(cert.key, msg, sig)
        }
        (SignatureScheme::RsaPssRsaeSha512, KeyType::Rsa) => {
            pss::<sha2::Sha512>(cert.key, msg, sig)
        }
        (s, k) => {
            ::log::warn!("tls: signature {s:?} with a {k:?} key not supported");
            Err(TlsError::InvalidSignatureScheme)
        }
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
    /// that name and our roots).
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
