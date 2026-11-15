//! Processes and threads: the types and the scheduler's state. The work
//! is in the submodules:
//!
//! - `sched`: who runs next (switching, the timer tick, sleeping, waiting,
//!   notifications);
//! - `process`: how a process starts (from an ELF image) and ends;
//! - `ipc`: how a message crosses (call, receive, reply).
//!
//! - A **process** is an address space, a capability table and its threads
//!   (one, for now).
//! - A **thread** has a kernel stack and, in user processes, saved FPU state
//!   and an IPC buffer (UTCB).
//! - **Kernel threads**: "kernel" (the boot context; after starting init it
//!   only logs a heartbeat) and "idle".
//!
//! Scheduling: strict priority, round-robin within a priority. User threads
//! are preempted by the 250 Hz timer; kernel threads only switch when they
//! block (they never hold a lock when they do). All scheduler state is
//! touched with interrupts off, under one spinlock (`SCHED`).
//!
//! **Several cores** (`arch::MAX_CPUS`; 1 on the ARM926): each runs one thread (`Sched::current`,
//! by core) and has its own idle thread, pinned to it. A thread is "on" a
//! core (`on_cpu`) from when that core picks it until another thread has
//! taken over the core's stack after the switch (`finish_switch`): no
//! other core may pick it in between, its registers aren't saved yet. A
//! thread that becomes ready while a core idles wakes that core with an
//! IPI (`kick_idle`).

mod elf;
mod ipc;
pub mod kinfo;
mod kstack;
mod process;
mod sched;

pub use ipc::{call, recv, reply};
pub use process::{Spawn, exit_current, kill, spawn, user_fault};
pub use sched::{resched, IDLE_US, signal, sleep_ms, thread_start, tick, wait, yield_now};

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;
use touchos_abi::Error;

use crate::arch::{self, FpuState, context, irq as interrupts, paging};
use crate::cap::{CapTable, Notification, ProcInfo};
use crate::vm::AddressSpace;
use kstack::KStack;

/// Priorities: lower runs first.
pub mod prio {
    /// The kernel's boot thread (heartbeat).
    pub const KERNEL: u8 = 1;
    /// Highest priority a user process can get (init).
    pub const USER_MAX: u8 = 2;
    pub const USER: u8 = 4;
    pub const USER_MIN: u8 = 200;
    pub const IDLE: u8 = 255;
}

pub struct Process {
    pub info: Arc<ProcInfo>,
    pub space: Mutex<AddressSpace>,
    pub caps: Mutex<CapTable>,
    /// Signalled when the process ends.
    death: Option<(Arc<Notification>, u64)>,
}

impl Process {
    pub fn id(&self) -> u32 {
        self.info.id
    }
    pub fn name(&self) -> &str {
        &self.info.name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    /// Blocked until this time (µs since boot; `u64::MAX` = forever) or a
    /// notification.
    Sleeping(u64),
    /// A kernel thread waiting for the next timer tick.
    WaitTick,
    /// Queued on an endpoint, waiting for a receiver.
    SendBlocked(u32),
    /// A receiver has our message; waiting for its reply.
    ReplyBlocked(u32),
    /// Waiting for a message on an endpoint, a notification or the deadline.
    RecvBlocked {
        ep: u32,
        deadline: u64,
    },
    Dead,
}

type IpcResult = Result<(u64, u64), Error>;

struct Thread {
    id: u32,
    name: String,
    process: Option<Arc<Process>>,
    priority: u8,
    state: State,
    /// The saved kernel stack pointer while switched out.
    sp: u64,
    /// `None` for the boot thread (it keeps its boot stack).
    kstack: Option<KStack>,
    /// User threads only.
    fpu: Option<Box<FpuState>>,
    /// The address space's page-table root (TTBR).
    root: u64,
    /// Notification bits not yet returned by `WAIT`/`RECV`.
    pending: u64,
    /// Physical address of the UTCB (user threads).
    utcb: u64,
    /// Set by the other side of an IPC before waking us.
    ipc_result: Option<IpcResult>,
    /// The caller we owe a reply.
    reply_to: Option<u32>,
    /// Running on a core, or still being switched away from (see the
    /// module's notes).
    on_cpu: bool,
    /// Only this core may run it (the idle threads).
    pin: Option<u8>,
    /// Times it was switched to, and the count at the last [`busiest`].
    runs: u32,
    runs_seen: u32,
}

impl Thread {
    fn kernel(name: &str, priority: u8, state: State) -> Thread {
        Thread {
            id: new_id(),
            name: name.into(),
            process: None,
            priority,
            state,
            sp: 0,
            kstack: None,
            fpu: None,
            root: paging::kernel_root(),
            pending: 0,
            utcb: 0,
            ipc_result: None,
            reply_to: None,
            on_cpu: false,
            pin: None,
            runs: 0,
            runs_seen: 0,
        }
    }

    /// May core `cpu` pick it (it isn't running, or still being left, on
    /// another core)?
    fn runnable_on(&self, cpu: usize) -> bool {
        self.state == State::Ready && !self.on_cpu && self.pin.is_none_or(|p| p as usize == cpu)
    }
}

struct Sched {
    threads: Vec<Box<Thread>>,
    /// The thread each core runs (thread ids).
    current: [u32; arch::MAX_CPUS],
    /// The thread each core just switched away from (`finish_switch`).
    prev: [Option<u32>; arch::MAX_CPUS],
    /// Each core's idle thread (0: the core isn't running).
    idle: [u32; arch::MAX_CPUS],
    /// IPC handoff: run this thread next if nothing of higher priority is
    /// ready (the receiver of a message, the caller that got its reply).
    hint: Option<u32>,
}

impl Sched {
    /// The index of this core's thread.
    fn me(&self) -> usize {
        self.index(self.current[arch::cpu()]).expect("the current thread is gone")
    }

    fn cur(&mut self) -> &mut Thread {
        let i = self.me();
        &mut self.threads[i]
    }

    /// A thread became ready: if a core idles, let it look (one is
    /// enough; it takes the best one).
    fn kick_idle(&self) {
        let me = arch::cpu();
        for c in 0..arch::MAX_CPUS {
            if c != me && self.idle[c] != 0 && self.current[c] == self.idle[c] {
                arch::kick(c);
                return;
            }
        }
    }

    fn index(&self, tid: u32) -> Option<usize> {
        self.threads.iter().position(|t| t.id == tid)
    }

    /// Highest priority runnable thread; round-robin among equals, starting
    /// after the current one. The idle thread is always runnable.
    fn pick(&self) -> usize {
        let cpu = arch::cpu();
        let me = self.me();
        let n = self.threads.len();
        let mut best: Option<(u8, usize)> = None;
        for k in 1..=n {
            let i = (me + k) % n;
            let t = &self.threads[i];
            let ok = if i == me { matches!(t.state, State::Ready | State::Running) } else { t.runnable_on(cpu) };
            if ok && best.is_none_or(|(p, _)| t.priority < p) {
                best = Some((t.priority, i));
            }
        }
        let (best_prio, best) = best.expect("no runnable thread");
        if let Some(h) = self.hint
            && let Some(i) = self.index(h)
            && self.threads[i].runnable_on(cpu)
            && self.threads[i].priority <= best_prio
        {
            return i;
        }
        best
    }

    fn wake_sleepers(&mut self, now: u64) {
        for t in &mut self.threads {
            match t.state {
                State::Sleeping(d) | State::RecvBlocked { deadline: d, .. } if d <= now => {
                    t.state = State::Ready;
                }
                _ => {}
            }
        }
    }

    /// Free dead threads (never one that is on a core: we may be on its
    /// stack, or another core may).
    fn reap(&mut self) {
        let mut i = 0;
        while i < self.threads.len() {
            if self.threads[i].state == State::Dead && !self.threads[i].on_cpu {
                let t = self.threads.remove(i);
                if let Some(p) = &t.process
                    && Arc::strong_count(p) == 1
                {
                    ::log::debug!(
                        "task: process {} ({}) freed, {} KiB",
                        p.id(),
                        p.name(),
                        p.space.lock().owned_bytes() / 1024
                    );
                }
                drop(t);
            } else {
                i += 1;
            }
        }
    }

    /// Another thread this core could run, at `prio` or better?
    fn other_ready(&self, prio: u8) -> bool {
        let cpu = arch::cpu();
        self.threads.iter().any(|t| t.runnable_on(cpu) && t.priority <= prio)
    }

    /// False if the thread has ended (ids are never reused).
    fn signal(&mut self, tid: u32, bits: u64) -> bool {
        let Some(i) = self.index(tid).filter(|&i| self.threads[i].state != State::Dead) else {
            return false;
        };
        let t = &mut self.threads[i];
        t.pending |= bits;
        if matches!(t.state, State::Sleeping(_) | State::RecvBlocked { .. }) {
            t.state = State::Ready;
            self.hint = Some(tid);
            self.kick_idle();
        }
        true
    }

    fn process_of(&self, pid: u32) -> Option<Arc<Process>> {
        self.threads
            .iter()
            .filter_map(|t| t.process.as_ref())
            .find(|p| p.id() == pid)
            .cloned()
    }

    /// Mark a process dead: its threads stop, callers waiting for its
    /// replies get `Dead`, its supervisor is notified. Scheduler state
    /// only: the caller then runs `process::on_exit` without the lock
    /// (devices, the console).
    fn end_process(&mut self, p: &Arc<Process>, st: (u64, u64)) {
        *p.info.status.lock() = st;
        let tids: Vec<u32> = self
            .threads
            .iter()
            .filter(|t| t.process.as_ref().is_some_and(|q| Arc::ptr_eq(q, p)))
            .map(|t| t.id)
            .collect();
        for t in &mut self.threads {
            if tids.contains(&t.id) {
                t.state = State::Dead;
            } else if let State::ReplyBlocked(server) = t.state
                && tids.contains(&server)
            {
                t.ipc_result = Some(Err(Error::Dead));
                t.state = State::Ready;
            }
        }
        if let Some((n, bits)) = &p.death {
            self.signal(n.thread, *bits);
        }
        // Its threads running on other cores stop at once, not at their
        // next tick.
        let me = arch::cpu();
        for c in 0..arch::MAX_CPUS {
            if c != me && tids.contains(&self.current[c]) {
                arch::kick(c);
            }
        }
    }
}

static SCHED: Mutex<Option<Sched>> = Mutex::new(None);
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

fn with<R>(f: impl FnOnce(&mut Sched) -> R) -> R {
    interrupts::without(|| f(SCHED.lock().as_mut().expect("task::init not called")))
}

fn new_id() -> u32 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// `test_fail=kstack`: run a fresh kernel stack into its guard page (the
/// overflow must be reported, not loop or corrupt memory).
pub fn test_kstack_overflow() -> ! {
    let k = KStack::new();
    crate::arch::overflow_into_guard(k.bottom() as usize)
}

/// Turn the boot context into the "kernel" thread and start the boot
/// core's idle thread.
pub fn init() {
    kinfo::init();
    let mut kernel = Box::new(Thread::kernel("kernel", prio::KERNEL, State::Running));
    kernel.on_cpu = true;
    let idle_stack = KStack::new();
    let mut idle = Box::new(Thread::kernel("idle", prio::IDLE, State::Ready));
    idle.sp = context::new_kernel_stack(idle_stack.top, sched::idle_loop);
    idle.kstack = Some(idle_stack);
    idle.pin = Some(0);
    let mut current = [0; arch::MAX_CPUS];
    current[0] = kernel.id;
    let mut idle_ids = [0; arch::MAX_CPUS];
    idle_ids[0] = idle.id;
    *SCHED.lock() = Some(Sched {
        threads: alloc::vec![kernel, idle],
        current,
        prev: [None; arch::MAX_CPUS],
        idle: idle_ids,
        hint: None,
    });
}

/// The current thread's process (None for kernel threads).
pub fn current_process() -> Option<Arc<Process>> {
    with(|s| s.cur().process.clone())
}

pub fn current_name() -> String {
    with(|s| s.cur().name.clone())
}

pub fn current_thread_id() -> u32 {
    with(|s| s.cur().id)
}

/// Number of threads, and of those in user space, for the heartbeat log.
/// The threads switched to most often since the last call: (name, times),
/// at most `n`, busiest first. Who wakes up while nothing happens.
pub fn busiest(n: usize) -> Vec<(String, u32)> {
    with(|s| {
        let mut v: Vec<(String, u32)> = s
            .threads
            .iter_mut()
            .filter(|t| t.pin.is_none())
            .map(|t| {
                let d = t.runs.wrapping_sub(t.runs_seen);
                t.runs_seen = t.runs;
                (t.name.clone(), d)
            })
            .filter(|(_, d)| *d > 0)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v.truncate(n);
        v
    })
}

pub fn summary() -> (usize, usize) {
    with(|s| {
        let procs = s.threads.iter().filter(|t| t.process.is_some()).count();
        (s.threads.len(), procs)
    })
}
