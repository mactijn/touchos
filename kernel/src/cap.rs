//! Capabilities and the kernel objects they name.
//!
//! A process can only use what is in its capability table; a slot number is
//! meaningless anywhere else. Capabilities are copied between processes by
//! `PROC_SPAWN` and in IPC messages, and narrowed with `CAP_MINT`.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;
use touchos_abi::{Error, kind};

use crate::mem::{self, PAGE};

/// Synchronous IPC rendezvous point. Waiting senders queue here; the
/// receiving side is found by the scheduler (threads blocked in RECV on
/// this endpoint's id). All fields are only touched with the scheduler lock.
pub struct Endpoint {
    pub id: u32,
    /// Waiting callers: (thread id, badge, tag).
    pub queue: Mutex<alloc::collections::VecDeque<(u32, u64, u64)>>,
}

/// Signals one thread (its pending bits).
pub struct Notification {
    pub thread: u32,
}

/// Memory that can be mapped into several processes.
pub struct MemObj {
    /// One physical address per page.
    pub frames: Vec<u64>,
    /// Size in bytes (the last page may be partly used).
    pub len: u64,
    /// Free the frames when the object goes away.
    pub owned: bool,
    /// The frames are physically contiguous.
    pub contiguous: bool,
    /// MEM_WAIT/MEM_NOTIFY: the thread to signal, and with which bits,
    /// when another process notifies this object (a ring's consumer).
    pub waiter: Mutex<Option<(u32, u64)>>,
    /// Its address in the global window (ARM, FCSE: see
    /// arch/arm/paging.rs), given on its first cached mapping.
    global: Mutex<Option<u64>>,
}

impl MemObj {
    /// The object's address in the global window (None: the window is full).
    pub fn global_va(&self) -> Option<u64> {
        let mut g = self.global.lock();
        if g.is_none() {
            *g = crate::arch::paging::global_alloc(self.frames.len() as u64);
        }
        *g
    }

    /// Fresh zeroed memory, freed with the object.
    pub fn new(len: u64) -> MemObj {
        let pages = len.div_ceil(PAGE).max(1);
        MemObj {
            frames: (0..pages).map(|_| mem::alloc_frame()).collect(),
            len,
            owned: true,
            contiguous: false,
            waiter: Mutex::new(None),
            global: Mutex::new(None),
        }
    }

    /// Physically contiguous memory for DMA. Never freed: a device may still
    /// write to it after its driver died.
    pub fn new_contiguous(len: u64) -> MemObj {
        let pages = len.div_ceil(PAGE).max(1);
        let base = mem::alloc_frames(pages);
        // Zeroed through the cached direct map: write those lines out now,
        // before a device can DMA into pages no process has mapped yet (a
        // later eviction would overwrite what the device wrote).
        crate::arch::paging::sync_direct(base, (pages * PAGE) as usize);
        MemObj {
            frames: (0..pages).map(|i| base + i * PAGE).collect(),
            len,
            owned: false,
            contiguous: true,
            waiter: Mutex::new(None),
            global: Mutex::new(None),
        }
    }

    /// Existing memory (a boot module), not owned.
    pub fn wrap(phys: u64, len: u64) -> MemObj {
        let pages = len.div_ceil(PAGE).max(1);
        MemObj {
            frames: (0..pages)
                .map(|i| (phys & !(PAGE - 1)) + i * PAGE)
                .collect(),
            len,
            owned: false,
            contiguous: true,
            waiter: Mutex::new(None),
            global: Mutex::new(None),
        }
    }

    /// The bytes `[off, off+len)`, as one slice if contiguous, else copied.
    pub fn bytes(&self, off: u64, len: u64) -> Result<alloc::borrow::Cow<'_, [u8]>, Error> {
        let end = off.checked_add(len).ok_or(Error::InvalidArgument)?;
        if end > self.len {
            return Err(Error::InvalidArgument);
        }
        // A process may have written it through its own mapping.
        crate::arch::paging::sync_all();
        if self.contiguous {
            let p = mem::phys_to_virt(self.frames[0] + off);
            return Ok(alloc::borrow::Cow::Borrowed(unsafe {
                core::slice::from_raw_parts(p, len as usize)
            }));
        }
        let mut v = alloc::vec![0u8; len as usize];
        for (i, b) in v.iter_mut().enumerate() {
            let a = off + i as u64;
            *b = unsafe { *mem::phys_to_virt(self.frames[(a / PAGE) as usize] + a % PAGE) };
        }
        Ok(alloc::borrow::Cow::Owned(v))
    }
}

impl Drop for MemObj {
    fn drop(&mut self) {
        if let Some(va) = *self.global.lock() {
            crate::arch::paging::global_free(va, self.frames.len() as u64);
        }
        if self.owned {
            for &f in &self.frames {
                mem::free_frame(f);
            }
        }
    }
}

/// A range of device registers (or the framebuffer).
pub struct Mmio {
    pub phys: u64,
    pub len: u64,
    /// The boot framebuffer: mapped write-combining, and the kernel console
    /// stops drawing while a process has it mapped.
    pub framebuffer: bool,
}

/// What `PROC_STATUS` reports, shared by the process and its capabilities.
pub struct ProcInfo {
    pub id: u32,
    pub name: alloc::string::String,
    /// `touchos_abi::status` value and code.
    pub status: Mutex<(u64, u64)>,
}

#[derive(Clone)]
pub enum Object {
    Endpoint(Arc<Endpoint>),
    Notification(Arc<Notification>),
    Mem(Arc<MemObj>),
    Mmio(Arc<Mmio>),
    MmioRoot,
    Process(Arc<ProcInfo>),
    System,
    Irq(Arc<crate::irqs::IrqLine>),
    IrqRoot,
}

impl Object {
    pub fn kind(&self) -> u32 {
        match self {
            Object::Endpoint(_) => kind::ENDPOINT,
            Object::Notification(_) => kind::NOTIFICATION,
            Object::Mem(_) => kind::MEMORY,
            Object::Mmio(_) => kind::MMIO,
            Object::MmioRoot => kind::MMIO_ROOT,
            Object::Process(_) => kind::PROCESS,
            Object::System => kind::SYSTEM,
            Object::Irq(_) => kind::IRQ,
            Object::IrqRoot => kind::IRQ_ROOT,
        }
    }
}

#[derive(Clone)]
pub struct Cap {
    pub obj: Object,
    pub rights: u32,
    pub badge: u64,
}

impl Cap {
    pub fn new(obj: Object) -> Cap {
        Cap {
            obj,
            rights: u32::MAX,
            badge: 0,
        }
    }
    pub fn require(&self, rights: u32) -> Result<(), Error> {
        if self.rights & rights == rights {
            Ok(())
        } else {
            Err(Error::NoRights)
        }
    }
}

pub const MAX_CAPS: usize = 256;
/// Slots below this are fixed (null, own notification, supervisor).
const FIRST_FREE: usize = 3;

#[derive(Default)]
pub struct CapTable {
    slots: Vec<Option<Cap>>,
}

impl CapTable {
    pub fn get(&self, slot: u32) -> Result<&Cap, Error> {
        self.slots
            .get(slot as usize)
            .and_then(Option::as_ref)
            .ok_or(Error::InvalidCap)
    }

    pub fn insert(&mut self, cap: Cap) -> Result<u32, Error> {
        if self.slots.len() < FIRST_FREE {
            self.slots.resize(FIRST_FREE, None);
        }
        if let Some(i) = self.slots[FIRST_FREE..].iter().position(Option::is_none) {
            self.slots[FIRST_FREE + i] = Some(cap);
            return Ok((FIRST_FREE + i) as u32);
        }
        if self.slots.len() >= MAX_CAPS {
            return Err(Error::Full);
        }
        self.slots.push(Some(cap));
        Ok(self.slots.len() as u32 - 1)
    }

    /// Put `cap` in a fixed slot (process set-up only).
    pub fn set(&mut self, slot: u32, cap: Cap) {
        let i = slot as usize;
        if self.slots.len() <= i {
            self.slots.resize(i + 1, None);
        }
        self.slots[i] = Some(cap);
    }

    pub fn remove(&mut self, slot: u32) -> Result<Cap, Error> {
        self.slots
            .get_mut(slot as usize)
            .and_then(Option::take)
            .ok_or(Error::InvalidCap)
    }
}

static NEXT_EP: AtomicU32 = AtomicU32::new(1);

pub fn new_endpoint() -> Arc<Endpoint> {
    Arc::new(Endpoint {
        id: NEXT_EP.fetch_add(1, Ordering::Relaxed),
        queue: Mutex::new(alloc::collections::VecDeque::new()),
    })
}
