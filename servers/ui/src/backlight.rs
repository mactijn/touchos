//! Panel backlight: the Toon's i.MX PWM (PWM1, capability "backlight"),
//! set up by U-Boot (`backlight_brightness` in its environment, 50 %).

use touchos_drv::mmio::Regs;
use touchos_rt::cap;

const PWMCR: usize = 0x00;
const PWMSR: usize = 0x04;
const PWMSAR: usize = 0x0c;
const PWMPR: usize = 0x10;
const CR_EN: u32 = 1 << 0;
/// U-Boot's control value: enabled, clocked from ipg_highfreq.
const UBOOT_CR: u32 = 0x0002_0001;
/// U-Boot's period (PWMPR; a period is PWMPR + 2 clocks).
const UBOOT_PR: u32 = 0x5bf1;

pub struct Backlight {
    regs: Regs,
    period: u32,
}

impl Backlight {
    pub fn init() -> Option<Backlight> {
        let regs = Regs::from_cap(cap("backlight")?, 0, 0x1000).ok()?;
        let (cr, pr, sar) = (regs.r32(PWMCR), regs.r32(PWMPR), regs.r32(PWMSAR));
        // Not running (no U-Boot before us, e.g. after a reset over
        // JTAG): U-Boot's set-up.
        let period = if cr & CR_EN == 0 || pr == 0 {
            regs.w32(PWMCR, 0);
            regs.w32(PWMPR, UBOOT_PR);
            regs.w32(PWMCR, UBOOT_CR);
            UBOOT_PR + 2
        } else {
            pr + 2
        };
        ::log::info!(
            "backlight: PWM period {period}, firmware duty {sar} ({}%)",
            sar.min(period) * 100 / period
        );
        Some(Backlight { regs, period })
    }

    /// Brightness in 0.01 % steps (0..=10000).
    pub fn set_permyriad(&self, p: u32) {
        // PWMSAR feeds a 4-sample FIFO (PWMSR bits 2:0 = samples in it);
        // a full FIFO drops the write. One sample lasts one period.
        for _ in 0..1000 {
            if self.regs.r32(PWMSR) & 0b111 < 4 {
                break;
            }
        }
        self.regs.w32(PWMSAR, self.period * p.min(10_000) / 10_000);
    }
}
