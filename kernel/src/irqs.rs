//! Interrupts for drivers in user space. A driver holds an `Irq`
//! capability for one source and binds it to notification bits of its
//! own thread (`IRQ_BIND`); the kernel signals them when it fires.
//!
//! A source is an AITC/AVIC line; init claims it from its "irq_root"
//! (`IRQ_CLAIM`). Lines are level-triggered: masked when they fire,
//! unmasked by `IRQ_ACK` once the driver has cleared its device.
//!
//! The driver's loop is: clear the device's status, handle it,
//! `IRQ_ACK`, wait for the bit.

use alloc::sync::Arc;
use spin::Mutex;
use touchos_abi::Error;

use crate::arch::irq as interrupts;

/// Sources at once: the i.MX's 64 lines.
const SLOTS: usize = 64;

/// One claimed source (the object behind an `Irq` capability). Dropping
/// the last capability masks it and frees its slot. (Init keeps the
/// capabilities it hands out, so a restarted driver gets the same source
/// and binds it to its new thread.)
pub struct IrqLine {
    slot: usize,
}

#[derive(Clone, Copy)]
struct Binding {
    tid: u32,
    bits: u64,
}

struct Slot {
    source: Option<backend::Source>,
    binding: Option<Binding>,
}

static TABLE: Mutex<[Slot; SLOTS]> = Mutex::new([const { Slot { source: None, binding: None } }; SLOTS]);

fn locked<R>(f: impl FnOnce(&mut [Slot; SLOTS]) -> R) -> R {
    interrupts::without(|| f(&mut TABLE.lock()))
}

/// Claim `source` (it isn't claimed already) in a slot `backend` picks.
fn claim(source: backend::Source) -> Result<Arc<IrqLine>, Error> {
    let slot = locked(|t| {
        if t.iter().any(|s| s.source.as_ref().is_some_and(|x| x.same(&source))) {
            return Err(Error::Full);
        }
        let slot = backend::slot_for(&source, |i| t[i].source.is_none()).ok_or(Error::Full)?;
        t[slot].source = Some(source.clone());
        Ok(slot)
    })?;
    if let Err(e) = backend::setup(slot, &source) {
        locked(|t| t[slot].source = None);
        return Err(e);
    }
    Ok(Arc::new(IrqLine { slot }))
}

/// `IRQ_CLAIM`: the interrupt controller's line `line`.
pub fn claim_line(line: u64) -> Result<Arc<IrqLine>, Error> {
    claim(backend::line(line)?)
}

/// Signal `bits` to thread `tid` when it fires, and unmask it.
pub fn bind(irq: &IrqLine, tid: u32, bits: u64) {
    locked(|t| t[irq.slot].binding = Some(Binding { tid, bits }));
    ack(irq);
}

/// The driver has handled the interrupt: unmask it again.
pub fn ack(irq: &IrqLine) {
    let source = locked(|t| t[irq.slot].binding.and(t[irq.slot].source.clone()));
    if let Some(s) = source {
        backend::unmask(irq.slot, &s);
    }
}

/// From the interrupt handler (interrupts off): slot `slot` fired. Masks
/// it (if it is a line) and signals the driver; false if nobody has bound
/// it (the caller masks it and complains).
pub fn fire(slot: usize) -> bool {
    let (b, s) = {
        let t = TABLE.lock();
        let Some(e) = t.get(slot) else { return false };
        (e.binding, e.source.clone())
    };
    let (Some(b), Some(s)) = (b, s) else {
        return false;
    };
    backend::mask(slot, &s);
    // A driver that has died isn't signalled; the source stays masked
    // until its successor binds it.
    crate::task::signal(b.tid, b.bits);
    true
}

impl Drop for IrqLine {
    fn drop(&mut self) {
        let s = locked(|t| {
            t[self.slot].binding = None;
            t[self.slot].source.take()
        });
        if let Some(s) = s {
            backend::release(self.slot, &s);
        }
    }
}

mod backend {
    use touchos_abi::Error;

    /// An AITC line; its slot is its number.
    #[derive(Clone)]
    pub struct Source(pub u32);

    impl Source {
        pub fn same(&self, o: &Source) -> bool {
            self.0 == o.0
        }
    }

    pub fn line(line: u64) -> Result<Source, Error> {
        if line >= super::SLOTS as u64 || crate::board::irq::reserved(line as u32) {
            return Err(Error::InvalidArgument);
        }
        Ok(Source(line as u32))
    }

    pub fn slot_for(s: &Source, free: impl Fn(usize) -> bool) -> Option<usize> {
        free(s.0 as usize).then_some(s.0 as usize)
    }

    pub fn setup(_slot: usize, _s: &Source) -> Result<(), Error> {
        Ok(()) // enabled when bound
    }

    pub fn mask(_slot: usize, s: &Source) {
        crate::board::irq::disable(s.0);
    }

    pub fn unmask(_slot: usize, s: &Source) {
        crate::board::irq::enable(s.0);
    }

    pub fn release(_slot: usize, s: &Source) {
        crate::board::irq::disable(s.0);
    }
}
