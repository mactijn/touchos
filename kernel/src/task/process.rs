//! How a process starts (from an ELF image, with its capabilities and its
//! BootInfo) and how it ends (exit, kill, a CPU exception).

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use touchos_abi::{ABI_VERSION, BOOTINFO_MAGIC, BootInfo, CapEntry, Error, MAX_BOOT_CAPS, rights, slot};

use super::kstack::KStack;
use super::sched::schedule;
use super::{Process, SCHED, State, Thread, current_name, current_process, elf, kinfo, new_id, prio, with};
use crate::arch::{self, FpuState, context, irq as interrupts, paging};
use crate::cap::{Cap, CapTable, Notification, Object, ProcInfo};
use crate::mem::PAGE;
use crate::uabi::layout::{BOOTINFO_VA, COUNTER_VA, DEFAULT_STACK_SIZE, KINFO_VA, STACK_TOP, UTCB_VA};
use crate::uabi::{prot, status};
use crate::vm::AddressSpace;

/// What a new process gets.
pub struct Spawn<'a> {
    pub name: &'a str,
    pub elf: &'a [u8],
    pub cmdline: &'a str,
    pub priority: u8,
    /// Named capabilities (they appear in the BootInfo manifest).
    pub caps: Vec<(String, Cap)>,
    /// Signalled with these bits when the process ends.
    pub death: Option<(Arc<Notification>, u64)>,
}

fn bootinfo(name: &str, cmdline: &str, manifest: &[CapEntry]) -> Box<BootInfo> {
    // BootInfo is plain data; all-zero is a valid (empty) value.
    let mut bi: Box<BootInfo> = unsafe { Box::new_zeroed().assume_init() };
    bi.magic = BOOTINFO_MAGIC;
    bi.version = ABI_VERSION;
    let n = name.len().min(bi.name.len());
    bi.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    let c = cmdline.len().min(bi.cmdline.len());
    bi.cmdline[..c].copy_from_slice(&cmdline.as_bytes()[..c]);
    bi.cmdline_len = c as u32;
    bi.ncaps = manifest.len() as u32;
    bi.caps[..manifest.len()].copy_from_slice(manifest);
    bi
}

/// Create a process from an ELF image and make its thread runnable.
pub fn spawn(spec: Spawn) -> Result<Arc<ProcInfo>, &'static str> {
    let Spawn {
        name,
        elf: elf_image,
        cmdline,
        priority,
        caps,
        death,
    } = spec;
    if caps.len() > MAX_BOOT_CAPS {
        return Err("too many capabilities");
    }
    let mut space = AddressSpace::new();
    let entry = elf::load(&mut space, elf_image)?;
    let oom = |_: Error| "out of memory";
    space
        .map_new(
            STACK_TOP - DEFAULT_STACK_SIZE,
            DEFAULT_STACK_SIZE,
            prot::WRITE,
        )
        .map_err(oom)?;
    // Uncached where caches alias (ARM's VIVT): the kernel rewrites it every
    // tick, also while this process runs, so a cached copy would go stale.
    let kinfo_kind = if arch::paging::CACHE_ALIASES {
        arch::paging::MemKind::Device
    } else {
        arch::paging::MemKind::Normal
    };
    space
        .map_shared_as(KINFO_VA, &[kinfo::frame()], prot::READ, kinfo_kind)
        .map_err(oom)?;
    if let Some((page, _)) = crate::board::clock::kinfo_clock().user_counter {
        space.map_device_at(COUNTER_VA, page).map_err(oom)?;
    }
    space.map_new(UTCB_VA, PAGE, prot::WRITE).map_err(oom)?;
    let utcb = space.translate(UTCB_VA).ok_or("no UTCB")?.0;

    let tid = new_id();
    let mut table = CapTable::default();
    let mut own = Cap::new(Object::Notification(Arc::new(Notification { thread: tid })));
    own.rights = rights::SIGNAL;
    table.set(slot::SELF_NOTIFICATION, own);
    let mut manifest = Vec::new();
    for (cap_name, cap) in caps {
        let kind = cap.obj.kind();
        let s = table.insert(cap).map_err(|_| "capability table full")?;
        let mut e = CapEntry {
            name: [0; 16],
            slot: s,
            kind,
        };
        let n = cap_name.len().min(16);
        e.name[..n].copy_from_slice(&cap_name.as_bytes()[..n]);
        manifest.push(e);
    }
    let bi = bootinfo(name, cmdline, &manifest);
    let bytes = unsafe {
        core::slice::from_raw_parts(
            (&*bi as *const BootInfo).cast::<u8>(),
            core::mem::size_of::<BootInfo>(),
        )
    };
    space
        .map_init(BOOTINFO_VA, PAGE, prot::READ, 0, bytes)
        .map_err(oom)?;

    let root = space.root();
    let info = Arc::new(ProcInfo {
        id: new_id(),
        name: name.to_string(),
        status: Mutex::new((status::RUNNING, 0)),
    });
    let process = Arc::new(Process {
        info: info.clone(),
        space: Mutex::new(space),
        caps: Mutex::new(table),
        death,
    });
    let kstack = KStack::new();
    let sp = context::new_user_stack(kstack.top, entry, STACK_TOP, BOOTINFO_VA);
    let thread = Box::new(Thread {
        id: tid,
        name: name.to_string(),
        process: Some(process),
        priority: priority.clamp(prio::USER_MAX, prio::USER_MIN),
        state: State::Ready,
        sp,
        kstack: Some(kstack),
        fpu: Some(Box::new(FpuState::new())),
        root,
        pending: 0,
        utcb,
        ipc_result: None,
        reply_to: None,
        on_cpu: false,
        pin: None,
        runs: 0,
        runs_seen: 0,
    });
    with(|s| {
        s.threads.push(thread);
        s.kick_idle();
    });
    ::log::info!("task: started process {} \"{name}\"", info.id);
    Ok(info)
}

/// End the current process (all its threads). Never returns.
fn end_current(st: (u64, u64), how: core::fmt::Arguments) -> ! {
    interrupts::disable();
    let p = {
        let mut g = SCHED.lock();
        let s = g.as_mut().unwrap();
        let t = s.cur();
        let Some(p) = t.process.clone() else {
            panic!("kernel thread {} tried to exit", t.name);
        };
        ::log::info!("task: process {} \"{}\" {how}", p.id(), p.name());
        s.end_process(&p, st);
        p
    };
    on_exit(&p);
    drop(p);
    // The address space is freed when the thread is reaped, from another
    // thread's context; switch to the kernel's page tables now.
    paging::switch_to(paging::kernel_root());
    schedule();
    unreachable!("a dead thread was scheduled");
}

pub fn exit_current(code: u64) -> ! {
    end_current(
        (status::EXITED, code),
        format_args!("exited with code {code}"),
    )
}

pub fn kill_current(reason: &str) -> ! {
    end_current(
        (status::KILLED, status::KILLED_BY_REQUEST),
        format_args!("killed: {reason}"),
    )
}

/// `PROC_KILL`: end another process.
pub fn kill(pid: u32) -> Result<(), Error> {
    if current_process().is_some_and(|p| p.id() == pid) {
        kill_current("by request");
    }
    let p = with(|s| {
        let p = s.process_of(pid).ok_or(Error::Dead)?;
        ::log::info!("task: process {pid} \"{}\" killed by request", p.name());
        s.end_process(&p, (status::KILLED, status::KILLED_BY_REQUEST));
        Ok(p)
    })?;
    on_exit(&p);
    Ok(())
}

/// What an ended process leaves behind outside the scheduler, done
/// without its lock: if it had the framebuffer, the console gets it back.
/// The one place for the next such clean-up.
fn on_exit(p: &Process) {
    crate::klog::process_ended(p.id());
}

/// A CPU exception in user code: report it and end the process.
pub fn user_fault(
    what: &str,
    vector: u8,
    (pc, sp): (u64, u64),
    code: Option<u64>,
    addr: Option<u64>,
) -> ! {
    let mut msg = String::new();
    use core::fmt::Write;
    let (pc_name, sp_name) = arch::REG_NAMES;
    let _ = write!(msg, "{what} at {pc_name} {pc:#x}");
    if let Some(a) = addr {
        let _ = write!(msg, ", address {a:#x}");
    }
    if let Some(c) = code {
        let _ = write!(msg, ", error code {c:#x}");
    }
    let _ = write!(msg, ", {sp_name} {sp:#x}");
    ::log::error!("{}: {msg}", current_name());
    end_current(
        (status::KILLED, vector as u64),
        format_args!("killed: {msg}"),
    )
}

