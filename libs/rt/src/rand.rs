//! Random bytes, from the kernel (`RANDOM`), which owns the entropy
//! sources.

pub fn fill(out: &mut [u8]) {
    for chunk in out.chunks_mut(256) {
        crate::sys::random(chunk).expect("no entropy from the kernel");
    }
}

pub fn u64() -> u64 {
    let mut b = [0u8; 8];
    fill(&mut b);
    u64::from_le_bytes(b)
}
