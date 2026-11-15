//! The i.MX27's clock controller (CCM): only its clock gates. U-Boot and
//! the stock kernel leave a block's clocks off while nobody uses it, and
//! touching a block whose clock is gated hangs the bus: whoever drives a
//! block ungates it here first. (The i.MX25's CCM is a different design;
//! only i.MX27 code calls this.)

use super::rmw;

const BASE: u64 = 0x1002_7000;
const PCCR0: usize = 0x20;
const PCCR1: usize = 0x24;

/// A clock gate: (register, bit).
pub struct Gate(usize, u32);

/// SAHARA2's register clock (ipg) and its DMA clock (ahb).
pub const SAHARA_IPG: Gate = Gate(PCCR0, 7);
pub const SAHARA_AHB: Gate = Gate(PCCR1, 13);
/// The USB block (`usb`) and its bus clock (`usb_ahb`).
#[cfg(feature = "board-toon")]
pub const USB: Gate = Gate(PCCR1, 25);
#[cfg(feature = "board-toon")]
pub const USB_AHB: Gate = Gate(PCCR1, 11);

/// UART2's register clock (ipg): the boiler link (servers/boiler). Its
/// baud clock (per1) is on for UART1 already. Off under U-Boot: the rate
/// registers take no writes until it is on.
#[cfg(feature = "board-toon")]
pub const UART2_IPG: Gate = Gate(PCCR1, 30);
/// UART3's register clock (ipg): the Z-Wave controller (servers/zwave).
#[cfg(feature = "board-toon")]
pub const UART3_IPG: Gate = Gate(PCCR1, 29);

/// Switch the clocks behind `gates` on (in this order).
pub fn ungate(gates: &[Gate]) {
    let ccm = crate::mem::map_mmio(BASE, 0x100);
    for g in gates {
        rmw(ccm + g.0, 0, 1 << g.1);
    }
}
