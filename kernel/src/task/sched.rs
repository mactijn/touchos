//! Who runs next: switching threads, the timer tick, sleeping and waiting,
//! and notifications.

use core::sync::atomic::{AtomicU64, Ordering};

use super::{SCHED, State, kinfo, with};
use crate::arch::{self, context, irq as interrupts, paging};
use crate::time;

/// Microseconds the idle thread has spent waiting for an interrupt (the
/// heartbeat logs the share). Kernel threads aren't preempted, so the wait
/// ends in this thread: the time is the halt plus the handler of the
/// interrupt that ended it.
pub static IDLE_US: AtomicU64 = AtomicU64::new(0);

pub(super) extern "C" fn idle_loop() -> ! {
    // The boot core's idle thread starts through a switch.
    finish_switch();
    loop {
        interrupts::disable();
        schedule();
        let t = time::now_us();
        interrupts::enable_and_wait();
        IDLE_US.fetch_add(time::now_us().saturating_sub(t), Ordering::Relaxed);
    }
}

/// Switch to the best runnable thread (maybe the current one). Interrupts
/// must be off. Returns when this thread is scheduled again.
pub(super) fn schedule() {
    debug_assert!(!interrupts::are_enabled());
    let (save, next_sp) = {
        let mut g = SCHED.lock();
        let s = g.as_mut().unwrap();
        s.reap();
        s.wake_sleepers(time::now_us());
        let cpu = arch::cpu();
        let cur = s.me();
        let next = s.pick();
        s.hint = None;
        if next == cur {
            s.cur().state = State::Running;
            return;
        }
        let c = s.cur();
        if c.state == State::Running {
            c.state = State::Ready;
        }
        if let Some(fpu) = &mut c.fpu {
            fpu.save();
        }
        let save = &mut c.sp as *mut u64;
        // `c` stays on this core until the next thread has taken over
        // (`finish_switch`): its registers aren't saved yet.
        s.prev[cpu] = Some(c.id);
        let n = &mut s.threads[next];
        n.on_cpu = true;
        n.runs = n.runs.wrapping_add(1);
        let nid = n.id;
        s.current[cpu] = nid;
        let n = &mut s.threads[next];
        n.state = State::Running;
        if let Some(k) = &n.kstack {
            arch::set_kernel_stack(k.top);
        }
        let sp = n.sp;
        paging::switch_to(n.root);
        (save, sp)
    };
    unsafe { context::switch(save, next_sp) };
    finish_switch();
}

/// Right after a switch, on the new thread's stack: the thread this core
/// left may now run elsewhere, and ours gets its FPU state back.
fn finish_switch() {
    let mut g = SCHED.lock();
    let s = g.as_mut().unwrap();
    let cpu = arch::cpu();
    if let Some(p) = s.prev[cpu].take()
        && let Some(i) = s.index(p)
    {
        s.threads[i].on_cpu = false;
        // Woken while it was still being left: another core may run it now.
        if s.threads[i].state == State::Ready {
            s.kick_idle();
        }
    }
    if let Some(fpu) = &s.cur().fpu {
        fpu.restore();
    }
}

/// First code of a new user thread (called from `context::user_entry`).
pub extern "C" fn thread_start() {
    finish_switch();
}

/// Timer interrupt (interrupts are off). `from_user`: it interrupted ring 3.
/// Every core's timer; the boot core's also keeps the time (`time::tick`).
pub fn tick(from_user: bool) {
    if arch::cpu() == 0 {
        kinfo::tick(time::ticks());
    }
    let preempt = {
        let mut g = SCHED.lock();
        let Some(s) = g.as_mut() else { return };
        let mut woken = 0;
        for t in &mut s.threads {
            if t.state == State::WaitTick {
                t.state = State::Ready;
                woken += 1;
            }
        }
        let before = s.threads.iter().filter(|t| t.state == State::Ready).count();
        s.wake_sleepers(time::now_us());
        woken += s.threads.iter().filter(|t| t.state == State::Ready).count() - before;
        if woken > 0 {
            s.kick_idle();
        }
        should_preempt(s, from_user)
    };
    if preempt {
        schedule();
    }
}

/// User code is preempted for a ready thread as good or better, and when
/// its thread was ended (from another core).
fn should_preempt(s: &mut super::Sched, from_user: bool) -> bool {
    let c = s.cur();
    let (p, dead) = (c.priority, c.state == State::Dead);
    from_user && (dead || s.other_ready(p))
}

/// Another core asked this one to look for work (`kick`): an idle core
/// does that anyway as its halt ends; user code is preempted if needed.
pub fn resched(from_user: bool) {
    let preempt = {
        let mut g = SCHED.lock();
        let Some(s) = g.as_mut() else { return };
        should_preempt(s, from_user)
    };
    if preempt {
        schedule();
    }
}

/// Let a kernel thread (the boot thread) sleep.
pub fn sleep_ms(ms: u64) {
    let deadline = time::now_us() + ms * 1000;
    while time::now_us() < deadline {
        wait(deadline);
    }
}

pub fn yield_now() {
    interrupts::disable();
    schedule();
}

/// Block until a notification bit is set or `deadline_us` passes. Returns
/// (and clears) the pending bits.
pub fn wait(deadline_us: u64) -> u64 {
    interrupts::disable();
    let block = {
        let mut g = SCHED.lock();
        let s = g.as_mut().unwrap();
        let t = s.cur();
        let block = t.pending == 0 && deadline_us > time::now_us();
        if block {
            t.state = State::Sleeping(deadline_us);
        }
        block
    };
    if block {
        schedule();
    }
    with(|s| core::mem::take(&mut s.cur().pending))
}

/// Set notification bits of a thread (and wake it if it waits).
/// False if the thread has ended.
pub fn signal(tid: u32, bits: u64) -> bool {
    with(|s| s.signal(tid, bits))
}

