//! Process entry: `_start` -> heap and logger set-up -> the program's `main`.

use core::sync::atomic::{AtomicPtr, Ordering};
use touchos_abi::{BOOTINFO_MAGIC, BootInfo, exit};

use crate::{heap, logger, sys};

static BOOT_INFO: AtomicPtr<BootInfo> = AtomicPtr::new(core::ptr::null_mut());

unsafe extern "Rust" {
    /// Defined by `touchos_rt::entry!`.
    fn __touchos_main();
}

/// The kernel enters here (A32) with `r0` = address of our [`BootInfo`].
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov fp, #0",     // end of the frame-pointer chain for backtraces
        "mov lr, #0",
        "bic sp, sp, #7", // AAPCS: 8-byte aligned
        "bl {start}",
        "udf #0",
        start = sym rust_start,
    );
}

extern "C" fn rust_start(bootinfo: *mut BootInfo) -> ! {
    BOOT_INFO.store(bootinfo, Ordering::Relaxed);
    logger::init();
    heap::init();
    unsafe { __touchos_main() };
    sys::exit(exit::OK)
}

/// Our [`BootInfo`] (name, command line, named capabilities).
pub(crate) fn boot_info() -> Option<&'static BootInfo> {
    let p = BOOT_INFO.load(Ordering::Relaxed);
    let bi = unsafe { p.as_ref()? };
    (bi.magic == BOOTINFO_MAGIC).then_some(bi)
}

/// The value of `key=value` on our command line (the first one).
pub fn arg(key: &str) -> Option<&'static str> {
    cmdline()
        .split_whitespace()
        .find_map(|a| a.strip_prefix(key)?.strip_prefix('='))
}

/// Whether the bare word `name` is on our command line.
pub fn flag(name: &str) -> bool {
    cmdline().split_whitespace().any(|a| a == name)
}

pub fn cmdline() -> &'static str {
    boot_info().map_or("", |b| {
        let n = (b.cmdline_len as usize).min(b.cmdline.len());
        core::str::from_utf8(&b.cmdline[..n]).unwrap_or("")
    })
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    ::log::error!("panic: {info}");
    sys::exit(exit::PANIC)
}
