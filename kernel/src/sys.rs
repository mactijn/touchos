//! Syscall implementations (entry: `arch::syscall`). Every argument from
//! user space is checked here: capability slots against the caller's
//! table and rights, pointers through `vm::AddressSpace::read`/`write`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use touchos_abi::{Error, SpawnArgs, Word, join64, nr, rights};

use crate::uabi::{level, prot, status};

use crate::arch::syscall::SyscallFrame;
use crate::cap::{self, Cap, MemObj, Mmio, Object};
use crate::mem::PAGE;
use crate::task::{self, Process};

type R = Result<(u64, u64), Error>;

fn one(v: u64) -> R {
    Ok((v, 0))
}

pub fn dispatch(f: &mut SyscallFrame) {
    let [a, b, c, d] = f.args();
    // 64-bit values (deadlines, times) arrive as two words, low then high.
    let wide = |lo: u64, hi: u64| join64(lo as Word, hi as Word);
    let r: R = match f.number() as Word {
        nr::LOG => log(a, b, c),
        nr::EXIT => task::exit_current(a),
        nr::YIELD => {
            task::yield_now();
            one(0)
        }
        nr::WAIT => one(task::wait(wide(a, b))),
        nr::REBOOT => reboot(a, b),
        nr::POWEROFF => poweroff(a),
        nr::TIME_SET => time_set(a, wide(b, c)),
        nr::KLOG_READ => klog_read(a, b, c, d),
        nr::CAP_MINT => cap_mint(a, b, c),
        nr::CAP_DROP => cap_drop(a),
        nr::CAP_INFO => cap_info(a),
        nr::CALL => call(a, b),
        nr::RECV => recv(a, wide(b, c)),
        nr::REPLY_RECV => reply_recv(a, b, wide(c, d)),
        nr::SIGNAL => signal(a, b),
        nr::EP_CREATE => ep_create(),
        nr::NOTIF_CREATE => notif_create(),
        nr::VM_ALLOC => vm_alloc(a, b, c),
        nr::MEM_CREATE => mem_create(a, b),
        nr::MEM_MAP => mem_map(a, b, c, d),
        nr::MEM_UNMAP => mem_unmap(a),
        nr::MEM_PHYS => mem_phys(a, b),
        nr::MMIO_MAP => mmio_map(a, b, c),
        nr::MMIO_DERIVE => mmio_derive(a, b, c),
        nr::IRQ_CLAIM => irq_claim(a, b),
        nr::IRQ_BIND => irq_bind(a, b),
        nr::IRQ_ACK => irq_ack(a),
        nr::PROC_SPAWN => proc_spawn(a, b, c, d),
        nr::PROC_KILL => proc_kill(a),
        nr::PROC_STATUS => proc_status(a),
        nr::WATCHDOG => watchdog(a, b),
        nr::MEM_WAIT => mem_wait(a, b),
        nr::MEM_NOTIFY => mem_notify(a),
        nr::RANDOM => random(a, b),
        _ => Err(Error::NoSys),
    };
    match r {
        Ok((v, w)) => f.set_result(v, w),
        Err(e) => f.set_error(e as isize as u64),
    }
}

fn me() -> Result<Arc<Process>, Error> {
    task::current_process().ok_or(Error::InvalidArgument)
}

/// The capability in `slot` of the caller's table.
fn cap(slot: u64) -> Result<Cap, Error> {
    me()?.caps.lock().get(slot as u32).cloned()
}

fn insert(c: Cap) -> R {
    one(me()?.caps.lock().insert(c)? as u64)
}

fn system(slot: u64, right: u32) -> Result<(), Error> {
    let c = cap(slot)?;
    match c.obj {
        Object::System => c.require(right),
        _ => Err(Error::InvalidCap),
    }
}

// ---- basics ----

const MAX_LOG: usize = 1024;

fn log(ptr: u64, len: u64, lvl: u64) -> R {
    let p = me()?;
    let len = (len as usize).min(MAX_LOG);
    let mut buf = alloc::vec![0u8; len];
    p.space.lock().read(ptr, &mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let text = text.trim_end_matches('\n');
    let lvl = match lvl {
        level::ERROR => ::log::Level::Error,
        level::WARN => ::log::Level::Warn,
        level::DEBUG => ::log::Level::Debug,
        level::TRACE => ::log::Level::Trace,
        _ => ::log::Level::Info,
    };
    ::log::log!(lvl, "{}: {text}", p.name());
    one(0)
}

fn reboot(sys: u64, target: u64) -> R {
    system(sys, rights::REBOOT)?;
    match target {
        crate::uabi::reboot::DEFAULT => {
            ::log::warn!("{}: reboot requested", task::current_name());
        }
        _ => return Err(Error::InvalidArgument),
    }
    crate::board::reboot()
}

fn poweroff(sys: u64) -> R {
    system(sys, rights::POWEROFF)?;
    ::log::warn!("{}: power off requested", task::current_name());
    crate::board::poweroff();
    Err(Error::NoSys) // still running: no way to switch off
}

fn watchdog(sys: u64, op: u64) -> R {
    system(sys, rights::WATCHDOG)?;
    match op {
        crate::uabi::watchdog::PET => crate::board::watchdog::pet(),
        crate::uabi::watchdog::STOP => {
            ::log::warn!("{}: watchdog stopped", task::current_name());
            crate::board::watchdog::stop()
        }
        crate::uabi::watchdog::QUERY => return one(crate::board::watchdog::query() as u64),
        _ => return Err(Error::InvalidArgument),
    }
    one(crate::board::watchdog::running() as u64)
}

fn time_set(sys: u64, unix: u64) -> R {
    system(sys, rights::TIME_SET)?;
    crate::time::set_unix_now(unix);
    one(0)
}

fn klog_read(sys: u64, buf: u64, len: u64, cursor: u64) -> R {
    system(sys, rights::KLOG_READ)?;
    let mut tmp = alloc::vec![0u8; (len as usize).min(16 * 1024)];
    let (n, next) = crate::klog::ring_read(cursor, &mut tmp);
    me()?.space.lock().write(buf, &tmp[..n])?;
    Ok((n as u64, next))
}

// ---- capabilities ----

fn cap_mint(slot: u64, rights_mask: u64, badge: u64) -> R {
    let mut c = cap(slot)?;
    c.rights &= rights_mask as u32;
    if badge != 0 {
        if c.badge != 0 {
            return Err(Error::NoRights); // badges can't be changed
        }
        c.badge = badge;
    }
    insert(c)
}

fn cap_drop(slot: u64) -> R {
    if slot < 3 {
        return Err(Error::InvalidCap);
    }
    me()?.caps.lock().remove(slot as u32)?;
    one(0)
}

/// First result: kind | rights (low 16 bits) << 8 (never an error code);
/// second: the size in bytes (memory, MMIO), or 0.
fn cap_info(slot: u64) -> R {
    let c = cap(slot)?;
    let size = match &c.obj {
        Object::Mem(m) => m.len,
        Object::Mmio(m) => m.len,
        _ => 0,
    };
    // The rights' low 16 bits: the result must fit a 32-bit register and
    // stay clear of the error range (-4095..-1) there.
    Ok(((c.obj.kind() & 0xff) as u64 | ((c.rights & 0xffff) as u64) << 8, size))
}

// ---- IPC ----

fn endpoint(slot: u64, right: u32) -> Result<(Arc<cap::Endpoint>, u64), Error> {
    let c = cap(slot)?;
    match &c.obj {
        Object::Endpoint(e) => {
            c.require(right)?;
            Ok((e.clone(), c.badge))
        }
        _ => Err(Error::InvalidCap),
    }
}

fn call(ep: u64, tag: u64) -> R {
    let (e, badge) = endpoint(ep, rights::CALL)?;
    task::call(&e, badge, tag)
}

fn recv(ep: u64, deadline: u64) -> R {
    let (e, _) = endpoint(ep, rights::RECV)?;
    task::recv(&e, deadline)
}

/// Reply to the last caller (if any), then receive on `ep` (unless 0).
fn reply_recv(ep: u64, tag: u64, deadline: u64) -> R {
    let replied = task::reply(tag);
    if ep == 0 {
        return replied.map(|_| (0, 0));
    }
    recv(ep, deadline)
}

fn signal(slot: u64, bits: u64) -> R {
    let c = cap(slot)?;
    match &c.obj {
        Object::Notification(n) => {
            c.require(rights::SIGNAL)?;
            // Dead: the notification's thread has ended, so nobody will
            // ever see it (servers drop that client's resources).
            if task::signal(n.thread, bits) { one(0) } else { Err(Error::Dead) }
        }
        _ => Err(Error::InvalidCap),
    }
}

fn ep_create() -> R {
    insert(Cap::new(Object::Endpoint(cap::new_endpoint())))
}

/// A new capability to our own notification (to hand to others).
fn notif_create() -> R {
    let tid = task::current_thread_id();
    insert(Cap::new(Object::Notification(Arc::new(
        cap::Notification { thread: tid },
    ))))
}

// ---- memory ----

fn vm_alloc(va: u64, len: u64, p: u64) -> R {
    me()?.space.lock().map_new(va, len, p)?;
    one(va)
}

const MAX_MEM: u64 = 64 * 1024 * 1024;
const MAX_CONTIG: u64 = 4 * 1024 * 1024;

fn mem_create(len: u64, flags: u64) -> R {
    if len == 0 || len > MAX_MEM {
        return Err(Error::InvalidArgument);
    }
    if len > crate::mem::free_bytes() / 4 {
        return Err(Error::NoMemory);
    }
    let obj = if flags & crate::uabi::mem::CONTIGUOUS != 0 {
        if len > MAX_CONTIG {
            return Err(Error::InvalidArgument);
        }
        MemObj::new_contiguous(len)
    } else {
        MemObj::new(len)
    };
    insert(Cap::new(Object::Mem(Arc::new(obj))))
}

fn mem_obj(slot: u64) -> Result<(Arc<MemObj>, Cap), Error> {
    let c = cap(slot)?;
    match &c.obj {
        Object::Mem(m) => Ok((m.clone(), c.clone())),
        _ => Err(Error::InvalidCap),
    }
}

/// Map `len` bytes of a memory object from byte `off` (page aligned).
fn mem_map(slot: u64, off: u64, len: u64, p: u64) -> R {
    let (m, c) = mem_obj(slot)?;
    c.require(rights::MAP_READ)?;
    if p & prot::WRITE != 0 {
        c.require(rights::MAP_WRITE)?;
    }
    if off % PAGE != 0 || len == 0 {
        return Err(Error::InvalidArgument);
    }
    let first = (off / PAGE) as usize;
    let pages = len.div_ceil(PAGE) as usize;
    one(me()?.space.lock().map_mem(m, first, pages, p)?)
}

fn mem_unmap(va: u64) -> R {
    me()?.space.lock().unmap(va)?;
    one(0)
}

fn mem_phys(slot: u64, off: u64) -> R {
    let (m, c) = mem_obj(slot)?;
    c.require(rights::PHYS)?;
    let f = m
        .frames
        .get((off / PAGE) as usize)
        .ok_or(Error::InvalidArgument)?;
    one(f + off % PAGE)
}

// ---- devices ----

const MAX_MMIO: u64 = 16 * 1024 * 1024;

fn mmio_allowed(phys: u64, len: u64) -> Result<(), Error> {
    let end = phys.checked_add(len).ok_or(Error::InvalidArgument)?;
    if len == 0 || len > MAX_MMIO || crate::mem::overlaps_ram(phys, len) {
        return Err(Error::NoRights);
    }
    if crate::board::KERNEL_MMIO.iter().any(|&(b, l)| phys < b + l && b < end) {
        return Err(Error::NoRights);
    }
    Ok(())
}

fn mmio_derive(root: u64, phys: u64, len: u64) -> R {
    match cap(root)?.obj {
        Object::MmioRoot => {}
        _ => return Err(Error::InvalidCap),
    }
    mmio_allowed(phys, len)?;
    insert(Cap::new(Object::Mmio(Arc::new(Mmio {
        phys,
        len,
        framebuffer: false,
    }))))
}

fn irq_claim(root: u64, line: u64) -> R {
    match cap(root)?.obj {
        Object::IrqRoot => {}
        _ => return Err(Error::InvalidCap),
    }
    insert(Cap::new(Object::Irq(crate::irqs::claim_line(line)?)))
}

fn irq_line(slot: u64) -> Result<Arc<crate::irqs::IrqLine>, Error> {
    match cap(slot)?.obj {
        Object::Irq(l) => Ok(l),
        _ => Err(Error::InvalidCap),
    }
}

fn irq_bind(slot: u64, bits: u64) -> R {
    let l = irq_line(slot)?;
    if bits == 0 {
        return Err(Error::InvalidArgument);
    }
    crate::irqs::bind(&l, task::current_thread_id(), bits);
    one(0)
}

fn irq_ack(slot: u64) -> R {
    crate::irqs::ack(&*irq_line(slot)?);
    one(0)
}

fn mmio_map(slot: u64, off: u64, len: u64) -> R {
    let c = cap(slot)?;
    let Object::Mmio(m) = &c.obj else {
        return Err(Error::InvalidCap);
    };
    let end = off.checked_add(len).ok_or(Error::InvalidArgument)?;
    if len == 0 || end > m.len {
        return Err(Error::InvalidArgument);
    }
    let p = me()?;
    let va = p
        .space
        .lock()
        .map_device(m.phys + off, len, m.framebuffer)?;
    if m.framebuffer {
        crate::klog::framebuffer_taken(p.id());
    }
    one(va)
}

// ---- processes ----

fn nul_str(b: &[u8]) -> &str {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    core::str::from_utf8(&b[..n]).unwrap_or("?")
}

/// Start a program from a memory object (e.g. a file in the initrd).
fn proc_spawn(mem_slot: u64, off: u64, len: u64, args_ptr: u64) -> R {
    let p = me()?;
    let (m, c) = mem_obj(mem_slot)?;
    c.require(rights::MAP_READ)?;
    let mut args: Box<SpawnArgs> = unsafe { Box::new_zeroed().assume_init() };
    let raw = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut *args as *mut SpawnArgs).cast::<u8>(),
            core::mem::size_of::<SpawnArgs>(),
        )
    };
    p.space.lock().read(args_ptr, raw)?;
    if args.ncaps as usize > args.caps.len() || args.cmdline_len as usize > args.cmdline.len() {
        return Err(Error::InvalidArgument);
    }
    let mut caps = Vec::new();
    {
        let table = p.caps.lock();
        for e in &args.caps[..args.ncaps as usize] {
            caps.push((String::from(nul_str(&e.name)), table.get(e.slot)?.clone()));
        }
    }
    let death = match args.death_notification {
        0 => None,
        s => match cap(s as u64)?.obj {
            Object::Notification(n) => Some((n, args.death_bits as u64)),
            _ => return Err(Error::InvalidCap),
        },
    };
    let image = m.bytes(off, len)?;
    let name = nul_str(&args.name);
    let cmdline = core::str::from_utf8(&args.cmdline[..args.cmdline_len as usize])
        .map_err(|_| Error::InvalidArgument)?;
    let info = task::spawn(task::Spawn {
        name,
        elf: &image,
        cmdline,
        priority: match args.priority {
            0 => task::prio::USER,
            n => n.min(255) as u8,
        },
        caps,
        death,
    })
    .map_err(|e| {
        ::log::warn!("{}: can't start {name}: {e}", p.name());
        Error::InvalidArgument
    })?;
    let id = info.id;
    let slot = p.caps.lock().insert(Cap::new(Object::Process(info)))?;
    Ok((slot as u64, id as u64))
}

fn proc_info(slot: u64, right: u32) -> Result<Arc<cap::ProcInfo>, Error> {
    let c = cap(slot)?;
    match &c.obj {
        Object::Process(i) => {
            c.require(right)?;
            Ok(i.clone())
        }
        _ => Err(Error::InvalidCap),
    }
}

fn proc_kill(slot: u64) -> R {
    let info = proc_info(slot, rights::KILL)?;
    if info.status.lock().0 != status::RUNNING {
        return one(0);
    }
    task::kill(info.id)?;
    one(0)
}

fn proc_status(slot: u64) -> R {
    let info = proc_info(slot, rights::STATUS)?;
    let st = *info.status.lock();
    Ok(st)
}

/// Be signalled with `bits` when someone calls MEM_NOTIFY on this memory
/// object (we consume a ring in it).
fn mem_wait(slot: u64, bits: u64) -> R {
    let (m, _) = mem_obj(slot)?;
    *m.waiter.lock() = Some((task::current_thread_id(), bits));
    one(0)
}

/// Signal whoever waits on this memory object (MEM_WAIT).
fn mem_notify(slot: u64) -> R {
    let (m, c) = mem_obj(slot)?;
    c.require(rights::MAP_WRITE)?;
    if let Some((tid, bits)) = *m.waiter.lock() {
        let _ = task::signal(tid, bits); // (a consumer that ended: nobody to wake)
    }
    one(0)
}

/// `RANDOM`: fill a user buffer (at most 256 bytes) from the board's
/// entropy source.
fn random(buf: u64, len: u64) -> R {
    if len > 256 {
        return Err(Error::InvalidArgument);
    }
    let mut tmp = [0u8; 256];
    let out = &mut tmp[..len as usize];
    crate::board::random(out);
    me()?.space.lock().write(buf, out)?;
    one(0)
}
