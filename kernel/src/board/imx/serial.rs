//! The i.MX UART1 (the Toon's console on its JTAG header; QEMU's serial0).
//!
//! The boot page table maps its MiB uncached at 0xfe000000. On the Toon,
//! U-Boot set it up (115200 8N1); we only make sure the UART and its
//! transmitter are on, and keep the baud rate.

use core::fmt;

use super::soc;

use crate::arch::boot::EARLY_UART_VIRT as EARLY_VA;

const UTXD: usize = 0x40;
const UCR1: usize = 0x80;
const UCR2: usize = 0x84;
const UTS: usize = 0xb4;
const UTS_TXFULL: u32 = 1 << 4;

fn at(off: usize) -> *mut u32 {
    ((EARLY_VA + (soc::UART1 & 0xf_ffff)) as usize + off) as *mut u32
}

pub struct Serial {}

impl Serial {
    pub const fn new() -> Self {
        Serial {}
    }

    pub fn init(&mut self) {
        unsafe {
            at(UCR1).write_volatile(at(UCR1).read_volatile() | 1); // UARTEN
            // SRST (not in reset), RXEN, TXEN, 8 bits, ignore RTS.
            at(UCR2).write_volatile(at(UCR2).read_volatile() | 0x4027);
        }
    }

    fn put(&mut self, b: u8) {
        unsafe {
            while at(UTS).read_volatile() & UTS_TXFULL != 0 {}
            at(UTXD).write_volatile(b as u32);
        }
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.put(b'\r');
            }
            self.put(b);
        }
        Ok(())
    }
}
