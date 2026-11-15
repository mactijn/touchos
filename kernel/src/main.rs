//! touchos — a single-purpose OS that turns a touch screen into a
//! wall-mounted Home Assistant panel (the Eneco Toon v1, ARMv5 i.MX27).
//!
//! The board (`board::*`) boots the machine (the boot stage in NAND, QEMU
//! or OpenOCD loads it, `arch/arm/boot.rs` turns on the MMU,
//! `board/imx/boot.rs` takes over) and then calls [`run`], which starts the
//! user space.

#![no_std]
#![no_main]

extern crate alloc;
// The atomic libcalls (__sync_*, __atomic_*_8) on ARMv5.
extern crate touchos_atomics;

mod arch;
mod board;
mod console;
mod entropy;
mod initrd;
mod irqs;
mod cap;
mod klog;
mod mem;
mod sys;
mod task;
mod time;
mod uabi;
mod vm;

use ::log::{info, warn};

/// What the board hands over after bringing up the machine.
pub struct Boot {
    /// Boot modules by name: "initrd", "config", firmware files.
    pub modules: alloc::vec::Vec<(&'static str, &'static [u8])>,
    /// Arguments for init: the kernel command line, plus what the board
    /// knows (e.g. where the config file came from).
    pub init_args: alloc::string::String,
    /// The framebuffer, for init's "framebuffer" capability and the
    /// geometry every process reads in KInfo.
    pub framebuffer: Option<Framebuffer>,
}

pub struct Framebuffer {
    pub phys: u64,
    pub len: u64,
    pub info: touchos_abi::FbInfo,
}

/// Start the system: the scheduler, then init with the boot modules.
pub fn run(boot: Boot) -> ! {
    if let Some(fb) = &boot.framebuffer {
        task::kinfo::set_framebuffer(fb.info);
    }
    task::init();
    // Other cores, where the CPU has them (the ARM926 has one); `cpus=1`
    // keeps to one.
    let cpus = cmdline()
        .split_whitespace()
        .find_map(|a| a.strip_prefix("cpus=")?.parse().ok())
        .unwrap_or(usize::MAX);
    arch::start_cpus(cpus);
    let fb = boot.framebuffer.as_ref().map(|f| (f.phys, f.len));
    let have_init = start_init(&boot.modules, &boot.init_args, fb);
    if !have_init {
        // Nobody would pet it.
        board::watchdog::stop();
    }

    // Normally init handles `autoreboot=`; without init the kernel does.
    let reboot_at = autoreboot_secs().filter(|_| !have_init).map(|secs| {
        warn!("test boot: rebooting in {secs} s (autoreboot, no init)");
        time::now_ms() + secs * 1000
    });
    info!("boot took {} ms", time::now_ms());
    // The kernel's own test failures; init handles the others.
    let test_fail = test_fail().filter(|w| matches!(*w, "panic" | "hang" | "kstack"));
    if let Some(what) = test_fail {
        warn!("test boot: failing on purpose (test_fail={what}) after 20 s");
    }
    // From here on this (boot) thread only logs a heartbeat.
    loop {
        task::sleep_ms(15_000);
        heartbeat();
        if reboot_at.is_some_and(|t| time::now_ms() >= t) {
            info!("autoreboot: rebooting");
            board::reboot();
        }
        if time::now_ms() >= 20_000 {
            match test_fail {
                Some("panic") => panic!("test_fail=panic"),
                Some("kstack") => task::test_kstack_overflow(),
                Some("hang") => {
                    warn!("test_fail=hang: hanging with interrupts off");
                    arch::irq::disable();
                    loop {
                        core::hint::spin_loop();
                    }
                }
                _ => {}
            }
        }
    }
}

/// `test_fail=WHAT` on the command line: break the system on purpose, to
/// test that it recovers by itself.
fn test_fail() -> Option<&'static str> {
    cmdline()
        .split_whitespace()
        .find_map(|a| a.strip_prefix("test_fail="))
}

/// Start the init process (from the initrd) with the capabilities only
/// the first process gets: the system, all device memory and interrupt
/// lines, and the boot modules. Returns false if there is no init.
fn start_init(modules: &[(&str, &'static [u8])], args: &str, framebuffer: Option<(u64, u64)>) -> bool {
    use alloc::string::ToString;
    use cap::{Cap, MemObj, Object};
    let Some(initrd) = modules
        .iter()
        .find(|(n, _)| *n == "initrd")
        .map(|(_, d)| *d)
    else {
        warn!("no initrd module");
        return false;
    };
    let files = initrd::files(initrd);
    let names: alloc::vec::Vec<&str> = files.iter().map(|(n, _)| *n).collect();
    info!("initrd: {} programs: {}", files.len(), names.join(", "));
    let Some(elf) = initrd::find(initrd, "init") else {
        warn!("initrd: no init program");
        return false;
    };
    let mut caps = alloc::vec![
        ("system".to_string(), Cap::new(Object::System)),
        ("mmio_root".to_string(), Cap::new(Object::MmioRoot)),
        ("irq_root".to_string(), Cap::new(Object::IrqRoot)),
    ];
    if let Some((phys, len)) = framebuffer {
        caps.push((
            "framebuffer".to_string(),
            Cap::new(Object::Mmio(alloc::sync::Arc::new(cap::Mmio {
                phys,
                len,
                framebuffer: true,
            }))),
        ));
    }
    for (name, data) in modules {
        let phys = mem::virt_to_phys(data.as_ptr());
        let mut c = Cap::new(Object::Mem(alloc::sync::Arc::new(MemObj::wrap(
            phys,
            data.len() as u64,
        ))));
        c.rights = touchos_abi::rights::MAP_READ;
        caps.push((name.to_string(), c));
    }
    match task::spawn(task::Spawn {
        name: "init",
        elf,
        cmdline: args,
        priority: task::prio::USER_MAX,
        caps,
        death: None,
    }) {
        Ok(_) => true,
        Err(e) => {
            warn!("can't start init: {e}");
            false
        }
    }
}

fn cmdline() -> &'static str {
    board::cmdline()
}

/// `autoreboot=N` on the kernel command line: reboot after N seconds (for
/// test boots).
fn autoreboot_secs() -> Option<u64> {
    cmdline()
        .split_whitespace()
        .find_map(|a| a.strip_prefix("autoreboot=")?.parse().ok())
}

/// Cores running (the idle share is over all of them).
fn cores() -> usize {
    arch::MAX_CPUS
}

/// Uptime and memory every 15 s (shows the timer and scheduler are alive),
/// and since the last one: the address-space switch rate and the share of
/// time the CPU was idle.
fn heartbeat() {
    use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
    static LAST: (AtomicU32, AtomicU64, AtomicU64) = (AtomicU32::new(0), AtomicU64::new(0), AtomicU64::new(0));
    let (threads, user) = task::summary();
    let (n, now) = (arch::SWITCHES.load(Relaxed), time::now_ms());
    let (dn, dt) = (n.wrapping_sub(LAST.0.swap(n, Relaxed)), now - LAST.1.swap(now, Relaxed));
    let idle = task::IDLE_US.load(Relaxed);
    let didle = idle - LAST.2.swap(idle, Relaxed);
    info!(
        "uptime {} s (ticks {}, {threads} threads, {user} in user space, {} MiB free, {} switches/s, {}% idle)",
        now / 1000,
        time::ticks(),
        mem::free_bytes() >> 20,
        dn as u64 * 1000 / dt.max(1),
        (didle / 10 / dt.max(1) / cores() as u64).min(100)
    );
    // Every minute: who was switched to most (the wake-ups behind the
    // switch rate).
    static BEATS: AtomicU32 = AtomicU32::new(0);
    static RUNS_AT: AtomicU64 = AtomicU64::new(0);
    if BEATS.fetch_add(1, Relaxed) % 4 == 3 {
        let dt = now - RUNS_AT.swap(now, Relaxed);
        let mut line = alloc::string::String::new();
        for (name, n) in task::busiest(8) {
            use core::fmt::Write;
            let _ = write!(line, " {name} {}", n as u64 * 1000 / dt.max(1));
        }
        info!("runs/s:{line}");
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    arch::irq::disable();
    arch::halt_others();
    // The panic may have happened while a logger lock was held; we are about
    // to stop the machine, so breaking the locks is fine.
    unsafe {
        klog::SERIAL.force_unlock();
        klog::CONSOLE.force_unlock();
    }
    klog::CONSOLE.lock().set_color(console::RED);
    kprint!("\nKERNEL PANIC: {info}\n");
    // Restart, so the panel recovers by itself (a copy on trial goes back to
    // the active copy). Test boots wait longer, so the message can be read.
    // Without a calibrated clock (a panic very early in boot) we can't wait,
    // and a restart right away would loop fast: stay here then.
    let halt = cmdline().split_whitespace().any(|a| a == "panic=halt");
    if !halt && time::is_calibrated() {
        let secs = if autoreboot_secs().is_some() { 30 } else { 10 };
        kprint!("restarting in {secs} s (panic=halt keeps it here)\n");
        time::busy_wait_us(secs * 1_000_000);
        board::reboot();
    }
    // Staying here on purpose: don't let the watchdog restart us.
    board::watchdog::stop();
    arch::halt_forever()
}
