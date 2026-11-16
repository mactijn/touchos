use super::*;

static ROOTS: &[u8] = include_bytes!("../cacerts.der");

/// A chain as a server sent it in November 2026 (see tests/verify.rs).
fn chain(host: &str) -> Vec<&'static [u8]> {
    let der: &'static [u8] = match host {
        "letsencrypt.org" => include_bytes!("../tests/chains/letsencrypt.org.der"),
        "www.google.com" => include_bytes!("../tests/chains/www.google.com.der"),
        "www.digicert.com" => include_bytes!("../tests/chains/www.digicert.com.der"),
        "github.com" => include_bytes!("../tests/chains/github.com.der"),
        "www.kernel.org" => include_bytes!("../tests/chains/www.kernel.org.der"),
        _ => unreachable!(),
    };
    Roots(der).iter().collect()
}

fn cn(name: &[u8]) -> &str {
    name_attr(name, CN).unwrap_or("")
}

#[test]
fn the_roots_file_is_whole() {
    let roots = Roots(ROOTS);
    let total: usize = roots.iter().map(<[u8]>::len).sum();
    assert_eq!(total, ROOTS.len(), "trailing bytes");
    assert!(roots.iter().count() > 100);
    assert_eq!(
        roots.iter().count(),
        roots.certs().count(),
        "every root parses"
    );
    for c in roots.certs() {
        assert!(
            c.self_issued(),
            "{} is not self-issued",
            short_name(c.subject)
        );
        assert_ne!(c.key_type, KeyType::Other, "{}", short_name(c.subject));
        assert!(c.not_before < c.not_after);
    }
}

#[test]
fn zero_padding_ends_the_roots() {
    // A memory object is whole pages: the file is followed by zeros.
    let mut padded = ROOTS.to_vec();
    padded.resize(ROOTS.len() + 3000, 0);
    assert_eq!(Roots(&padded).iter().count(), Roots(ROOTS).iter().count());
}

#[test]
fn damage_drops_the_rest() {
    let mut damaged = ROOTS.to_vec();
    let first = Roots(ROOTS).iter().next().unwrap().len();
    damaged[first] = 0x04; // the second root's tag
    assert_eq!(Roots(&damaged).iter().count(), 1);
}

#[test]
fn parses_a_certificate() {
    let isrg = Roots(ROOTS)
        .certs()
        .find(|c| cn(c.subject) == "ISRG Root X1")
        .unwrap();
    assert_eq!(
        name_attr(isrg.subject, O),
        Some("Internet Security Research Group")
    );
    assert_eq!(isrg.key_type, KeyType::Rsa);
    // notBefore 2015-06-04 11:04:38Z, notAfter 2035-06-04 11:04:38Z
    assert_eq!(isrg.not_before, 1_433_415_878);
    assert_eq!(isrg.not_after, 2_064_567_878);
    assert!(isrg.valid_at(1_800_000_000));
    assert!(!isrg.valid_at(1_400_000_000));

    let leaf = Cert::parse(chain("www.google.com")[0]).unwrap();
    assert_eq!(cn(leaf.subject), "www.google.com");
    assert_eq!(cn(leaf.issuer), "WE2");
    assert_eq!(leaf.key_type, KeyType::P256);
    assert_eq!(leaf.key.len(), 65); // uncompressed point
    assert_eq!(leaf.key[0], 0x04);
}

#[test]
fn utc_and_generalized_time() {
    let t = |tag, s: &str| {
        time(Tlv {
            tag,
            value: s.as_bytes(),
            raw: &[],
        })
    };
    assert_eq!(t(der::UTC_TIME, "700101000000Z"), Some(0));
    assert_eq!(t(der::UTC_TIME, "491231235959Z"), Some(2_524_607_999)); // 2049
    assert_eq!(
        t(der::GENERALIZED_TIME, "20500101000000Z"),
        Some(2_524_608_000)
    );
    assert_eq!(t(der::UTC_TIME, "700101000000"), None);
    assert_eq!(t(der::UTC_TIME, "701301000000Z"), None);
}
