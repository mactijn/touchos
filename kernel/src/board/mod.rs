//! The machine around the CPU: how it boots, its clock and timer, console,
//! watchdog, reboot and power-off, and firmware services.
//!
//! The i.MX boards: the Toon (i.MX27) and QEMU's imx25-pdk.
//!
//! What the rest of the kernel uses, the same on every board: `cmdline`,
//! `KERNEL_HEAP_MAX`, `KERNEL_MMIO` (registers no capability may cover),
//! `reboot`, `poweroff`, `random`, and the modules
//! `clock`, `serial` and `watchdog`. A board without a thing answers as
//! such: a power-off that returns.
//! A board whose interrupt controller isn't the CPU's (the i.MX boards'
//! AITC) also provides `irq::dispatch`, which the ARM IRQ vector calls.
//!
//! The `board-*` cargo feature picks the board (build.rs insists on
//! exactly one). The i.MX board also finishes the boot loader's one-shot set-up
//! (pins, clock gates, the USB host's PHY) before any driver runs: a
//! block whose clock is gated hangs the bus, and the GPIO ports are shared
//! by several drivers' pins.

mod imx;
pub use self::imx::*;
