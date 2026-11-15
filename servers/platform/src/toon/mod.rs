//! The Toon's I²C devices (docs/hardware-toon.md):
//! - bus 1 ("i2c1"): two ADT7410 temperature sensors (0x48, 0x49);
//! - bus 2 ("i2c2"): the TSC2007 touch controller (0x48) and the ISL1208
//!   real-time clock (0x6f).
//!
//! The RTC sets the wall clock at start (NTP corrects it later; the RTC is
//! only read). Touches go to the dashboard through the input ring. The
//! temperatures are logged.
//!
//! QEMU's imx25-pdk runs this too, as the test bench for the I²C driver:
//! it has the same controller, as "i2c1" (devices only from QEMU_EXTRA).

mod imx_i2c;
mod pen;

use ::log::{info, warn};
use touchos_proto::input::Event;
use touchos_rt::{cap, exec, sys, time};

use imx_i2c::I2c;

/// Bus clock divider: 640 (about 100 kHz from the 66.5 MHz IPG clock).
const IFDR: u16 = 0x38;

const ADT7410: [u8; 2] = [0x48, 0x49];
const TSC2007: u8 = 0x48;
const ISL1208: u8 = 0x6f;

fn bus(name: &str) -> Option<&'static I2c> {
    let i2c = I2c::new(cap(name)?, IFDR)?;
    // How fast is the bus? An unanswered address byte still clocks 9 bits.
    let t0 = time::now_us();
    let r = i2c.xfer(0x7f, &[], &mut []);
    info!("{name}: {} µs for an address byte ({r:?})", time::now_us() - t0);
    // Who answers? (An address byte alone: no register changes.)
    let found: alloc::vec::Vec<u8> = (0x08..0x78).filter(|&a| i2c.xfer(a, &[], &mut []).is_ok()).collect();
    info!("{name}: devices at {found:02x?}");
    Some(alloc::boxed::Box::leak(alloc::boxed::Box::new(i2c)))
}

pub fn start() {
    if let Some(b1) = bus("i2c1") {
        exec::spawn(temperatures(b1));
    }
    if let Some(b2) = bus("i2c2") {
        rtc(b2);
        exec::spawn(touch(b2));
    }
}

/// Hundredths of a degree as "21.06".
fn centi(c: i32) -> alloc::string::String {
    let sign = if c < 0 { "-" } else { "" };
    alloc::format!("{sign}{}.{:02}", c.abs() / 100, c.abs() % 100)
}

async fn temperatures(i2c: &'static I2c) {
    // ADT7410: ID register 0x0b reads 0b11001xxx.
    let found: alloc::vec::Vec<u8> = ADT7410
        .into_iter()
        .filter(|&a| {
            let mut id = [0u8];
            match i2c.xfer_probe(a, &[0x0b], &mut id) {
                Ok(()) if id[0] >> 3 == 0x19 => true,
                Ok(()) => {
                    warn!("temperature: device at {a:#x} has ID {:#04x}, not an ADT7410", id[0]);
                    false
                }
                Err(_) => false,
            }
        })
        .collect();
    if found.is_empty() {
        info!("temperature: no ADT7410");
        return;
    }
    loop {
        let mut line = alloc::string::String::new();
        for &a in &found {
            // Temperature (0x00): 13 bits, 1/16 °C, left-aligned in 16.
            let mut b = [0u8; 2];
            let v = match i2c.xfer(a, &[0x00], &mut b) {
                Ok(()) => centi((i16::from_be_bytes(b) >> 3) as i32 * 100 / 16),
                Err(e) => alloc::format!("{e:?}"),
            };
            line += &alloc::format!(" {a:#x}: {v} °C");
        }
        info!("temperature:{line}");
        exec::sleep_ms(300_000).await;
    }
}

/// Read the ISL1208 and set the clock from it (taken to be UTC).
fn rtc(i2c: &I2c) {
    // SC MN HR DT MO YR DW SR, BCD.
    let mut r = [0u8; 8];
    if let Err(e) = i2c.xfer_probe(ISL1208, &[0x00], &mut r) {
        info!("rtc: no ISL1208 ({e:?})");
        return;
    }
    let bcd = |b: u8| (b >> 4) as u32 * 10 + (b & 0xf) as u32;
    let hour = if r[2] & 0x80 != 0 {
        bcd(r[2] & 0x3f) // 24-hour mode
    } else {
        bcd(r[2] & 0x1f) % 12 + if r[2] & 0x20 != 0 { 12 } else { 0 }
    };
    let (y, mo, d, mi, s) = (2000 + bcd(r[5]) as i32, bcd(r[4] & 0x1f), bcd(r[3] & 0x3f), bcd(r[1] & 0x7f), bcd(r[0] & 0x7f));
    let when = alloc::format!("{y}-{mo:02}-{d:02} {hour:02}:{mi:02}:{s:02}");
    // SR bit 0 (RTCF): the clock lost power and doesn't know the time.
    if r[7] & 0x01 != 0 || y < 2024 || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        warn!("rtc: {when}, status {:#04x}: not set", r[7]);
        return;
    }
    let unix = touchos_civil::unix_from_civil(y, mo, d, hour, mi, s);
    match cap("system").map(|c| sys::time_set(c, unix as u64)) {
        Some(Ok(())) => info!("rtc: {when} UTC: clock set"),
        _ => warn!("rtc: {when} UTC: can't set the clock"),
    }
}

/// TSC2007 commands: measure (bits 7-4), ADC on and pen IRQ off while
/// measuring (bit 2), 12 bits.
const READ_X: u8 = 0xc4;
const READ_Y: u8 = 0xd4;
const READ_Z1: u8 = 0xe4;
const READ_Z2: u8 = 0xf4;
/// Power down between conversions, pen IRQ on.
const POWER_DOWN: u8 = 0x00;
/// With the pen-down line: how often a sample checks that it works.
const PEN_CHECK_MS: u64 = 5000;

fn measure(i2c: &I2c, cmd: u8) -> Option<u32> {
    let mut b = [0u8; 2];
    i2c.xfer(TSC2007, &[cmd], &mut b).ok()?;
    Some((u16::from_be_bytes(b) >> 4) as u32)
}

/// A touch: raw (x, y), or None when nothing presses.
fn sample(i2c: &I2c) -> Option<Option<(u32, u32)>> {
    let y = measure(i2c, READ_Y)?;
    let x = measure(i2c, READ_X)?;
    let z1 = measure(i2c, READ_Z1)?;
    let z2 = measure(i2c, READ_Z2)?;
    let _ = i2c.xfer(TSC2007, &[POWER_DOWN], &mut []);
    // Touch resistance (as Linux's tsc2007, with a 400 ohm X plate): no
    // touch when z1 is 0 or the resistance is out of range.
    if z1 == 0 || z2 <= z1 {
        return Some(None);
    }
    // In u64: (z2 - z1) * x * 400 overflows u32 for a light touch (small z1).
    let rt = ((z2 - z1) as u64 * x as u64 * 400 / z1 as u64 + 2047) >> 12;
    Some((rt < 4096).then_some((x, y)))
}

/// Sample the touch controller every 20 ms while a finger is down, and
/// report touches in screen pixels, mapped with the Toon panel's
/// calibration. With nothing on the panel it waits for the pen-down line
/// (`pen`), checking with a sample every `PEN_CHECK_MS` that the line works.
async fn touch(i2c: &'static I2c) {
    // Present? (A Nack ends it right away; a timeout gets two more tries.)
    if let Err(e) = i2c.xfer_probe(TSC2007, &[POWER_DOWN], &mut []) {
        info!("touch: no TSC2007 ({e:?})");
        return;
    }
    let Some(first) = (0..3).find_map(|_| sample(i2c)) else {
        info!("touch: no TSC2007");
        return;
    };
    info!("touch: TSC2007 found ({})", if first.is_some() { "pressed" } else { "idle" });
    let fb = touchos_rt::time::kinfo().fb;
    let (w, h) = (fb.width.max(1) as i32, fb.height.max(1) as i32);
    // The Toon's panel, calibrated by tapping its corners (the average
    // of 5 to 8 taps per corner): x runs from RAW_LEFT to
    // RAW_RIGHT, y from RAW_TOP down to RAW_BOTTOM.
    const RAW_LEFT: i32 = 183;
    const RAW_RIGHT: i32 = 3935;
    const RAW_TOP: i32 = 3745;
    const RAW_BOTTOM: i32 = 322;
    let to_screen = |(x, y): (u32, u32)| {
        let sx = (x as i32 - RAW_LEFT) * (w - 1) / (RAW_RIGHT - RAW_LEFT);
        let sy = (RAW_TOP - y as i32) * (h - 1) / (RAW_TOP - RAW_BOTTOM);
        (sx.clamp(0, w - 1), sy.clamp(0, h - 1))
    };
    // Where the finger is, and for how many samples it seemed gone: contact
    // bounces, so it only counts as lifted after RELEASE samples (60 ms).
    const RELEASE: u32 = 3;
    let mut last: Option<(i32, i32)> = None;
    let mut gone = 0;
    let mut pen = pen::PenDown::new();
    match &pen {
        Some(p) => info!("touch: waits for the pen-down line (PF20, now {})", if p.down() { "low" } else { "high" }),
        None => info!("touch: polled every 20 ms"),
    }
    loop {
        // Waiting for a finger: the line, or (as a check) a sample now and then.
        let check = match (&pen, last) {
            (Some(p), None) => !p.wait(PEN_CHECK_MS).await,
            _ => {
                exec::sleep_ms(20).await;
                false
            }
        };
        let Some(s) = sample(i2c) else { continue };
        if check {
            match s {
                Some((x, y)) if x.min(y) > 0 && x.max(y) < 4095 => {
                    warn!("touch: a touch the pen-down line didn't report; polling from now on");
                    pen = None;
                }
                _ => continue,
            }
        }
        // A finger landing or lifting can give a saturated reading (0 or
        // 4095): skip it rather than tap at the screen's edge.
        if s.is_some_and(|(x, y)| x.min(y) == 0 || x.max(y) >= 4095) {
            continue;
        }
        match s {
            Some(raw) => {
                let (x, y) = to_screen(raw);
                gone = 0;
                match last {
                    None => {
                        ::log::debug!("touch: down at raw {raw:?} -> ({x}, {y})");
                        crate::input::push(Event::TouchDown { x, y })
                    }
                    Some(p) if p != (x, y) => crate::input::push(Event::TouchMove { x, y }),
                    _ => {}
                }
                last = Some((x, y));
            }
            None => {
                if let Some((x, y)) = last {
                    gone += 1;
                    if gone >= RELEASE {
                        crate::input::push(Event::TouchUp { x, y });
                        last = None;
                        gone = 0;
                    }
                }
            }
        }
    }
}
