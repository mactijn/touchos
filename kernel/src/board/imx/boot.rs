//! Booting an i.MX board. `arch/arm/boot.rs` brings the CPU to `arm_kmain`
//! with the MMU on (RAM at 0xc0000000, the UART at 0xfe000000). The loader
//! (xtask/src/arm.rs) put the boot-info block, the config and the initrd at
//! fixed offsets in RAM: `touchos_abi::imx_boot`, the contract between the
//! two.

use core::fmt::Write;

use ::log::{info, warn};
use spin::Once;
use touchos_abi::imx_boot::{self, BOOTINFO, BootBlock, CONFIG, FB_BPP, FB_HEIGHT, FB_WIDTH, FRAMEBUFFER, FRAMEBUFFER_LEN, INITRD, KERNEL_VIRT};

use super::soc;
use crate::mem::{self, MemRegion, PAGE, RegionKind};
use crate::{arch, klog};

/// What the boot-info block says.
struct BootInfo {
    initrd: &'static [u8],
    config: &'static [u8],
    cmdline: &'static str,
}

static BOOT: Once<BootInfo> = Once::new();

/// RAM at offset `off` (through the start-up code's mapping at
/// `KERNEL_VIRT`).
fn ram(off: u64, len: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts((KERNEL_VIRT + off) as usize as *const u8, len) }
}

fn read_boot_info() -> BootInfo {
    let Some(b) = BootBlock::decode(ram(BOOTINFO, imx_boot::BLOCK_LEN)) else {
        warn!("boot: no boot-info block at RAM + {BOOTINFO:#x}: no initrd, no config");
        return BootInfo { initrd: &[], config: &[], cmdline: "" };
    };
    BootInfo {
        initrd: ram(INITRD, b.initrd_len as usize),
        config: ram(CONFIG, b.config_len as usize),
        cmdline: core::str::from_utf8(b.cmdline).unwrap_or(""),
    }
}

/// RAM minus what is in use: the kernel image, the boot-info block, the
/// config and the initrd (kept as RAM), and the framebuffer (not RAM).
fn memory_map(b: &BootInfo, out: &mut [MemRegion; 16]) -> usize {
    unsafe extern "C" {
        static __kernel_end: u8;
    }
    let base = soc::RAM_BASE;
    let page_up = |x: u64| (x + PAGE - 1) & !(PAGE - 1);
    // Before mem::init, so not mem::virt_to_phys: RAM is at KERNEL_VIRT.
    let kernel_end = &raw const __kernel_end as u64 - KERNEL_VIRT;
    // (offset, length, kind): in use, in address order.
    let used = [
        (0, page_up(kernel_end), Some(RegionKind::Ram)),
        (BOOTINFO, page_up(CONFIG - BOOTINFO + b.config.len() as u64), Some(RegionKind::Ram)),
        (FRAMEBUFFER, page_up(FRAMEBUFFER_LEN), None),
        (INITRD, page_up(b.initrd.len() as u64), Some(RegionKind::Ram)),
    ];
    let mut n = 0;
    let mut push = |base: u64, len: u64, kind| {
        if len > 0 && n < out.len() {
            out[n] = MemRegion { base, len, kind };
            n += 1;
        }
    };
    let mut at = 0;
    for (off, len, kind) in used.into_iter().filter(|u| u.1 > 0) {
        // In order and inside RAM (xtask checks the layout): otherwise warn,
        // and clamp rather than hand the same memory out twice.
        let end = (off + len).min(soc::RAM_SIZE);
        if off < at || end < off + len {
            warn!("boot: the region at {:#x} (+{len:#x}) overlaps the one before or ends past RAM", base + off);
        }
        let off = off.max(at);
        if end <= off {
            continue;
        }
        push(base + at, off - at, RegionKind::Usable);
        if let Some(k) = kind {
            push(base + off, end - off, k);
        }
        at = end;
    }
    push(base + at, soc::RAM_SIZE.saturating_sub(at), RegionKind::Usable);
    n
}

#[unsafe(no_mangle)]
extern "C" fn arm_kmain(boot_l1: u32) -> ! {
    klog::init();
    let _ = writeln!(klog::SERIAL.lock(), "\ntouchos {}", env!("CARGO_PKG_VERSION"));
    info!("board: {}, RAM {:#x} + {} MiB", soc::NAME, soc::RAM_BASE, soc::RAM_SIZE >> 20);
    info!("cpu: MIDR {:#010x}", arch::midr());

    let b = BOOT.call_once(read_boot_info);
    info!(
        "boot: cmdline \"{}\", initrd {} KiB, config {} bytes",
        b.cmdline,
        b.initrd.len() / 1024,
        b.config.len()
    );
    arch::paging::set_boot_table(boot_l1 as u64);
    let mut regions = [MemRegion { base: 0, len: 0, kind: RegionKind::Ram }; 16];
    let n = memory_map(b, &mut regions);
    for r in &regions[..n] {
        ::log::debug!(
            "mem: {:#010x}-{:#010x} {}",
            r.base,
            r.base + r.len,
            if r.kind == RegionKind::Usable { "free" } else { "in use" }
        );
    }
    mem::init(KERNEL_VIRT - soc::RAM_BASE, &regions[..n]);
    arch::init();
    super::open_peripheral_bridges();
    super::irq::init();
    super::clock::init();
    super::board_setup();
    // The framebuffer (the layout's: see imx_boot::FRAMEBUFFER), RGB565.
    let framebuffer = Some(crate::Framebuffer {
        phys: soc::RAM_BASE + FRAMEBUFFER,
        len: FRAMEBUFFER_LEN,
        info: touchos_abi::FbInfo {
            width: FB_WIDTH,
            height: FB_HEIGHT,
            pitch: FB_WIDTH * (FB_BPP as u32 / 8),
            bpp: FB_BPP,
            r_shift: 11,
            g_shift: 5,
            b_shift: 0,
            _pad: [0; 3],
        },
    });
    let mut modules = alloc::vec::Vec::new();
    if !b.initrd.is_empty() {
        modules.push(("initrd", b.initrd));
    }
    if !b.config.is_empty() {
        modules.push(("config", b.config));
    }
    // Armed once init runs (init pets it); see watchdog.rs.
    let arg = |s: &str| b.cmdline.split_whitespace().any(|a| a == s);
    super::watchdog::init(if arg("watchdog=off") {
        Some("watchdog=off")
    } else if arg("panic=halt") {
        Some("panic=halt: it can't be stopped once armed")
    } else {
        None
    });
    crate::run(crate::Boot {
        modules,
        init_args: alloc::format!("{} board={}", b.cmdline, super::soc::BOARD),
        framebuffer,
    })
}

/// The kernel command line (from the boot-info block).
pub fn cmdline() -> &'static str {
    BOOT.get().map_or("", |b| b.cmdline)
}
