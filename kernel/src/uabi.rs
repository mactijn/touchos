//! The ABI's word-sized constants as the `u64`s the kernel uses internally
//! for user addresses and syscall arguments: the kernel works in `u64` on
//! both architectures, and a 32-bit ARM word converts without loss.

macro_rules! as_u64 {
    ($m:ident: $($c:ident),*) => {
        #[allow(dead_code)]
        pub mod $m {
            $(pub const $c: u64 = touchos_abi::$m::$c as u64;)*
        }
    };
}

as_u64!(layout: NULL_GUARD_END, KINFO_VA, BOOTINFO_VA, COUNTER_VA, UTCB_VA, IMAGE_BASE, HEAP_BASE, SHARED_BASE, STACK_TOP, DEFAULT_STACK_SIZE, USER_END);
as_u64!(prot: READ, WRITE, EXEC);
as_u64!(status: RUNNING, EXITED, KILLED, KILLED_BY_REQUEST);
as_u64!(level: ERROR, WARN, INFO, DEBUG, TRACE);
as_u64!(reboot: DEFAULT);
as_u64!(watchdog: PET, STOP, QUERY);
as_u64!(mem: CONTIGUOUS);
