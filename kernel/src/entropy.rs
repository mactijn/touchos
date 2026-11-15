//! The kernel's entropy pool (the ARM926 has no random-number
//! instruction): noise samples go in (`add`), random
//! bytes come out (`fill`).
//!
//! The pool is a SHA-256 state: every sample is hashed into it. Output
//! blocks are SHA-256(pool, counter); after each request the pool itself
//! moves on (hashed with a marker), so earlier output can't be recovered
//! from a later pool.

use sha2::{Digest, Sha256};
use spin::Mutex;

struct Pool {
    pool: [u8; 32],
    counter: u64,
    samples: u64,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { pool: [0; 32], counter: 0, samples: 0 });

/// Mix noise into the pool.
pub fn add(data: &[u8]) {
    crate::arch::irq::without(|| {
        let mut p = POOL.lock();
        let mut h = Sha256::new();
        h.update(p.pool);
        h.update(data);
        p.pool = h.finalize().into();
        p.samples += 1;
    })
}

/// How many samples went in so far.
pub fn samples() -> u64 {
    POOL.lock().samples
}

/// Random bytes from the pool.
pub fn fill(out: &mut [u8]) {
    crate::arch::irq::without(|| {
        let mut p = POOL.lock();
        for chunk in out.chunks_mut(32) {
            let mut h = Sha256::new();
            h.update(p.pool);
            h.update(p.counter.to_le_bytes());
            h.update(b"out");
            p.counter += 1;
            let block: [u8; 32] = h.finalize().into();
            chunk.copy_from_slice(&block[..chunk.len()]);
        }
        let mut h = Sha256::new();
        h.update(p.pool);
        h.update(b"next");
        p.pool = h.finalize().into();
    })
}
