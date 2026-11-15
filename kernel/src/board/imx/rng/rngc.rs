//! The i.MX25's RNGC (QEMU models it): used once at boot, to seed the pool.

const COMMAND: usize = 0x04;
const STATUS: usize = 0x0c;
const FIFO: usize = 0x14;

const COMMAND_SEED: u32 = 1 << 1;

/// Seed the generator, then take 32 words from its FIFO into the pool
/// (each after waiting, boundedly, for the FIFO to fill).
pub fn seed(base: u64) {
    let b = crate::mem::map_mmio(base, 0x100);
    let r = |off: usize| (b + off) as *mut u32;
    unsafe {
        r(COMMAND).write_volatile(COMMAND_SEED);
        for _ in 0..32 {
            for _ in 0..100_000 {
                if (r(STATUS).read_volatile() >> 8) & 0xf != 0 {
                    break; // the FIFO level
                }
            }
            let w = r(FIFO).read_volatile();
            crate::entropy::add(&w.to_le_bytes());
        }
    }
}
