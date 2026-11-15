//! Thread switching: each thread has its own kernel (SVC) stack. Switching
//! saves the callee-saved registers and the CPSR on the old stack, stores
//! sp, and loads the new thread's sp. A thread that was interrupted in user
//! mode has its trap frame (`vectors.rs`) further up its stack.

/// SVC mode, IRQ and FIQ masked: the CPSR a new thread starts with.
const CPSR_SVC_MASKED: u32 = 0xd3;
/// User mode, interrupts on.
const CPSR_USER: u32 = 0x10;

/// Switch from the current thread to the one whose saved sp is `next`. The
/// current thread's sp is written to `*save`. Returns when another thread
/// switches back to us.
///
/// # Safety
/// Interrupts must be off; `next` must come from `switch` or a `new_*_stack`.
#[unsafe(naked)]
pub unsafe extern "C" fn switch(save: *mut u64, next: u64) {
    // AAPCS: r0 = save, r2:r3 = next (a 64-bit argument takes an even pair).
    core::arch::naked_asm!(
        "mrs r12, cpsr",
        "push {{r4-r12, lr}}",
        "str sp, [r0]",
        "mov r1, #0",
        "str r1, [r0, #4]",
        "mov sp, r2",
        "pop {{r4-r12, lr}}",
        "msr cpsr_c, r12",
        "bx lr",
    );
}

/// Write `words` just below `top`; returns the address of the first.
fn push(top: u64, words: &[u32]) -> u64 {
    let start = top - (words.len() as u64) * 4;
    let p = start as usize as *mut u32;
    for (i, w) in words.iter().enumerate() {
        unsafe { p.add(i).write(*w) };
    }
    start
}

/// What `switch` pops: r4-r12 (r12 = the CPSR) and lr.
fn switch_frame(lr: u32) -> [u32; 10] {
    [0, 0, 0, 0, 0, 0, 0, 0, CPSR_SVC_MASKED, lr]
}

/// Prepare a kernel thread's stack (`top` 8-byte aligned) so that switching
/// to it calls `entry`.
pub fn new_kernel_stack(top: u64, entry: extern "C" fn() -> !) -> u64 {
    push(top, &switch_frame(entry as usize as u32))
}

/// Prepare a new thread's kernel stack so that switching to it enters user
/// mode at `pc` with stack `sp` and `r0 = arg`: a trap frame for
/// `arm_trap_return`, below it a switch frame that returns to `user_entry`.
pub fn new_user_stack(top: u64, pc: u64, sp: u64, arg: u64) -> u64 {
    let mut frame = [0u32; 18];
    frame[0] = arg as u32;
    frame[13] = sp as u32;
    frame[15] = pc as u32;
    frame[16] = CPSR_USER;
    let frame_at = push(top, &frame);
    push(frame_at, &switch_frame(user_entry as *const () as usize as u32))
}

/// First code of a user thread: the scheduler's per-thread start-up, then
/// down to user mode through its trap frame (sp points at it).
#[unsafe(naked)]
unsafe extern "C" fn user_entry() {
    core::arch::naked_asm!(
        "bl {start}",
        "b arm_trap_return",
        start = sym crate::task::thread_start,
    );
}
