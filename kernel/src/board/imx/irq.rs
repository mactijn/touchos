//! The interrupt controller: the i.MX27's AITC or the i.MX25's AVIC (the
//! same registers). 64 sources, all routed as normal IRQs (no FIQ), no
//! priority masking. Sources are level-triggered: a driver clears its
//! device's status, nothing is acknowledged here.

use core::sync::atomic::{AtomicUsize, Ordering};

use super::{clock, soc};

const INTCNTL: usize = 0x00;
const NIMASK: usize = 0x04;
const INTENNUM: usize = 0x08;
const INTDISNUM: usize = 0x0c;
const INTENABLEH: usize = 0x10;
const INTENABLEL: usize = 0x14;
const INTTYPEH: usize = 0x18;
const INTTYPEL: usize = 0x1c;
const NIPNDH: usize = 0x58;
const NIPNDL: usize = 0x5c;

static BASE: AtomicUsize = AtomicUsize::new(0);

fn reg(off: usize) -> *mut u32 {
    (BASE.load(Ordering::Relaxed) + off) as *mut u32
}

pub fn init() {
    BASE.store(crate::mem::map_mmio(soc::INTC, 0x100), Ordering::Relaxed);
    unsafe {
        reg(INTENABLEH).write_volatile(0);
        reg(INTENABLEL).write_volatile(0);
        reg(INTTYPEH).write_volatile(0);
        reg(INTTYPEL).write_volatile(0);
        reg(INTCNTL).write_volatile(0);
        reg(NIMASK).write_volatile(0x1f); // no priority masking
    }
}

pub fn enable(n: u32) {
    unsafe { reg(INTENNUM).write_volatile(n) };
}

pub fn disable(n: u32) {
    unsafe { reg(INTDISNUM).write_volatile(n) };
}

/// Lines the kernel handles itself (drivers can't claim them).
pub fn reserved(n: u32) -> bool {
    n == soc::IRQ_GPT
}

/// From the IRQ vector: handle every pending source. A driver's line is
/// masked and the driver signalled (`irqs::fire`); then, if that made a
/// thread ready, user code may give way to it at once.
pub fn dispatch(from_user: bool) {
    let pending = unsafe { (reg(NIPNDH).read_volatile() as u64) << 32 | reg(NIPNDL).read_volatile() as u64 };
    let mut drivers = false;
    for n in 0..64 {
        if pending & 1 << n == 0 {
            continue;
        }
        match n {
            soc::IRQ_GPT => clock::interrupt(from_user),
            _ if crate::irqs::fire(n as usize) => drivers = true,
            _ => {
                ::log::warn!("irq: unexpected source {n}, disabled");
                disable(n);
            }
        }
    }
    if drivers {
        crate::task::resched(from_user);
    }
}
