//! A device's interrupt, for a driver: the "irq" capability init hands it
//! (an interrupt controller line). The kernel
//! masks the line when it fires and signals one of our notification bits;
//! the driver handles the device, then [`Irq::ack`]s to unmask it.

use crate::{exec, sys};

pub struct Irq {
    cap: u32,
    bit: usize,
}

impl Irq {
    /// The interrupt behind capability `name`, bound to a bit of ours (and
    /// unmasked). None if there is none (the driver then polls).
    pub fn take(name: &str) -> Option<Irq> {
        Irq::bind(crate::cap(name)?, name)
    }

    fn bind(cap: u32, what: &str) -> Option<Irq> {
        let bit = exec::alloc_bit();
        match sys::irq_bind(cap, bit) {
            Ok(()) => Some(Irq { cap, bit }),
            Err(e) => {
                ::log::warn!("{what}: can't bind the interrupt ({e:?}); polling");
                exec::free_bit(bit);
                None
            }
        }
    }

    /// Its notification bit (to wait for it together with others).
    pub fn bit(&self) -> usize {
        self.bit
    }

    /// Wait until the line fires (it is masked then; [`ack`](Self::ack)).
    pub async fn wait(&self) {
        exec::wait_bits(self.bit).await;
    }

    /// Did it fire (without waiting)?
    pub fn fired(&self) -> bool {
        exec::take_bits(self.bit) != 0
    }

    /// The device's status is cleared: unmask the line.
    pub fn ack(&self) {
        let _ = sys::irq_ack(self.cap);
    }
}
