//! The certificate check netc's TLS does (rustls-webpki, with the
//! RustCrypto provider's signature algorithms), against cacerts.der, on
//! chains as servers sent them in November 2026 (leaf first, concatenated
//! DER, captured with `openssl s_client -showcerts`). The time is fixed, so
//! the chains don't go stale.

use std::sync::Arc;

use rustls::RootCertStore;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::ServerCertVerifier;
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use touchos_certs::Roots;

static ROOTS: &[u8] = include_bytes!("../cacerts.der");

/// 2026-11-16 12:00 UTC, when the chains were captured.
const WHEN: u64 = 1_794_830_400;

fn chain(host: &str) -> Vec<CertificateDer<'static>> {
    let der: &'static [u8] = match host {
        "letsencrypt.org" => include_bytes!("chains/letsencrypt.org.der"),
        "www.google.com" => include_bytes!("chains/www.google.com.der"),
        "www.digicert.com" => include_bytes!("chains/www.digicert.com.der"),
        "github.com" => include_bytes!("chains/github.com.der"),
        "www.kernel.org" => include_bytes!("chains/www.kernel.org.der"),
        _ => unreachable!(),
    };
    Roots(der).iter().map(CertificateDer::from).collect()
}

/// The verifier as netc sets it up, over `roots`.
fn verifier(roots: &'static [u8]) -> Arc<WebPkiServerVerifier> {
    let mut store = RootCertStore::empty();
    for der in Roots(roots).iter() {
        let der: &'static CertificateDer<'static> = Box::leak(Box::new(CertificateDer::from(der)));
        store
            .roots
            .push(webpki::anchor_from_trusted_cert(der).unwrap());
    }
    WebPkiServerVerifier::builder_with_provider(
        Arc::new(store),
        Arc::new(rustls_rustcrypto::provider()),
    )
    .build()
    .unwrap()
}

fn check(
    v: &WebPkiServerVerifier,
    chain: &[CertificateDer<'static>],
    name: &str,
    at: u64,
) -> Result<(), rustls::Error> {
    let name = ServerName::try_from(name.to_string()).unwrap();
    let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(at));
    v.verify_server_cert(&chain[0], &chain[1..], &name, &[], at)
        .map(|_| ())
}

#[test]
fn every_root_is_an_anchor() {
    let roots = Roots(ROOTS);
    for der in roots.iter() {
        webpki::anchor_from_trusted_cert(&CertificateDer::from(der)).unwrap();
    }
}

#[test]
fn real_chains_verify() {
    let v = verifier(ROOTS);
    // Chains that end in a cross-sign by a root the distros dropped (GTS
    // Root R4 by GlobalSign Root CA), that send the root itself, that need
    // ISRG Root X2 rather than X1, and an EV leaf with 9 name parts.
    for host in [
        "letsencrypt.org",
        "www.google.com",
        "www.digicert.com",
        "github.com",
        "www.kernel.org",
    ] {
        check(&v, &chain(host), host, WHEN).unwrap_or_else(|e| panic!("{host}: {e}"));
    }
}

#[test]
fn wrong_name_is_refused() {
    let v = verifier(ROOTS);
    let e = check(&v, &chain("www.google.com"), "www.example.com", WHEN).unwrap_err();
    assert!(matches!(e, rustls::Error::InvalidCertificate(_)), "{e}");
}

#[test]
fn expired_is_refused() {
    let v = verifier(ROOTS);
    let e = check(
        &v,
        &chain("www.google.com"),
        "www.google.com",
        WHEN + 5 * 365 * 86_400,
    )
    .unwrap_err();
    assert!(
        matches!(
            e,
            rustls::Error::InvalidCertificate(
                rustls::CertificateError::Expired | rustls::CertificateError::ExpiredContext { .. }
            )
        ),
        "{e}"
    );
}

#[test]
fn unknown_root_is_refused() {
    // Only the ISRG roots: Google's chain leads to none of them.
    let isrg: Vec<u8> = Roots(ROOTS)
        .certs()
        .filter(|c| {
            touchos_certs::name_attr(c.subject, touchos_certs::CN)
                .is_some_and(|n| n.starts_with("ISRG"))
        })
        .flat_map(|c| c.der.iter().copied())
        .collect();
    let v = verifier(Box::leak(isrg.into_boxed_slice()));
    check(&v, &chain("letsencrypt.org"), "letsencrypt.org", WHEN).unwrap();
    let e = check(&v, &chain("www.google.com"), "www.google.com", WHEN).unwrap_err();
    assert!(
        matches!(
            e,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer)
        ),
        "{e}"
    );
}
