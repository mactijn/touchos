//! `cargo xtask check [--quick]`: one command for "did I break anything
//! that can be checked without hardware".
//!
//! - the host tests of every crate that has them (HOST_TESTS);
//! - both boards' builds (the kernels, the servers and the initrd), with
//!   the layout checks of `cargo xtask build`;
//! - the settings page's unit tests, when npm is installed;
//! - a warning when libs/certs/cacerts.der is behind the host's CA bundle
//!   or has a root that expires within a year (`cargo xtask cacerts`).
//!
//! `--quick` runs the host tests and `cargo check` of both targets instead
//! of the full builds (and skips the settings page).
//!
//! The on-target tests are `cargo xtask selftest`.

use anyhow::Result;
use std::process::Command;

use crate::{arm, root, sh};

/// The crates with host tests (`cargo test` on the development machine).
/// A crate whose Cargo.toml says `test = false` is built only for touchos
/// and does not belong here.
pub const HOST_TESTS: &[&str] = &[
    "xtask",
    "touchos-abi",
    "touchos-atomics",
    "touchos-blockdev",
    "touchos-certs",
    "touchos-civil",
    "touchos-config",
    "touchos-drv",
    "touchos-gpt",
    "touchos-mdns",
    "touchos-http",
    "touchos-nand",
    "touchos-opentherm",
    "touchos-proto",
    "touchos-hass",
    "touchos-elf",
    "touchos-rt2800",
    "touchos-setupnet",
    "touchos-storage",
    "touchos-tar",
    "touchos-update",
    "touchos-usb",
    "touchos-webauth",
    "touchos-wlan",
    "touchos-zwave",
];

pub fn check(quick: bool) -> Result<()> {
    step("host tests");
    let mut test = Command::new(env!("CARGO"));
    test.current_dir(root()).arg("test").arg("--quiet");
    for p in HOST_TESTS {
        test.args(["--package", p]);
    }
    sh(&mut test)?;

    if quick {
        for b in arm::Board::ALL {
            step(&format!("cargo check of the {} kernel", b.name()));
            arm::check(Some(b))?;
        }
        step("cargo check of the servers");
        arm::check(None)?;
    } else {
        for b in arm::Board::ALL {
            step(b.name());
            arm::build(b, false, "")?;
        }
        webui_tests()?;
    }
    // An out-of-date list of roots or zones is worth knowing, not a failed check.
    for w in crate::cacerts::warnings().into_iter().chain(crate::tzdata::warnings()) {
        println!("warning: {w}");
    }
    step("all checks passed");
    Ok(())
}

fn webui_tests() -> Result<()> {
    if Command::new("npm").arg("--version").output().is_err() {
        println!("note: npm not found; skipping the settings page's tests");
        return Ok(());
    }
    step("the settings page's unit tests");
    // `cargo xtask webui` (through the builds above) has installed node_modules.
    sh(Command::new("npx")
        .current_dir(root().join("webui"))
        .args(["ng", "test", "--watch=false"]))
}

fn step(what: &str) {
    println!("\n== check: {what}");
}
