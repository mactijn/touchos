//! The Eneco Toon v1's own bring-up: what U-Boot leaves undone on this
//! board (not on every i.MX27) for touchos's drivers: the Ethernet and I²C
//! pins, a stuck I²C bus, the USB host port of the Wi-Fi module, and the
//! external USB port.

use super::gpio::{self, Port, port};
use super::{ccm, rmw};

/// All of it, in this order (`board_setup`).
pub fn setup() {
    setup_pins();
    setup_usb_h2();
    setup_usb_otg();
    // The boiler link's UART (receive only; its pins are U-Boot's).
    ccm::ungate(&[ccm::UART2_IPG]);
    setup_zwave_uart();
}

/// The Z-Wave controller (a ZW0301 with the Serial API; servers/zwave):
/// UART3's clock, its TXD/RXD pins (PE8/PE9) on their primary function,
/// and the chip let out of reset. U-Boot leaves PE8/PE9 GPIOs, and PF14
/// (the chip's RESET_N) on its primary function, reading low: the chip
/// is held in reset and the UART goes nowhere.
///
/// From the stock kernel's board file: the `zwave300` SPI device's
/// platform data is PF14 (RESET_N), PD28, PD29, PD31, PF11. Only the
/// flasher (zwave300.ko) uses the other four; RESET_N high is how stock
/// Linux runs the chip. Here PF14 becomes a GPIO output, low for 20 ms
/// (a clean reset), then high.
fn setup_zwave_uart() {
    use gpio::{DDIR, DR, GIUS, GPR, OCR_DR};
    ccm::ungate(&[ccm::UART3_IPG]);
    let e = port('E');
    const UART_PINS: u32 = 0b11 << 8;
    e.rmw(GPR, UART_PINS, 0);
    e.rmw(GIUS, UART_PINS, 0);

    let f = port('F');
    const RESET_N: u32 = 1 << 14;
    f.rmw(DR, RESET_N, 0);
    f.set_ocr(RESET_N, OCR_DR);
    f.rmw(DDIR, 0, RESET_N);
    f.rmw(GIUS, 0, RESET_N);
    crate::time::busy_wait_us(20_000);
    f.rmw(DR, 0, RESET_N);
    ::log::info!("zwave: UART3 on PE8/PE9, the controller out of reset (PF14 high)");
}

/// Pin set-up U-Boot leaves undone for Ethernet and I²C: the Toon's FEC pins
/// (PD0-PD16, PF23) are still on their primary functions, the I²C pins are
/// GPIOs, and the PHY is held in reset. Linux's values for these pins (read
/// from the stock system over JTAG), masked to them: the FEC
/// signals through the GPIO module's alternate paths, MDIO (PD8) on its
/// alternate function, no pull-ups. QEMU has no pins, so only the real board shows a mistake here
/// (TX_EN on PF23 was missing at first: frames went "out", none on the wire).
fn setup_pins() {
    use gpio::*;
    // (register, value, mask of the bits that are ours), in this order.
    fn apply(port: Port, settings: &[(usize, u32, u32)]) {
        for &(off, val, mask) in settings {
            port.rmw(off, mask, val & mask);
        }
    }
    let (a, c, d, f) = (port('A'), port('C'), port('D'), port('F'));
    // First, while the I²C pins are still GPIOs: free a stuck bus.
    recover_i2c("i2c1", d, 17, 18);
    recover_i2c("i2c2", c, 5, 6);
    // PD0-PD16: TXD0-3, RX_ER, RXD1-3, MDIO, MDC, CRS, TX_CLK, RXD0, RX_DV,
    // RX_CLK, COL, TX_ER; PD17: I2C1's data (primary function).
    apply(
        d,
        &[
            (DDIR, 0xba07_020f, 0x0003_ffff),
            (ICONFA1, 0x0000_0000, 0xffff_ffff), // pins 0-15, 2 bits each
            (ICONFA2, 0x0000_0000, 0x0000_000f), // pins 16-17
            (ICONFB1, 0x0000_0000, 0xffff_ffff),
            (ICONFB2, 0x0000_0000, 0x0000_000f),
            (GPR, 0x05f8_0100, 0x0003_ffff), // PD8 alternate (MDIO)
            (PUEN, 0x0000_0000, 0x0003_ffff),
            (GIUS, 0x1a01_feff, 0x0003_ffff), // last: switch the pins over
        ],
    );
    // PF23: TX_EN, a GPIO output driven from the FEC (AIN).
    apply(
        f,
        &[
            (OCR2, 0, 0b11 << 14), // pin 23 from AIN
            (GPR, 0, 1 << 23),
            (PUEN, 0, 1 << 23),
            (DDIR, 1 << 23, 1 << 23), // output
            (GIUS, 1 << 23, 1 << 23),
        ],
    );
    // The PHY's reset (active low) is PA25: a GPIO output, high. (Found by
    // pulling candidate pins low under the stock system: only PA25 silences
    // the PHY.)
    apply(
        a,
        &[
            (DR, 1 << 25, 1 << 25),         // high
            (OCR2, 0b11 << 18, 0b11 << 18), // pin 25 from DR
            (DDIR, 1 << 25, 1 << 25),       // output
            (GIUS, 1 << 25, 1 << 25),       // GPIO
        ],
    );
    // I²C: I2C1's clock PD18 (its data, PD17, is set above) and I2C2's
    // PC5/PC6 on their primary function (GIUS and GPR clear), as under Linux.
    // U-Boot leaves them as GPIOs.
    for (port, mask) in [(d, 1u32 << 18), (c, 0b11 << 5)] {
        apply(port, &[(GPR, 0, mask), (GIUS, 0, mask)]);
    }
}

/// I²C bus recovery (as Linux's): a device reset in the middle of a
/// transfer can keep holding SDA low, and the bus then stays stuck for the
/// controller. A JTAG reset or a crash doesn't power the devices off, so
/// this does happen (seen on the Toon's i2c2 after a reset while touch was
/// polled). Clock SCL until the device lets go of SDA (9 clocks at most),
/// then a STOP. The pins (`sda`, `scl` of `port`) must still be GPIOs, as
/// U-Boot leaves them. Open drain is emulated: released = input (the bus's
/// pull-up), low = output 0. The delays are counted register reads, not
/// the clock (I²C has no minimum clock rate).
fn recover_i2c(name: &str, port: Port, sda: u32, scl: u32) {
    use gpio::{DDIR, DR, GIUS, OCR_DR, SSR};
    let set = |off: usize, mask: u32, on: bool| port.rmw(off, mask, if on { mask } else { 0 });
    let (sda_m, scl_m) = (1u32 << sda, 1u32 << scl);
    let level = |m: u32| port.read(SSR) & m != 0;
    // About 5 µs or more (a peripheral read takes some 100 ns): reads, so
    // the compiler can't drop the loop (spin_loop is empty on ARMv5).
    let delay = || {
        for _ in 0..64 {
            let _ = level(0);
        }
    };
    // Release a line and wait (boundedly) until the pull-up has it high.
    let release = |m: u32| {
        set(DDIR, m, false);
        for _ in 0..256 {
            if level(m) {
                break;
            }
        }
        delay();
    };
    // Both released: GPIO inputs; when driven, from DR = 0.
    set(DDIR, sda_m | scl_m, false);
    set(DR, sda_m | scl_m, false);
    port.set_ocr(sda_m | scl_m, OCR_DR);
    set(GIUS, sda_m | scl_m, true);
    delay();
    if !level(sda_m) {
        let mut clocks = 0;
        while clocks < 9 && !level(sda_m) {
            set(DDIR, scl_m, true); // SCL low
            delay();
            release(scl_m);
            clocks += 1;
        }
        // A STOP (SDA rising while SCL is high), so the device starts over.
        // SDA goes low while SCL is low: low with SCL high would be a START.
        set(DDIR, scl_m, true);
        delay();
        set(DDIR, sda_m, true);
        delay();
        release(scl_m);
        release(sda_m);
        if level(sda_m) {
            ::log::warn!("{name}: the bus was stuck (SDA low); free after {clocks} clocks");
        } else {
            ::log::error!("{name}: the bus is stuck (SDA stays low after {clocks} clocks)");
        }
    }
    // Leave both outputs at 1: the pin setup that follows may make them
    // outputs before it hands them to the controller, and a 0 there would
    // put a START (and a STOP) on the bus.
    set(DR, sda_m | scl_m, true);
}

/// The Toon's USB host 2 (the RT5370 Wi-Fi module, through a ULPI PHY),
/// brought up the way the stock kernel does it (Prodrive's ehci-mxc probe,
/// disassembled; docs/hardware-toon.md has the golden values). U-Boot leaves
/// the USB clocks off: touching the USB block before this hangs the bus.
/// The wifi server (its EHCI driver) then resets the controller and runs
/// the port.
fn setup_usb_h2() {
    use gpio::{DDIR, DR, GIUS, GPR, OCR_DR, PUEN};
    const USB: u64 = 0x1002_4000;
    const H2: usize = 0x400;
    const USBMODE: usize = H2 + 0x1a8;
    const PORTSC1: usize = H2 + 0x184;
    const USB_CTRL: usize = 0x600;
    let (a, b, d, f) = (port('A'), port('B'), port('D'), port('F'));

    // 1. Clocks. The 60 MHz USB clock (SPLL / CSCR.USB_DIV) is already set
    //    up by U-Boot.
    ccm::ungate(&[ccm::USB, ccm::USB_AHB]);

    // 2. The module's and PHY's enables: PB25 (USB1_RESEST_N) and PF13
    //    (WLAN_DIS_N), GPIO outputs, high.
    for (port, pin) in [(b, 25u32), (f, 13)] {
        port.rmw(DR, 0, 1 << pin);
        port.set_ocr(1 << pin, OCR_DR);
        port.rmw(DDIR, 0, 1 << pin);
        port.rmw(PUEN, 1 << pin, 0);
        port.rmw(GIUS, 0, 1 << pin);
    }

    // 3. The ULPI pins held as GPIOs until the controller speaks ULPI:
    //    data (PA2, PD19-PD24, PD26) low, STP (PA4) high, CLK/DIR/NXT
    //    (PA0, PA1, PA3) inputs.
    const A_ULPI: u32 = 0b1_1111; // PA0-PA4
    const A_OUT: u32 = 1 << 2 | 1 << 4;
    const D_ULPI: u32 = 0x7f << 19 & !(1 << 25) | 1 << 26; // PD19-24, PD26
    a.rmw(DR, 1 << 2, 1 << 4);
    d.rmw(DR, D_ULPI, 0);
    a.set_ocr(A_OUT, OCR_DR);
    d.set_ocr(D_ULPI, OCR_DR);
    a.rmw(DDIR, A_ULPI & !A_OUT, A_OUT);
    d.rmw(DDIR, 0, D_ULPI);
    a.rmw(PUEN, A_ULPI, 0);
    d.rmw(PUEN, D_ULPI, 0);
    a.rmw(GIUS, 0, A_ULPI);
    d.rmw(GIUS, 0, D_ULPI);

    // 4. Host mode, PTS = ULPI, then 10 ms (Ehci::init in libs/usb redoes both after its reset).
    let usb = crate::mem::map_mmio(USB, 0x1000);
    rmw(usb + USBMODE, 0, 3);
    rmw(usb + PORTSC1, 0b11 << 30 | 0b10_1010, 0b10 << 30); // (W1C bits written 0)
    crate::time::busy_wait_us(10_000);

    // 5. The pins to the controller: PA0-PA4 primary function, the port D
    //    data pins alternate (GPR); 1 ms.
    a.rmw(GPR, A_ULPI, 0);
    a.rmw(GIUS, A_ULPI, 0);
    d.rmw(GPR, 0, D_ULPI);
    d.rmw(GIUS, D_ULPI, 0);
    // As Linux leaves them: direction in, OCR 0 (the peripheral drives them).
    for (port, pins) in [(a, A_ULPI), (d, D_ULPI)] {
        port.rmw(DDIR, pins, 0);
        port.set_ocr(pins, 0);
    }
    crate::time::busy_wait_us(1000);

    // 6. USB_CTRL (shared with the OTG and H1 ports): H2's serial interface
    //    (bits 22:21) = 0, power mask (16) and DT (5) set.
    rmw(usb + USB_CTRL, 0b11 << 21, 1 << 16 | 1 << 5);
    ::log::info!("usb: host 2 set up (ULPI), PORTSC1 {:#010x}", unsafe { ((usb + PORTSC1) as *const u32).read_volatile() });
}

/// The Toon's external USB port: the OTG controller (offset 0 in the USB
/// block) as a host, through a second SMSC USB3370 ULPI PHY, set up as the
/// stock kernel leaves it (read over JTAG under stock Linux with a USB
/// disk in the port; docs/hardware-toon.md). The same steps as
/// for host 2 (`setup_usb_h2`, after it: the USB clocks are on), with the
/// OTG pins: NXT PE0, STP PE1, DIR PE2, CLK PE24, DATA7 PE25 and DATA0-6
/// on PC7-PC13, all on their primary function. PE18 is a GPIO output,
/// high, under stock (U-Boot leaves it an input); it is taken to be the
/// PHY's enable, as PB25 is host 2's. VBUS is switched by the PHY, which
/// the usb server's EHCI driver turns on ("vbus").
fn setup_usb_otg() {
    use gpio::{DDIR, DR, GIUS, GPR, OCR_DR, PUEN};
    const USB: u64 = 0x1002_4000;
    const USBMODE: usize = 0x1a8;
    const PORTSC1: usize = 0x184;
    const USB_CTRL: usize = 0x600;
    let (c, e) = (port('C'), port('E'));

    // 1. PE18: a GPIO output, high.
    e.rmw(DR, 0, 1 << 18);
    e.set_ocr(1 << 18, OCR_DR);
    e.rmw(DDIR, 0, 1 << 18);
    e.rmw(PUEN, 1 << 18, 0);
    e.rmw(GIUS, 0, 1 << 18);

    // 2. The ULPI pins held as GPIOs until the controller speaks ULPI: data
    //    (PC7-PC13, PE25) low, STP (PE1) high, CLK/DIR/NXT inputs.
    const C_ULPI: u32 = 0x7f << 7; // PC7-PC13
    const E_ULPI: u32 = 0b111 | 0b11 << 24; // PE0-PE2, PE24-PE25
    const E_OUT: u32 = 1 << 1 | 1 << 25;
    c.rmw(DR, C_ULPI, 0);
    e.rmw(DR, 1 << 25, 1 << 1);
    c.set_ocr(C_ULPI, OCR_DR);
    e.set_ocr(E_OUT, OCR_DR);
    c.rmw(DDIR, 0, C_ULPI);
    e.rmw(DDIR, E_ULPI & !E_OUT, E_OUT);
    c.rmw(PUEN, C_ULPI, 0);
    e.rmw(PUEN, E_ULPI, 0);
    c.rmw(GIUS, 0, C_ULPI);
    e.rmw(GIUS, 0, E_ULPI);

    // 3. Host mode, PTS = ULPI, then 10 ms.
    let usb = crate::mem::map_mmio(USB, 0x1000);
    rmw(usb + USBMODE, 0, 3);
    rmw(usb + PORTSC1, 0b11 << 30 | 0b10_1010, 0b10 << 30); // (W1C bits written 0)
    crate::time::busy_wait_us(10_000);

    // 4. The pins to the controller (primary function: GIUS and GPR clear),
    //    direction in, OCR 0; 1 ms.
    for (port, pins) in [(c, C_ULPI), (e, E_ULPI)] {
        port.rmw(GPR, pins, 0);
        port.rmw(GIUS, pins, 0);
        port.rmw(DDIR, pins, 0);
        port.set_ocr(pins, 0);
    }
    crate::time::busy_wait_us(1000);

    // 5. USB_CTRL's OTG fields as under stock: serial interface (bits 30:29)
    //    0, power mask (24) clear.
    rmw(usb + USB_CTRL, 0b11 << 29 | 1 << 24, 0);
    ::log::info!("usb: OTG port set up (ULPI), PORTSC1 {:#010x}", unsafe { ((usb + PORTSC1) as *const u32).read_volatile() });
}
