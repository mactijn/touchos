//! Build tooling for touchos: `cargo xtask <command>`. The commands and
//! their options are in commands.rs (`cargo xtask` alone prints them).
//!
//! Where things are: arm.rs (building for a board, QEMU, loading the Toon
//! over JTAG), nand.rs (the Toon's NAND: flashing, booting from it, update
//! bundles), initrd.rs (the servers in the initrd), update.rs (the signing
//! key and signatures), config.rs (which touchos.toml a build carries),
//! cacerts.rs (the TLS roots, from the distro's CA bundle), tzdata.rs
//! (the time zones' rules, from the distro's tzdata).

mod arm;
mod build_id;
mod cacerts;
mod check;
mod commands;
mod config;
mod initrd;
mod nand;
mod pair;
mod selftest;
mod tzdata;
mod update;
mod webui;

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use arm::Board;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = commands::check(&args) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let flag = |f: &str| commands::flag(&args, f);
    let opt = |name: &str| commands::opt(&args, name);
    let release = flag("--release");
    let cmdline = opt("--cmdline").unwrap_or_default();
    let secs = |default: u64| opt("--secs").and_then(|s| s.parse().ok()).unwrap_or(default);
    match args.first().map(String::as_str) {
        Some("build") => {
            let board = Board::parse(opt("--board").as_deref().unwrap_or("toon"))?;
            arm::build(board, release, &cmdline)?;
        }
        Some("qemu") => {
            let a = arm::build(Board::Imx25, release, &cmdline)?;
            arm::qemu(&a, secs(20))?;
        }
        Some("load") => {
            let a = arm::build(Board::Toon, release, &cmdline)?;
            arm::load(&a, secs(20))?;
        }
        Some("flash") => {
            let slot = match opt("--slot").as_deref() {
                None | Some("A") => b'A',
                Some("B") => b'B',
                Some(s) => bail!("--slot {s}: A or B"),
            };
            let a = arm::build(Board::Toon, release, &cmdline)?;
            nand::flash(&a, slot, flag("--config"), secs(120).max(120))?;
        }
        Some("nandboot") => nand::nandboot(secs(20))?,
        Some("bundle") => {
            let a = arm::build(Board::Toon, release, &cmdline)?;
            nand::bundle(&a)?;
        }
        Some("check") => check::check(flag("--quick"))?,
        Some("selftest") => selftest::main(secs(150))?,
        Some("log") => {
            let port = args.get(1).and_then(|p| p.parse().ok()).unwrap_or(5555u16);
            receive_log(port)?;
        }
        Some("pair") => pair::pair(
            args.get(1)
                .filter(|a| !a.starts_with("--"))
                .map(String::as_str)
                .unwrap_or("http://localhost:8080"),
            opt("--port").and_then(|p| p.parse().ok()).unwrap_or(5555),
        )?,
        Some("webui") => match args.get(1).map(String::as_str) {
            Some("serve") => webui::serve(
                args.get(2)
                    .map(String::as_str)
                    .unwrap_or("http://localhost:8080"),
            )?,
            _ => webui::build(true)?,
        },
        Some("keygen") => update::keygen()?,
        Some("cacerts") => cacerts::run(
            args.get(1).filter(|a| !a.starts_with("--")).map(String::as_str),
            flag("--check"),
        )?,
        Some("tzdata") => tzdata::run(
            args.get(1).filter(|a| !a.starts_with("--")).map(String::as_str),
            flag("--check"),
        )?,
        _ => unreachable!("commands::check knows every command"),
    }
    Ok(())
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn sh(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("failed to start {cmd:?}"))?;
    if !status.success() {
        bail!("command failed ({status}): {cmd:?}");
    }
    Ok(())
}

/// Print log lines that touchos sends over UDP (see `debug.log_host`).
fn receive_log(port: u16) -> Result<()> {
    let sock = std::net::UdpSocket::bind(("0.0.0.0", port))
        .with_context(|| format!("binding UDP port {port}"))?;
    eprintln!("listening for touchos log lines on UDP port {port}...");
    let mut buf = [0u8; 2048];
    loop {
        let (n, from) = sock.recv_from(&mut buf)?;
        println!("{} {}", from.ip(), String::from_utf8_lossy(&buf[..n]));
    }
}
