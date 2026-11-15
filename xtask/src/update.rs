//! Software updates for the settings page: the signing key (`cargo xtask
//! keygen`) and the signature on a bundle (`cargo xtask bundle`, whose
//! files nand.rs gathers). A bundle is checked here the way the panel
//! checks it.

use anyhow::{Context, Result};

use crate::root;

/// Where the update signing key lives (outside the repository).
fn key_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("TOUCHOS_UPDATE_KEY") {
        return p.into();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join(".config/touchos/update-key")
}

fn public_key_path() -> std::path::PathBuf {
    root().join("config/update-key.pub")
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    (s.len() % 2 == 0)
        .then(|| {
            (0..s.len() / 2)
                .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
                .collect()
        })
        .flatten()
}

/// `cargo xtask keygen`: a new signing key for updates. The secret key
/// goes to ~/.config/touchos/update-key (readable only by you); the
/// public key to config/update-key.pub, which is built into touchos.
pub fn keygen() -> Result<()> {
    let sk_path = key_path();
    anyhow::ensure!(
        !sk_path.exists(),
        "{} exists already; remove it first to make a new key (old builds then refuse new updates)",
        sk_path.display()
    );
    let mut seed = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let (sk, pk) = touchos_update::keypair(&seed);
    std::fs::create_dir_all(sk_path.parent().unwrap())?;
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&sk_path)?;
        writeln!(f, "{}", hex(&sk))?;
    }
    std::fs::write(public_key_path(), format!("{}\n", hex(&pk)))?;
    println!(
        "secret key: {} (keep it; don't share it)",
        sk_path.display()
    );
    println!("public key: {} = {}", public_key_path().display(), hex(&pk));
    println!("builds from now on accept updates signed with this key");
    Ok(())
}

/// Sign `files` as build `build`, check the result as the panel will
/// (`check`), and write build/touchos-<build>.update.
pub fn sign(
    build: &str,
    files: &[(String, Vec<u8>)],
    check: fn(&touchos_update::Bundle) -> Result<(), String>,
) -> Result<std::path::PathBuf> {
    let sk_text = std::fs::read_to_string(key_path()).with_context(|| {
        format!(
            "no signing key at {} (cargo xtask keygen)",
            key_path().display()
        )
    })?;
    let sk: [u8; 64] = unhex(&sk_text)
        .and_then(|v| v.try_into().ok())
        .context("the signing key file is damaged")?;
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let bundle = touchos_update::build(build, &refs, &sk);
    // Check it against the public key the build carries.
    let pk: [u8; 32] = std::fs::read_to_string(public_key_path())
        .ok()
        .and_then(|t| unhex(&t))
        .and_then(|v| v.try_into().ok())
        .context("no config/update-key.pub")?;
    let checked = touchos_update::verify(&bundle, &pk)
        .map_err(|e| anyhow::anyhow!("the new bundle doesn't verify: {e:?}"))?;
    check(&checked).map_err(|e| anyhow::anyhow!("the panel would refuse it: {e}"))?;
    let out = root().join(format!("build/touchos-{build}.update"));
    std::fs::write(&out, &bundle)?;
    println!("{} ({} KiB, {} files)", out.display(), bundle.len() / 1024, files.len());
    Ok(out)
}
