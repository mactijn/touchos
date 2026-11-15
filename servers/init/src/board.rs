//! The machines init runs on: one record per board, chosen once at start.
//! It holds everything board-specific that init hands out (device pages,
//! interrupt lines, which server drives which controller); programs.rs
//! builds the program list from it.

use log::error;
use touchos_abi::imx_boot;
use touchos_rt::{cap, sys};

pub struct Board {
    /// Short name: `board=` from the kernel. The platform server gets it
    /// as `board=` too.
    pub name: &'static str,
    /// For the log.
    pub description: &'static str,
    /// The NAND controller's page, for the storage server (the settings,
    /// and with them updates).
    pub nand: Option<u64>,
    /// The I²C controllers (and GPIOs) for the platform server.
    pub platform: Option<Platform>,
    /// The EHCI the usb server drives.
    pub usb: Option<Ehci>,
    pub wifi: Wifi,
    /// The FEC's (Ethernet) register page and interrupt line.
    pub fec: Option<(u64, u32)>,
    /// The boiler link (the Toon's UART2 towards the Ketelmodule): its
    /// register page and interrupt line. Read-only (servers/boiler).
    pub boiler: Option<(u64, u32)>,
    /// The Z-Wave controller (the Toon's UART3, a ZW0301 with the Serial
    /// API): its register page and interrupt line (servers/zwave).
    pub zwave: Option<(u64, u32)>,
    /// The backlight's PWM (its register page).
    pub backlight: Option<u64>,
    /// A free-running hardware counter for the MMIO self-test: its page,
    /// and the selftest's arguments (offset of the counter, its width in bits).
    pub counter: (u64, &'static str),
    /// The heap growth the vm self-test asks for.
    pub vm_test: &'static str,
}

pub struct Platform {
    /// (capability name, register page) of each controller.
    pub mmio: &'static [(&'static str, u64)],
    /// (capability name, interrupt line) of each controller that has one.
    pub irqs: &'static [(&'static str, u32)],
    /// It sets the clock from the board's RTC (the "system" capability
    /// with the right to).
    pub sets_time: bool,
}

/// A ChipIdea EHCI: the USB block's page, and the driver's arguments (the
/// controller's offset in it, and whether its port has a ULPI PHY).
#[derive(Clone, Copy)]
pub struct Ehci {
    pub page: u64,
    pub args: &'static str,
    /// Its interrupt line (capability "irq").
    pub irq: u32,
}

pub enum Wifi {
    None,
    /// The RT5370, alone on the board's EHCI: the Wi-Fi server drives the
    /// controller itself (and the password stays there). Its firmware is a
    /// file in the initrd.
    Rt5370 {
        ehci: Ehci,
        firmware_file: &'static str,
    },
}

/// QEMU's imx25-pdk: the test bench for the Toon's drivers. (The ARM
/// boards have 128 MiB, hence the smaller vm test.)
pub static IMX25: Board = Board {
    name: imx_boot::IMX25.name,
    description: "QEMU imx25-pdk",
    nand: None,
    platform: Some(Platform {
        mmio: &[("i2c1", 0x43f8_0000)],
        irqs: &[("i2c1_irq", 3)],
        sets_time: true,
    }),
    // USB2 (usb-bus.1): the usb server enumerates what QEMU plugs into it
    // (the same ChipIdea core as the Toon's).
    usb: Some(Ehci {
        page: 0x53ff_4000,
        args: "ehci 0x400",
        irq: 35,
    }),
    wifi: Wifi::None,
    fec: Some((0x5003_8000, 57)),
    boiler: None,
    zwave: None,
    backlight: None,
    // GPT1, a 32-bit counter (the kernel's clock).
    counter: (0x53f9_0000, "mmio 0x24 32"),
    vm_test: "vm 16",
};

/// The Eneco Toon v1 (docs/hardware-toon.md).
pub static TOON: Board = Board {
    name: imx_boot::TOON.name,
    description: "Eneco Toon v1",
    // The NFC: touchos.toml in the NAND's config ring, the system copies.
    nand: Some(0xd800_0000),
    platform: Some(Platform {
        // The GPIO module: the touch controller's pen-down line (PF20).
        mmio: &[("i2c1", 0x1001_2000), ("i2c2", 0x1001_d000), ("gpio", 0x1001_5000)],
        irqs: &[("i2c1_irq", 12), ("i2c2_irq", 1), ("gpio_irq", 8)],
        sets_time: true,
    }),
    // The external port: the OTG controller as a host (the kernel set up
    // its port; the PHY switches VBUS). Host 2 in the same block is the
    // Wi-Fi server's.
    usb: Some(Ehci {
        page: 0x1002_4000,
        args: "ehci 0x0 ulpi vbus",
        irq: 56,
    }),
    // USB host 2 (the kernel set up its port): the RT5370 Wi-Fi module.
    wifi: Wifi::Rt5370 {
        ehci: Ehci {
            page: 0x1002_4000,
            args: "ehci 0x400 ulpi",
            irq: 55,
        },
        firmware_file: "rt3071.bin",
    },
    fec: Some((0x1002_b000, 50)),
    boiler: Some((0x1000_b000, 19)),
    zwave: Some((0x1000_c000, 18)),
    backlight: Some(0x1000_6000), // PWM1
    // GPT1, a 32-bit counter (the kernel's clock).
    counter: (0x1000_3000, "mmio 0x10 32"),
    vm_test: "vm 16",
};

/// The board we run on: the kernel names it (`board=`, built in), so
/// anything else is a kernel and init that don't belong together.
pub fn detect() -> &'static Board {
    match touchos_rt::arg("board") {
        Some(n) if n == IMX25.name => &IMX25,
        Some(n) if n == TOON.name => &TOON,
        other => panic!("unknown board {other:?} (the kernel names toon or imx25)"),
    }
}

/// Interrupt line `line` (`what`, for the log), from the "irq_root"
/// capability.
pub fn irq(what: &str, line: u32) -> Option<u32> {
    match sys::irq_claim(cap("irq_root")?, line) {
        Ok(c) => Some(c),
        Err(touchos_rt::abi::Error::NoSys) => None,
        Err(e) => {
            error!("no interrupt for {what} (line {line}): {e:?}");
            None
        }
    }
}

/// One page of device registers (`what`, for the log), from the
/// "mmio_root" capability.
pub fn regs(what: &str, phys: u64) -> Option<u32> {
    sys::mmio_derive(cap("mmio_root")?, phys, 0x1000)
        .inspect_err(|e| error!("no MMIO capability for {what}: {e:?}"))
        .ok()
}
