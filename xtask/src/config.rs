//! Which touchos.toml a build carries, and a check before it ships.
//!
//! - The Toon (`build`, `load`, `flash`): $TOUCHOS_CONFIG, else
//!   config/touchos.toml (gitignored: it holds the Home Assistant token),
//!   else the example.
//! - QEMU imx25 (`build --board imx25`, `qemu`, `selftest`):
//!   $TOUCHOS_CONFIG, else the gitignored config/qemu.local.toml, else
//!   config/qemu.toml (no secrets).
//!
//! Update bundles carry no config: the panel keeps its own.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use touchos_config::{Config, Document};

use crate::root;

#[derive(Clone, Copy)]
pub enum Purpose {
    Toon,
    Qemu,
}

pub fn select(p: Purpose) -> PathBuf {
    let config = |name: &str| root().join("config").join(name);
    let env = std::env::var_os("TOUCHOS_CONFIG").map(PathBuf::from);
    match p {
        Purpose::Qemu => env.unwrap_or_else(|| {
            let local = config("qemu.local.toml");
            if local.exists() { local } else { config("qemu.toml") }
        }),
        Purpose::Toon => env.unwrap_or_else(|| {
            let own = config("touchos.toml");
            if own.exists() {
                own
            } else {
                println!("note: config/touchos.toml not found, using touchos.example.toml");
                config("touchos.example.toml")
            }
        }),
    }
}

/// Parse `path` as the panel will. A syntax error stops the build (the
/// panel couldn't read the file at all); the settings it would ignore or
/// correct are listed by field name (never with values: the file may hold
/// secrets). An empty setting, like QEMU's empty token, is such a case.
pub fn check(path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
    if let Err(e) = Document::parse(&text) {
        bail!("{}: {e}", path.display());
    }
    let (_, issues) = Config::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    for i in &issues {
        println!("note: {}: {}: {}", path.display(), i.field, i.msg);
    }
    Ok(())
}
