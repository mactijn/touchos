//! The first ring-3 process. Without arguments it says hello and exits; for
//! the `selftest` boot, init starts copies with test modes:
//!
//! - `fpu N`: fill SSE registers with a pattern, let other processes run
//!   (and be preempted) many times, check that the pattern survived.
//! - `spin MS`: burn CPU (the dashboard must stay smooth: preemption).
//! - `sleep MS`: block in WAIT; check the time it took.
//! - `vm MIB`: grow the heap by MIB MiB (VM_ALLOC) and use it.
//! - `crash`: write to address 0x10 (must kill only this process).
//! - `crash-kernel`: write to a kernel address (must also fail cleanly).
//! - `bad-syscall`: invalid syscall number and pointer (must return errors).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use log::{error, info};
use touchos_rt::time;

touchos_rt::entry!(main);

fn main() {
    let args = touchos_rt::cmdline();
    let mut words = args.split_whitespace();
    let mode = words.next().unwrap_or("");
    let n: u64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0);
    match mode {
        "" => {
            let v: Vec<u64> = (1..=10).collect();
            info!(
                "hello from ring 3! heap works (sum {}), clock says {} ms since boot",
                v.iter().sum::<u64>(),
                time::now_ms()
            );
        }
        "fpu" => fpu(n),
        "spin" => {
            let t0 = time::now_ms();
            let mut loops = 0u64;
            while time::now_ms() - t0 < n {
                loops += 1;
                core::hint::black_box(loops);
            }
            info!("spun for {n} ms ({loops} loops)");
        }
        "sleep" => {
            let t0 = time::now_us();
            time::sleep_ms(n);
            let took = (time::now_us() - t0) / 1000;
            if took >= n && took < n + 50 {
                info!("PASS: slept {took} ms (asked {n})");
            } else {
                error!("FAIL: slept {took} ms (asked {n})");
            }
        }
        "vm" => {
            let mut v: Vec<u8> = Vec::new();
            v.resize((n * 1024 * 1024) as usize, 0);
            for (i, b) in v.iter_mut().enumerate().step_by(4096) {
                *b = i as u8;
            }
            let sum: u64 = v.iter().step_by(4096).map(|&b| b as u64).sum();
            info!("PASS: heap grew by {n} MiB (checksum {sum})");
        }
        "crash" => {
            info!("writing to address 0x10; this process should be killed");
            unsafe { (0x10 as *mut u64).write_volatile(1) };
            error!("FAIL: still alive after writing to 0x10");
        }
        "crash-kernel" => {
            info!("writing to a kernel address; this process should be killed");
            unsafe { (KERNEL_ADDR as *mut u64).write_volatile(1) };
            error!("FAIL: still alive after writing to kernel memory");
        }
        "bad-syscall" => {
            let unknown = unsafe { raw_syscall(9999, 0, 0) };
            // LOG with a pointer into the kernel half: must be refused.
            let bad_ptr = unsafe { raw_syscall(0, KERNEL_ADDR, 16) };
            // LOG from the counter page: user-readable, but a device page
            // the kernel can't copy from (on ARM this used to panic it).
            let device = unsafe { raw_syscall(0, touchos_rt::abi::layout::COUNTER_VA, 16) };
            if unknown < 0 && bad_ptr < 0 && device < 0 {
                info!("PASS: bad syscalls refused ({unknown}, {bad_ptr}, {device})");
            } else {
                error!("FAIL: bad syscalls returned {unknown}, {bad_ptr}, {device}");
            }
        }
        other => error!("unknown mode \"{other}\""),
    }
}

/// An address in the kernel's part of every address space.
const KERNEL_ADDR: usize = 0xc010_0000;

unsafe fn raw_syscall(n: usize, a: usize, b: usize) -> isize {
    let r: isize;
    unsafe {
        core::arch::asm!("svc #0", in("r7") n, inlateout("r0") a as isize => r, inlateout("r1") b => _,
            in("r2") 3usize, in("r3") 0usize, options(nostack));
    }
    r
}

/// No FPU on ARMv5: nothing to keep across switches.
fn fpu(_n: u64) {
    info!("SKIP: no FPU registers on this CPU");
}

