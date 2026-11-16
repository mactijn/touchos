//! Building touchos for an i.MX board, and running it in QEMU or on the
//! Toon (the commands are in main.rs):
//!
//!   cargo xtask build [--board toon|imx25]   into build/arm-<board>/
//!   cargo xtask qemu                         boot in QEMU -M imx25-pdk (serial on stdout)
//!   cargo xtask load                         load into the Toon's RAM over JTAG and show its console
//!                                            (needs TOON_HOST=user@host: the Pi with tools/toon in ~/touchos-toon)
//!
//! Writing the Toon's NAND is nand.rs.
//!
//! Everything is loaded into RAM at fixed offsets from the board's RAM base
//! (QEMU with `-kernel` and `-device loader`, the Toon with OpenOCD's
//! `load_image`), the kernel ELF by its load addresses:
//! - `BOOTINFO`: the boot-info block (magic, lengths, command line);
//! - `CONFIG`:   the config file (touchos.toml);
//! - `INITRD`:   the initrd (ustar with the servers, the TLS roots
//!   `cacerts.der`, and on the Toon the Wi-Fi firmware `rt3071.bin`, when
//!   build/firmware has it).
//!
//! The offsets, the boot-info block and the boards' RAM bases are
//! `touchos_abi::imx_boot`, where the kernel reads them too (its side of
//! this is kernel/src/board/imx/boot.rs).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use touchos_abi::imx_boot::{self, BOOTINFO, BootBlock, CMDLINE_MAX, CONFIG, CONFIG_MAX, INITRD, INITRD_MAX, KERNEL};

use crate::initrd;
use crate::{root, sh};

/// The Toon's Wi-Fi firmware (the RT5370's 8051): linux-firmware's
/// rt3071.bin, 4 KiB. Not in git; see tools/toon/README.md. It goes into
/// the Toon's initrd, and init gives the Wi-Fi server a read-only copy.
pub const RT3071_FW: &str = "rt3071.bin";

fn wifi_firmware() -> Option<PathBuf> {
    let p = root().join("build/firmware").join(RT3071_FW);
    p.exists().then_some(p)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Board {
    Imx25,
    Toon,
}

impl Board {
    pub const ALL: [Board; 2] = [Board::Imx25, Board::Toon];

    pub fn parse(s: &str) -> Result<Board> {
        match s {
            "imx25" => Ok(Board::Imx25),
            "toon" => Ok(Board::Toon),
            _ => bail!("unknown board {s:?} (imx25 or toon)"),
        }
    }
    /// What the kernel knows of it too.
    fn facts(self) -> imx_boot::Board {
        match self {
            Board::Imx25 => imx_boot::IMX25,
            Board::Toon => imx_boot::TOON,
        }
    }
    pub fn name(self) -> &'static str {
        self.facts().name
    }
    fn ram_base(self) -> u64 {
        self.facts().ram_base
    }
}

pub struct Artifacts {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    pub config: PathBuf,
    pub bootinfo: PathBuf,
}

/// `cargo build` for the ARM target (a JSON target spec: core and alloc are
/// built from source).
fn cargo_arm(target_dir: &str, release: bool) -> Command {
    cargo_arm_sub("build", target_dir, release)
}

/// `cargo <sub>` ("build" or "check") for the ARM target.
fn cargo_arm_sub(sub: &str, target_dir: &str, release: bool) -> Command {
    let root = root();
    let mut c = Command::new(env!("CARGO"));
    c.current_dir(&root).args([
        sub,
        "-Zbuild-std=core,alloc",
        "-Zbuild-std-features=compiler-builtins-mem",
        "-Zjson-target-spec",
        "--target",
        "targets/armv5te-touchos.json",
        "--target-dir",
        target_dir,
    ]);
    if release {
        c.arg("--release");
    }
    c
}

fn profile(release: bool) -> &'static str {
    if release { "release" } else { "debug" }
}

/// The servers' build (one for all ARM boards), and where it puts them.
const USER_TARGET_DIR: &str = "target/arm-user";

pub fn cargo_user(release: bool) -> Command {
    cargo_arm(USER_TARGET_DIR, release)
}

/// `cargo check` of one board's kernel (`board` = None: the servers, which
/// are the same for every ARM board).
pub fn check(board: Option<Board>) -> Result<()> {
    match board {
        Some(b) => sh(cargo_arm_sub("check", &format!("target/arm-{}", b.name()), false).args([
            "--package",
            "touchos-kernel",
            "--features",
            &format!("board-{}", b.name()),
        ])),
        None => {
            let mut c = cargo_arm_sub("check", USER_TARGET_DIR, false);
            for s in initrd::SERVERS {
                c.args(["--package", s.package]);
            }
            sh(&mut c)
        }
    }
}

pub fn user_out_dir(release: bool) -> PathBuf {
    root().join(USER_TARGET_DIR).join("armv5te-touchos").join(profile(release))
}

pub fn build(board: Board, release: bool, cmdline: &str) -> Result<Artifacts> {
    let root = root();
    let out = root.join("build").join(format!("arm-{}", board.name()));
    std::fs::create_dir_all(&out)?;

    // The kernel, per board.
    let kdir = format!("target/arm-{}", board.name());
    sh(cargo_arm(&kdir, release).args(["--package", "touchos-kernel", "--features", &format!("board-{}", board.name())]))?;
    let kernel = out.join("touchos");
    std::fs::copy(
        root.join(&kdir).join("armv5te-touchos").join(profile(release)).join("touchos"),
        &kernel,
    )?;

    // The servers are the same for every ARM board.
    let elfs = initrd::build_servers(release)?;
    let initrd = out.join("touchos.initrd");
    // The TLS roots, shared by init with the programs that speak TLS.
    let mut files = vec![(touchos_certs::INITRD_FILE.to_string(), crate::cacerts::file())];
    if board == Board::Toon {
        match wifi_firmware() {
            Some(fw) => files.push((RT3071_FW.to_string(), fw)),
            None => println!("note: build/firmware/{RT3071_FW} not found; building without the RT5370's firmware"),
        }
    }
    initrd::create(&initrd, &elfs, &files)?;
    // The layout (see the top of this file): nothing may overlap.
    let kernel_end = elf_end(&kernel)?;
    if kernel_end > board.ram_base() + BOOTINFO {
        bail!("the kernel ends at {kernel_end:#x}, past the boot-info block at {:#x}", board.ram_base() + BOOTINFO);
    }
    let initrd_len = std::fs::metadata(&initrd)?.len();
    if initrd_len > INITRD_MAX {
        bail!("the initrd ({} KiB) doesn't fit in RAM after {INITRD:#x}", initrd_len >> 10);
    }

    // The config (see config.rs): the Toon's own, or QEMU's (no secrets).
    let src = crate::config::select(match board {
        Board::Toon => crate::config::Purpose::Toon,
        Board::Imx25 => crate::config::Purpose::Qemu,
    });
    crate::config::check(&src)?;
    let config = out.join("touchos.toml");
    std::fs::copy(&src, &config).with_context(|| format!("copying {}", src.display()))?;
    if std::fs::metadata(&config)?.len() > CONFIG_MAX {
        bail!("{}: too large (at most {} KiB: the framebuffer follows)", src.display(), CONFIG_MAX >> 10);
    }

    let bootinfo = out.join("bootinfo.bin");
    std::fs::write(
        &bootinfo,
        bootinfo_block(
            std::fs::metadata(&initrd)?.len() as u32,
            std::fs::metadata(&config)?.len() as u32,
            &crate::build_id::with_build(cmdline),
        )?,
    )?;
    println!(
        "build: {} (RAM at {:#x}): kernel, initrd, config, boot info in {}",
        board.name(),
        board.ram_base(),
        out.display()
    );
    Ok(Artifacts { kernel, initrd, config, bootinfo })
}

/// The highest physical (load) address of an ELF32's loadable segments.
fn elf_end(path: &Path) -> Result<u64> {
    let f = std::fs::read(path)?;
    let u16_at = |o: usize| u16::from_le_bytes([f[o], f[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap()) as u64;
    if f.len() < 52 || &f[..5] != b"\x7fELF\x01" {
        bail!("{}: not an ELF32 file", path.display());
    }
    let (phoff, phentsize, phnum) = (u32_at(0x1c) as usize, u16_at(0x2a), u16_at(0x2c));
    let mut end = 0;
    for i in 0..phnum {
        let p = phoff + i * phentsize;
        if p + 32 > f.len() {
            bail!("{}: truncated program headers", path.display());
        }
        if u32_at(p) == 1 {
            // PT_LOAD: p_paddr + p_memsz
            end = end.max(u32_at(p + 0x0c) + u32_at(p + 0x14));
        }
    }
    Ok(end)
}

/// The boot-info block (`imx_boot::BootBlock`); `build` checked the
/// lengths against the layout already.
fn bootinfo_block(initrd_len: u32, config_len: u32, cmdline: &str) -> Result<[u8; imx_boot::BLOCK_LEN]> {
    if cmdline.len() > CMDLINE_MAX {
        bail!("command line longer than {CMDLINE_MAX} bytes");
    }
    BootBlock { initrd_len, config_len, cmdline: cmdline.as_bytes() }
        .encode()
        .context("the boot-info block: a length doesn't fit the layout")
}

/// QEMU's imx25-pdk, headless, the console on stdout, for `secs` seconds.
pub fn qemu(a: &Artifacts, secs: u64) -> Result<()> {
    let mut c = Command::new("timeout");
    c.current_dir(out_dir(a)).arg(secs.to_string()).arg("qemu-system-arm").args(qemu_args(a));
    let status = c.status().context("starting qemu-system-arm")?;
    // `timeout` ends QEMU on purpose: 124 is the normal way out.
    if !status.success() && status.code() != Some(124) {
        bail!("qemu-system-arm: {status}");
    }
    Ok(())
}

/// QEMU's imx25-pdk, headless, the console on stdout, until it is stopped.
pub fn qemu_command(a: &Artifacts) -> Command {
    let mut c = Command::new("qemu-system-arm");
    c.current_dir(out_dir(a)).args(qemu_args(a));
    c
}

/// Where `a` was built (build/arm-<board>/): QEMU runs there.
fn out_dir(a: &Artifacts) -> &Path {
    a.kernel.parent().unwrap_or(Path::new("."))
}

/// qemu-system-arm's arguments for `a`.
fn qemu_args(a: &Artifacts) -> Vec<String> {
    let base = Board::Imx25.ram_base();
    let loader = |p: &Path, off: u64| format!("loader,file={},addr={:#x},force-raw=on", p.display(), base + off);
    let mut c: Vec<String> = Vec::new();
    let mut args = |v: &[&str]| c.extend(v.iter().map(|s| s.to_string()));
    args(&["-M", "imx25-pdk", "-m", &format!("{}M", imx_boot::RAM_SIZE >> 20), "-display", "none"]);
    // For screenshots: tools/toon/fbshot asks it for the framebuffer.
    // Relative to QEMU's working directory (out_dir): a socket's path may
    // have at most 107 bytes, which a deep checkout's absolute path exceeds.
    args(&["-monitor", "unix:monitor.sock,server,nowait", "-serial", "stdio"]);
    // The FEC on QEMU's user network; the settings page on localhost:8080.
    args(&["-nic", "user,hostfwd=tcp:127.0.0.1:8080-:80"]);
    args(&["-kernel", &a.kernel.display().to_string()]);
    args(&["-device", &loader(&a.bootinfo, BOOTINFO)]);
    args(&["-device", &loader(&a.config, CONFIG)]);
    args(&["-device", &loader(&a.initrd, INITRD)]);
    // E.g. QEMU_EXTRA="-device tmp105,bus=i2c-bus.0,address=0x48" (an I²C
    // device to talk to).
    if let Ok(extra) = std::env::var("QEMU_EXTRA") {
        c.extend(extra.split_whitespace().map(String::from));
    }
    c
}

/// Load everything into the Toon's RAM over JTAG (on the Pi, with
/// tools/toon's toon-ocd) and show its console for `secs` seconds.
///
/// The Toon is reset and U-Boot runs to its prompt first (toonconsole's
/// autostop stops it there; set back to "auto" here, after `nandboot`):
/// U-Boot sets up the display (the kernel takes the LCD controller over as
/// it is), the clocks and the backlight. Only then is the CPU halted and
/// loaded.
pub fn load(a: &Artifacts, secs: u64) -> Result<()> {
    let host = std::env::var("TOON_HOST")
        .context("set TOON_HOST=user@host: the Pi with tools/toon in ~/touchos-toon")?;
    let base = Board::Toon.ram_base();
    sh(Command::new("ssh").args([&host, "mkdir -p touchos-toon/run"]))?;
    let mut scp = Command::new("scp");
    scp.arg("-q").args([&a.kernel, &a.initrd, &a.config, &a.bootinfo]).arg(format!("{host}:touchos-toon/run/"));
    sh(&mut scp)?;
    let load = format!(
        "halt; load_image $HOME/touchos-toon/run/bootinfo.bin {:#x} bin; \
         load_image $HOME/touchos-toon/run/touchos.toml {:#x} bin; \
         load_image $HOME/touchos-toon/run/touchos.initrd {:#x} bin; \
         load_image $HOME/touchos-toon/run/touchos 0 elf; resume {:#x}",
        base + BOOTINFO,
        base + CONFIG,
        base + INITRD,
        base + KERNEL
    );
    // The console log from the moment of the load on, for `secs` seconds.
    // toon-ocd exits non-zero when the reset or a load failed: then stop,
    // instead of showing the console as if the new build were running.
    let script = format!(
        "cd ~/touchos-toon && log=~/toon-backup/serial.log && size=$(stat -c %s $log) || exit 2; \
         echo auto > autostop; [ -e guard ] || : > guard; \
         ./toon-ocd reset > /dev/null || {{ echo 'toon: the JTAG reset failed' >&2; exit 1; }}; \
         for i in $(seq 60); do tail -c +$((size + 1)) $log | grep -q 'U-Boot> ' && break; sleep 0.5; done; \
         tail -c +$((size + 1)) $log | grep -q 'U-Boot> ' || {{ echo 'toon: no U-Boot prompt after the reset' >&2; exit 1; }}; \
         out=$(./toon-ocd -k \"{load}\"); rc=$?; \
         printf '%s\\n' \"$out\" | grep -E 'Error|ERROR|state'; \
         [ $rc -eq 0 ] || {{ echo 'toon: the JTAG load failed' >&2; exit 1; }}; \
         timeout {secs} tail -c +$((size + 1)) -F $log | stdbuf -oL cut -c10- ; true"
    );
    sh(Command::new("ssh").arg(&host).arg(script)).context("loading the Toon over JTAG")?;
    Ok(())
}
