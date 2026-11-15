//! The TSC2007's pen-down line (PENIRQ, active low) on GPIO PF20: Linux's
//! IRQ 244 is GPIO interrupt 64 + 5 × 32 + 20. With it, the touch driver
//! only samples while a finger is down; the GPIO module's interrupt (all
//! ports share AITC line 8) tells it when one lands.
//!
//! Port F's registers in the GPIO page (0x10015000, ports 0x100 apart).

use touchos_drv::mmio::Regs;
use touchos_rt::irq::Irq;
use touchos_rt::time;


const PORT_F: usize = 0x500;
const DDIR: usize = PORT_F + 0x00;
const GIUS: usize = PORT_F + 0x20;
const SSR: usize = PORT_F + 0x24;
/// Interrupt configuration for pins 16-31, 2 bits each.
const ICR2: usize = PORT_F + 0x2c;
const IMR: usize = PORT_F + 0x30;
const ISR: usize = PORT_F + 0x34;

const PIN: u32 = 20;
const BIT: u32 = 1 << PIN;
/// ICR value: interrupt while the pin is low (a level: a finger that is
/// already down when we start waiting still counts).
const LOW_LEVEL: u32 = 0b11;

pub struct PenDown {
    regs: Regs,
    irq: Irq,
}

impl PenDown {
    /// From the "gpio" and "gpio_irq" capabilities; None without them.
    pub fn new() -> Option<PenDown> {
        let regs = Regs::from_cap(touchos_rt::cap("gpio")?, 0, 0x1000).ok()?;
        let irq = Irq::take("gpio_irq")?;
        // A GPIO input (as under the stock firmware), masked for now.
        regs.w32(IMR, regs.r32(IMR) & !BIT);
        regs.w32(DDIR, regs.r32(DDIR) & !BIT);
        regs.w32(GIUS, regs.r32(GIUS) | BIT);
        let shift = (PIN - 16) * 2;
        regs.w32(ICR2, regs.r32(ICR2) & !(0b11 << shift) | LOW_LEVEL << shift);
        regs.w32(ISR, BIT);
        Some(PenDown { regs, irq })
    }

    /// Is the line low (a finger on the panel, with PENIRQ enabled)?
    pub fn down(&self) -> bool {
        self.regs.r32(SSR) & BIT == 0
    }

    /// Wait until a finger lands, or `ms` pass; true if it did. The
    /// TSC2007 must be powered down with PENIRQ on (its POWER_DOWN
    /// command), or the line doesn't move.
    ///
    /// After every measurement the line dips low for a moment with no
    /// finger on the panel (on the Toon: 757 times a minute while every
    /// such wake-up took a sample; high again by the time we looked). A finger holds it low for tens of
    /// milliseconds, so a wake-up that finds it high was such a dip: wait
    /// on, without sampling (which would only cause the next dip).
    pub async fn wait(&self, ms: u64) -> bool {
        let end = time::now_ms() + ms;
        loop {
            self.regs.w32(ISR, BIT);
            self.regs.w32(IMR, self.regs.r32(IMR) | BIT);
            let left = end.saturating_sub(time::now_ms());
            let fired = left > 0 && touchos_rt::exec::wait_bits_or_ms(self.irq.bit(), left).await != 0;
            // Masked again until the next wait: while the finger stays
            // down, the level would fire again and again.
            self.regs.w32(IMR, self.regs.r32(IMR) & !BIT);
            self.regs.w32(ISR, BIT);
            if !fired {
                return false;
            }
            self.irq.ack();
            if self.down() {
                return true;
            }
        }
    }
}
