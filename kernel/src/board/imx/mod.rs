//! Freescale i.MX25/i.MX27 boards (ARM926EJ-S): the Eneco Toon v1 (i.MX27)
//! and, for testing, QEMU's imx25-pdk. The two share their peripherals'
//! designs (UART, AVIC/AITC, GPT, FEC, I2C, watchdog); only addresses,
//! interrupt numbers and the RAM base differ (`soc`).

pub mod boot;
mod ccm;
pub mod clock;
// The i.MX27's GPIO (the i.MX25's is another design); only the Toon's
// bring-up uses it.
#[cfg(feature = "board-toon")]
mod gpio;
pub mod irq;
mod rng;
pub mod serial;
#[cfg(feature = "board-toon")]
mod toon;
pub mod watchdog;

pub use boot::cmdline;

/// The kernel heap's size limit (these boards have 128 MiB).
pub const KERNEL_HEAP_MAX: u64 = 8 << 20;

/// Device registers only the kernel uses: the interrupt
/// controller, since drivers get its lines one by one (`irqs`). Candidates
/// still: the watchdog, the UART and the GPT (whose counter page every
/// process maps read-only on purpose, which must stay allowed).
pub const KERNEL_MMIO: &[(u64, u64)] = &[(soc::INTC, 0x1000)];

/// Addresses and interrupt numbers of the chip.
pub mod soc {
    use touchos_abi::imx_boot;

    // What the loader knows of the board too: from touchos_abi::imx_boot,
    // like xtask's numbers and the linker script's (build.rs).
    pub const BOARD: &str = chip::FACTS.name;
    /// RAM: `RAM_SIZE` (128 MiB) from here.
    pub const RAM_BASE: u64 = chip::FACTS.ram_base;
    pub const RAM_SIZE: u64 = imx_boot::RAM_SIZE;
    pub const UART1: u64 = chip::FACTS.uart;

    #[cfg(feature = "board-toon")]
    mod chip {
        pub const FACTS: super::imx_boot::Board = super::imx_boot::TOON;
        pub const NAME: &str = "Eneco Toon v1 (i.MX27)";
        pub const WDOG: u64 = 0x1000_2000;
        /// The interrupt controller (AITC).
        pub const INTC: u64 = 0x1004_0000;
        /// GPT1: the older i.MX1/2 layout (TCTL, TCMP, TCN).
        pub const GPT: u64 = 0x1000_3000;
        pub const GPT_V2: bool = false;
        pub const IRQ_GPT: u32 = 26;
        /// GPT2, on the peripheral clock: a fast counter for jitter.
        pub const GPT_FAST: u64 = 0x1000_4000;
        /// No RNGC on the i.MX27; its RNG is in the SAHARA2 crypto engine.
        pub const RNGC: Option<u64> = None;
        pub const SAHARA: Option<u64> = Some(0x1002_5000);
        /// The AIPI peripheral bridges: their access registers (PAR) make
        /// peripherals supervisor-only, which U-Boot leaves on for all.
        pub const AIPI: &[u64] = &[0x1000_0000, 0x1002_0000];
    }

    #[cfg(feature = "board-imx25")]
    mod chip {
        pub const FACTS: super::imx_boot::Board = super::imx_boot::IMX25;
        pub const NAME: &str = "QEMU imx25-pdk (i.MX25)";
        pub const WDOG: u64 = 0x53fd_c000;
        /// The interrupt controller (AVIC: the same layout as the AITC).
        pub const INTC: u64 = 0x6800_0000;
        /// GPT1: the newer i.MX3 layout (GPTCR, OCR1, CNT).
        pub const GPT: u64 = 0x53f9_0000;
        pub const GPT_V2: bool = true;
        pub const IRQ_GPT: u32 = 54;
        pub const GPT_FAST: u64 = 0x53f8_c000;
        /// The i.MX25's RNGC (QEMU models it).
        pub const RNGC: Option<u64> = Some(0x53fb_0000);
        pub const SAHARA: Option<u64> = None;
        /// QEMU doesn't model the peripheral bridges' access control.
        pub const AIPI: &[u64] = &[];
    }

    pub use chip::*;
}

/// Set up what this board needs beyond the chip, once the clock runs (so
/// the set-up can wait): on the Toon, its pins and the Wi-Fi's USB port.
pub fn board_setup() {
    #[cfg(feature = "board-toon")]
    toon::setup();
}

/// Read-modify-write a device register (mapped): clear the bits in `clear`,
/// then set those in `set`.
fn rmw(reg: usize, clear: u32, set: u32) {
    let r = reg as *mut u32;
    unsafe { r.write_volatile((r.read_volatile() & !clear) | set) };
}

/// Let user mode reach device registers. Drivers run in user processes, and
/// the MMU already limits each to the registers its capabilities map, so
/// the bridges' own "supervisor only" setting is lifted.
pub fn open_peripheral_bridges() {
    for &bridge in soc::AIPI {
        let par = (crate::mem::map_mmio(bridge, 0x10) + 0x08) as *mut u32;
        unsafe { par.write_volatile(0) };
    }
}

/// Restart the chip: the watchdog's software reset (WCR.SRS = 0).
pub fn reboot() -> ! {
    watchdog::reset_now()
}

/// No way to switch off (the boards have no power switch): returns at once.
pub fn poweroff() {}

/// Random bytes for `RANDOM`, from the kernel's pool (fed by `clock`'s
/// jitter sampling and, where there is one, a hardware RNG).
pub fn random(out: &mut [u8]) {
    rng::refresh();
    crate::entropy::fill(out);
}
