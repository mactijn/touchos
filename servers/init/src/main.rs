//! init: the first user process. It holds the powerful capabilities (the
//! system, device memory, interrupt lines, the boot modules), starts the
//! other programs from the initrd with just what each needs, and restarts
//! them when they crash (with backoff; it gives up on a program that keeps crashing).
//!
//! - `programs`: what runs, with which capabilities (the table);
//! - `supervise`: starting, noticing deaths, restarting, giving up;
//! - `health`: the watchdog's signs of life, and confirming this copy of
//!   the system (or sending it back);
//! - `selftest`: the `selftest` boot and the `test_fail=crash:` hook;
//! - `board`: the machine's devices, one record per board.
//!
//! Kernel command line options it handles: `autoreboot=N` (reboot after N
//! seconds, for test boots), `selftest`, `noui`, `test_fail=`, and
//! `slot=A|B` (which copy of the system this is, from the boot stage): a
//! copy on trial that runs a minute without any program crashing, with a
//! network address, becomes the active copy (health.rs).

#![no_std]
#![no_main]

extern crate alloc;

mod board;
mod health;
mod programs;
mod selftest;
mod supervise;

use alloc::string::String;
use alloc::vec::Vec;
use log::{error, info, warn};
use touchos_rt::{cap, caps::Mapping, sys, time};

use supervise::{Init, SECOND};

touchos_rt::entry!(main);

/// The kernel command line, as init uses it (parsed once).
pub struct Opts {
    pub selftest: bool,
    /// Leave the screen to the kernel's log (debugging without network).
    pub noui: bool,
    /// `slot=` as given ("-" if none) and `build=`, for the servers.
    pub slot_arg: &'static str,
    pub build: &'static str,
    /// The copy of the system, when `slot=` names one.
    pub slot: Option<char>,
    /// `test_fail=`: any value makes a test boot (never confirmed).
    pub test_fail: Option<&'static str>,
    /// `setup_ap=force` (a test boot, never confirmed): the Wi-Fi server
    /// runs the setup network even with a cable in, and net prefers it.
    pub setup_ap_force: bool,
    pub autoreboot: Option<u64>,
}

impl Opts {
    fn parse() -> Opts {
        let slot_arg = touchos_rt::arg("slot").unwrap_or("-");
        Opts {
            selftest: touchos_rt::flag("selftest"),
            noui: touchos_rt::flag("noui"),
            slot_arg,
            build: touchos_rt::arg("build").unwrap_or("-"),
            slot: match slot_arg {
                "A" => Some('A'),
                "B" => Some('B'),
                _ => None,
            },
            test_fail: touchos_rt::arg("test_fail"),
            setup_ap_force: touchos_rt::arg("setup_ap") == Some("force"),
            autoreboot: touchos_rt::arg("autoreboot").and_then(|s| s.parse().ok()),
        }
    }

    /// `test_fail=crash:NAME`.
    fn test_crash(&self) -> Option<&'static str> {
        self.test_fail.and_then(|t| t.strip_prefix("crash:"))
    }
}

fn main() {
    let opts = Opts::parse();
    let system = cap("system").expect("init needs the system capability");
    let Some(initrd_cap) = cap("initrd") else {
        error!("no initrd");
        return;
    };
    let initrd = Mapping::map(initrd_cap, false).expect("map initrd");
    let files: Vec<(String, u64, u64)> = touchos_tar::files(initrd.bytes())
        .map(|f| (String::from(f.name), f.offset as u64, f.data.len() as u64))
        .collect();

    let board = board::detect();
    info!("board {} ({})", board.name, board.description);
    info!("system copy {}, build {}", opts.slot_arg, opts.build);
    let table = programs::table(board, &opts, system, &initrd, &files);
    let mut init = Init::new(table.programs, initrd_cap, files, opts.test_crash());
    let mut tests = selftest::Tests::new(opts.selftest, table.tests_from);
    for i in 0..init.children.len() {
        if opts.noui && init.children[i].prog.name == "ui" {
            warn!("noui: not starting the dashboard");
            continue;
        }
        init.start(i);
        tests.started(&init.children[i], i, time::now_us());
    }

    let boot_time = time::now_us();
    let mut trial = health::Trial::new(&opts, &init, table.net_state, table.nand_storage, boot_time);
    let mut watchdog = health::Watchdog::new(system, &opts, boot_time);
    let mut crash = selftest::CrashTest::new(opts.test_crash(), boot_time);
    let reboot_at = opts.autoreboot.map(|s| time::now_us() + s * SECOND);
    if let Some(s) = opts.autoreboot {
        warn!("test boot: rebooting in {s} s (autoreboot)");
    }

    loop {
        watchdog.step(&init, system, time::now_us());
        let deadline = [init.deadline(), watchdog.deadline(), reboot_at, trial.deadline(), crash.deadline(), tests.deadline()]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(u64::MAX);
        let bits = sys::wait(deadline).unwrap_or(0);
        let now = time::now_us();
        init.step(bits, now, |name| crash.restarted(name, now));
        crash.step(&init, now);
        tests.step(&init, now);
        trial.step(init.failures, system, now);
        if reboot_at.is_some_and(|t| t <= now) {
            info!("autoreboot: rebooting");
            let _ = sys::reboot(system);
        }
    }
}
