//! The Toon's NAND: write touchos into it, and boot it from there.
//!
//!   cargo xtask flash [--slot A|B] [--config] [--release] [--cmdline ARGS] [--secs N]
//!   cargo xtask nandboot [--secs N]
//!   cargo xtask bundle [--release] [--cmdline ARGS]
//!
//! `flash` builds the boot stage (tools/toon/nandboot) as the uImage
//! U-Boot's stock `boot_nand` loads, a system copy (the kernel as a flat
//! binary, the initrd, the boot info) and the config, packs them into
//! one payload (libs/nand's `FlashPayload`; the config only with
//! `--config`: otherwise the one saved on the Toon stays, settings changes
//! included), loads the payload and the
//! stage into RAM over JTAG at U-Boot's prompt, and starts the stage's
//! flasher; it writes, reads back and compares, and says so on the
//! console. Blocks 0-23 (U-Boot, its env, the splash) are never written.
//!
//! `nandboot` lets the Toon boot by itself: toonconsole stops typing
//! U-Boot's password (autostop off) and the guard goes, so U-Boot's own
//! `bootcmd` runs; then a JTAG reset. From then on every reset (the
//! watchdog's too) boots touchos from NAND, until `load` is used
//! again (it sets autostop back to "auto").

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use touchos_nand::image::{self, FlashPayload, Part, SysHeader};
use touchos_nand::nfc::{BLOCK, PAGE};

use crate::arm::Artifacts;
use crate::{root, sh};

/// Where the stage runs (tools/toon/nandboot/link.ld), and its entries.
const STAGE: u32 = 0xa700_0000;
const STAGE_FLASH: u32 = STAGE + 4;
/// Where the flasher finds the payload.
const PAYLOAD: u32 = 0xa200_0000;
/// The kernel's load address (libs/abi's imx_boot: RAM + KERNEL).
const KERNEL: u64 = 0xa000_8000;

pub fn flash(a: &Artifacts, slot: u8, with_config: bool, secs: u64) -> Result<()> {
    let host = std::env::var("TOON_HOST").context("set TOON_HOST=user@host: the Pi with tools/toon in ~/touchos-toon")?;
    let out = a.kernel.parent().unwrap().to_path_buf();
    let stage_elf = build_stage()?;
    let stage = flat(&stage_elf, STAGE as u64)?;
    let mut boot = image::uimage_header(&stage, STAGE, "touchos nandboot", now()).to_vec();
    boot.extend_from_slice(&stage);
    if boot.len() > touchos_nand::layout::BOOT_MAX {
        bail!("the boot stage ({} bytes) is over 3 MiB", boot.len());
    }
    let system = system_copy(a)?;
    let config = if with_config { std::fs::read(&a.config)? } else { Vec::new() };
    if config.len() > image::CONFIG_MAX {
        bail!("{}: over {} KiB", a.config.display(), image::CONFIG_MAX >> 10);
    }

    // The payload: a header page, then the three parts, page-aligned.
    let mut payload = vec![0u8; PAGE];
    let mut add = |data: &[u8]| {
        let offset = payload.len() as u32;
        payload.extend_from_slice(data);
        payload.resize(payload.len().next_multiple_of(PAGE), 0xff);
        Part { offset, len: data.len() as u32, crc: image::crc32(0, data) }
    };
    let (b, s, c) = (add(&boot), add(&system), add(&config));
    let header = FlashPayload { boot: b, system: s, config: c, slot }.encode();
    payload[..header.len()].copy_from_slice(&header);
    let payload_path = out.join("nand-payload.bin");
    std::fs::write(&payload_path, &payload)?;
    println!(
        "nand: boot stage {} bytes, system copy {} KiB, config {} bytes; payload {} KiB",
        boot.len(),
        system.len() >> 10,
        config.len(),
        payload.len() >> 10
    );

    sh(Command::new("ssh").args([&host, "mkdir -p touchos-toon/run"]))?;
    sh(Command::new("scp").arg("-q").arg(&payload_path).arg(&stage_elf).arg(format!("{host}:touchos-toon/run/")))?;
    let stage_name = stage_elf.file_name().unwrap().to_string_lossy();
    let load = format!(
        "halt; load_image $HOME/touchos-toon/run/nand-payload.bin {PAYLOAD:#x} bin; \
         load_image $HOME/touchos-toon/run/{stage_name} 0 elf; resume {STAGE_FLASH:#x}"
    );
    let script = format!(
        "cd ~/touchos-toon && log=~/toon-backup/serial.log && size=$(stat -c %s $log) || exit 2; \
         echo auto > autostop; : > guard; \
         ./toon-ocd reset > /dev/null || {{ echo 'nand: the JTAG reset failed' >&2; exit 1; }}; \
         for i in $(seq 60); do tail -c +$((size + 1)) $log | grep -q 'U-Boot> ' && break; sleep 0.5; done; \
         tail -c +$((size + 1)) $log | grep -q 'U-Boot> ' || {{ echo 'nand: no U-Boot prompt after the reset' >&2; exit 1; }}; \
         out=$(TOON_OCD_TIMEOUT=300 ./toon-ocd -k \"{load}\"); rc=$?; \
         printf '%s\\n' \"$out\" | grep -E 'Error|ERROR|state'; \
         [ $rc -eq 0 ] || {{ echo 'nand: the JTAG load failed' >&2; exit 1; }}; \
         for i in $(seq {secs}); do tail -c +$((size + 1)) $log | grep -q -E 'flash done|FAILED|nothing written|nandboot: (flash:|panic)|didn.t read back' && break; sleep 1; done; \
         tail -c +$((size + 1)) $log | grep -a nandboot | cut -c10-; \
         tail -c +$((size + 1)) $log | grep -q 'flash done'"
    );
    sh(Command::new("ssh").arg(&host).arg(script)).context("flashing the Toon's NAND")?;
    Ok(())
}

/// Boot the Toon from NAND (see the module's notes) and show its console.
pub fn nandboot(secs: u64) -> Result<()> {
    let host = std::env::var("TOON_HOST").context("set TOON_HOST=user@host: the Pi with tools/toon in ~/touchos-toon")?;
    let script = format!(
        "cd ~/touchos-toon && log=~/toon-backup/serial.log && size=$(stat -c %s $log) || exit 2; \
         echo off > autostop; rm -f guard; \
         ./toon-ocd reset > /dev/null || {{ echo 'nand: the JTAG reset failed' >&2; exit 1; }}; \
         timeout {secs} tail -c +$((size + 1)) -F $log | stdbuf -oL cut -c10- ; true"
    );
    sh(Command::new("ssh").arg(&host).arg(script)).context("booting the Toon from NAND")?;
    Ok(())
}

/// A system copy (libs/nand's layout): the header page in the first
/// block (the rest of that block unused), the kernel from the second
/// block on, then the initrd and the boot info, page-aligned.
fn system_copy(a: &Artifacts) -> Result<Vec<u8>> {
    let kernel = flat(&a.kernel, KERNEL)?;
    let initrd = std::fs::read(&a.initrd)?;
    let bootinfo = std::fs::read(&a.bootinfo)?;
    let mut img = vec![0xffu8; BLOCK];
    let mut add = |data: &[u8]| {
        let offset = img.len() as u32;
        img.extend_from_slice(data);
        img.resize(img.len().next_multiple_of(PAGE), 0xff);
        Part { offset, len: data.len() as u32, crc: image::crc32(0, data) }
    };
    let (k, i, b) = (add(&kernel), add(&initrd), add(&bootinfo));
    let id = crate::build_id::build_id();
    let mut build = [0u8; 48];
    let n = id.len().min(47);
    build[..n].copy_from_slice(&id.as_bytes()[..n]);
    let h = SysHeader { build, kernel: k, initrd: i, bootinfo: b }.encode();
    img[..h.len()].copy_from_slice(&h);
    Ok(img)
}

/// A signed update for the Toon's settings page (libs/update's
/// nand_copy): the kernel as a flat binary, the initrd, the boot info.
pub fn bundle(a: &Artifacts) -> Result<PathBuf> {
    use touchos_update::nand_copy;
    let build = crate::build_id::build_id();
    let files = vec![
        (nand_copy::KERNEL.to_string(), flat(&a.kernel, KERNEL)?),
        (nand_copy::INITRD.to_string(), std::fs::read(&a.initrd)?),
        (nand_copy::BOOTINFO.to_string(), std::fs::read(&a.bootinfo)?),
    ];
    crate::update::sign(&build, &files, nand_copy::check)
}

/// The stage, built in its own workspace (release: small and fast).
fn build_stage() -> Result<PathBuf> {
    let dir = root().join("tools/toon/nandboot");
    let mut c = Command::new(env!("CARGO"));
    c.current_dir(&dir).args(["build", "--release"]);
    // The outer cargo's variables would point the build elsewhere.
    for (k, _) in std::env::vars() {
        if k.starts_with("CARGO_") && k != "CARGO_HOME" {
            c.env_remove(k);
        }
    }
    sh(&mut c)?;
    Ok(dir.join("target/armv5te-none-eabi/release/toon-nandboot"))
}

/// An ELF32's loadable segments as one flat image from `base` (their
/// physical addresses; .bss is left out, both the kernel and the stage
/// clear their own).
fn flat(path: &Path, base: u64) -> Result<Vec<u8>> {
    let f = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let u16_at = |o: usize| u16::from_le_bytes([f[o], f[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap()) as u64;
    if f.len() < 52 || &f[..5] != b"\x7fELF\x01" {
        bail!("{}: not an ELF32 file", path.display());
    }
    let (phoff, phentsize, phnum) = (u32_at(0x1c) as usize, u16_at(0x2a), u16_at(0x2c));
    let mut img: Vec<u8> = Vec::new();
    for i in 0..phnum {
        let p = phoff + i * phentsize;
        let (kind, off, paddr, filesz) = (u32_at(p), u32_at(p + 4) as usize, u32_at(p + 0x0c), u32_at(p + 0x10) as usize);
        if kind != 1 || filesz == 0 {
            continue;
        }
        if paddr < base {
            bail!("{}: a segment at {paddr:#x}, below {base:#x}", path.display());
        }
        let at = (paddr - base) as usize;
        if img.len() < at + filesz {
            img.resize(at + filesz, 0);
        }
        img[at..at + filesz].copy_from_slice(&f[off..off + filesz]);
    }
    if img.is_empty() {
        bail!("{}: nothing to load", path.display());
    }
    Ok(img)
}

fn now() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)
}
