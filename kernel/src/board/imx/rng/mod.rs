//! The chips' hardware random number generators: the i.MX25's RNGC, the
//! i.MX27's in its SAHARA2 crypto engine (`soc::RNGC`, `soc::SAHARA`; a
//! chip has one or the other). They feed the kernel's entropy pool, next to
//! the clock's jitter (`clock.rs`).

mod rngc;
mod sahara;

use alloc::string::String;

use super::soc;

/// At boot: seed the pool from the chip's RNG. Returns what was used, for
/// the clock's entropy log line.
pub fn seed() -> String {
    if let Some(base) = soc::RNGC {
        rngc::seed(base);
    }
    // The i.MX27's RNG: 256 bytes, and a sanity check.
    let mut sahara = None;
    if let Some(base) = soc::SAHARA
        && sahara::init(base)
    {
        let mut buf = [0u8; 256];
        if sahara::fill(&mut buf) {
            crate::entropy::add(&buf);
            let ones: u32 = buf.iter().map(|b| b.count_ones()).sum();
            sahara = Some(ones);
        }
    }
    match (soc::RNGC.is_some(), sahara) {
        (true, _) => String::from(", RNGC"),
        (_, Some(ones)) => alloc::format!(", SAHARA RNG: 256 bytes, {ones} of 2048 bits set"),
        _ => String::from("; weak: one crystal, no RNG"),
    }
}

/// Fresh hardware randomness into the pool, where the RNG can be asked
/// again after boot (SAHARA).
pub fn refresh() {
    let mut hw = [0u8; 32];
    if sahara::fill(&mut hw) {
        crate::entropy::add(&hw);
    }
}
