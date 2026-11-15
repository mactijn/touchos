//! The initrd: a ustar archive with the user-space servers, loaded into
//! RAM next to the kernel (see arm.rs). The ELFs in it are stripped; the
//! full ones stay in target/ for symbolizing crash addresses.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{root, sh};

/// A server program in the initrd.
pub struct Server {
    pub package: &'static str,
    pub bin: &'static str,
}

const fn server(package: &'static str, bin: &'static str) -> Server {
    Server { package, bin }
}

/// The servers, in initrd order (one set for both boards).
pub const SERVERS: &[Server] = &[
    server("touchos-srv-init", "init"),
    server("touchos-srv-config", "config"),
    server("touchos-srv-ui", "ui"),
    server("touchos-srv-platform", "platform"),
    server("touchos-srv-net", "net"),
    server("touchos-srv-ha", "ha"),
    server("touchos-srv-web", "web"),
    server("touchos-srv-storage", "storage"),
    server("touchos-srv-update", "update"),
    server("touchos-srv-usb", "usb"),
    server("touchos-srv-wifi", "wifi"),
    server("touchos-srv-fec", "fec"),
    server("touchos-srv-boiler", "boiler"),
    server("touchos-srv-zwave", "zwave"),
    server("touchos-srv-hello", "hello"),
    server("touchos-srv-selftest", "selftest"),
];

/// Build the servers; returns (name, ELF path) of each.
pub fn build_servers(release: bool) -> Result<Vec<(String, PathBuf)>> {
    // The web server embeds the settings page.
    crate::webui::build(false)?;
    let mut cargo = crate::arm::cargo_user(release);
    for s in SERVERS {
        cargo.args(["--package", s.package]);
    }
    sh(&mut cargo)?;
    let out = crate::arm::user_out_dir(release);
    Ok(SERVERS.iter().map(|s| (s.bin.to_string(), out.join(s.bin))).collect())
}

/// `llvm-objcopy` from the toolchain's llvm-tools component (in the
/// sysroot, under the host's target triple).
fn objcopy() -> Option<PathBuf> {
    let rustc = |args: &[&str]| -> Option<String> {
        let out = Command::new("rustc").args(args).output().ok()?;
        String::from_utf8(out.stdout).ok()
    };
    let sysroot = rustc(&["--print", "sysroot"])?;
    let host = rustc(&["-vV"])?.lines().find_map(|l| l.strip_prefix("host: ").map(str::to_string))?;
    let p = Path::new(sysroot.trim()).join(format!("lib/rustlib/{host}/bin/llvm-objcopy"));
    p.exists().then_some(p)
}

/// Write `out` with the servers, stripped of debug info (the stripped
/// copies go to `initrd/` next to `out`), and data files (firmware) stored
/// as they are, by name.
pub fn create(out: &Path, servers: &[(String, PathBuf)], files: &[(String, PathBuf)]) -> Result<()> {
    let stripped_dir = out.parent().unwrap_or(&root()).join("initrd");
    std::fs::create_dir_all(&stripped_dir)?;
    let objcopy = objcopy();
    if objcopy.is_none() {
        println!("note: llvm-objcopy not found; the initrd keeps debug info");
    }
    let mut tar = Vec::new();
    for (name, elf) in servers {
        let data = match &objcopy {
            Some(oc) => {
                let s = stripped_dir.join(name);
                sh(Command::new(oc).arg("--strip-debug").arg(elf).arg(&s))?;
                std::fs::read(&s)?
            }
            None => std::fs::read(elf)?,
        };
        check_elf(name, &data)?;
        append(&mut tar, name, &data)?;
    }
    for (name, path) in files {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        append(&mut tar, name, &data)?;
    }
    touchos_tar::finish(&mut tar);
    std::fs::write(out, &tar).with_context(|| format!("writing {}", out.display()))?;
    let names: Vec<&str> = servers.iter().chain(files).map(|(n, _)| n.as_str()).collect();
    println!(
        "initrd: {} ({} KiB): {}",
        out.display(),
        tar.len() / 1024,
        names.join(", ")
    );
    Ok(())
}

/// A static 32-bit ARM executable linked at the user image base.
fn check_elf(name: &str, d: &[u8]) -> Result<()> {
    let ok = d.len() > 64
        && &d[0..4] == b"\x7fELF"
        && u16::from_le_bytes([d[16], d[17]]) == 2; // ET_EXEC
    let machine = u16::from_le_bytes([d[18], d[19]]);
    if !(ok && d[4] == 1 && machine == 40) {
        bail!("{name}: not a static 32-bit ARM executable");
    }
    let entry = u32::from_le_bytes(d[24..28].try_into().unwrap());
    if !(0x40_0000..0x1000_0000).contains(&entry) {
        bail!("{name}: entry point {entry:#x} is outside the user image area");
    }
    Ok(())
}

/// One file into the archive (libs/tar's writer, which the kernel's and
/// init's reader are tested against).
fn append(tar: &mut Vec<u8>, name: &str, data: &[u8]) -> Result<()> {
    touchos_tar::append(tar, name, data).map_err(|e| anyhow::anyhow!("{name}: can't go into the initrd: {e:?}"))
}
