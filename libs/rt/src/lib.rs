//! The touchos userland runtime. Every server links this:
//!
//! ```ignore
//! #![no_std]
//! #![no_main]
//! touchos_rt::entry!(main);
//! fn main() { log::info!("hello"); }
//! ```
//!
//! It provides the process entry point (`_start`), syscall wrappers, a heap
//! that grows with `VM_ALLOC`, a `log` backend that writes to the kernel log,
//! and a panic handler that logs and exits.
//!
//! Only meaningful for the bare-metal target; on other targets (e.g. a
//! host-side `cargo check --workspace`) it compiles to nothing.

#![no_std]

#[cfg(target_os = "none")]
extern crate alloc;

pub use touchos_abi as abi;

// The atomic libcalls (__sync_*, __atomic_*_8) for every ARM program.
#[cfg(target_arch = "arm")]
extern crate touchos_atomics;

#[cfg(target_os = "none")]
pub mod caps;
#[cfg(target_os = "none")]
pub mod exec;
#[cfg(target_os = "none")]
mod heap;
#[cfg(target_os = "none")]
pub mod ipc;
#[cfg(target_os = "none")]
mod logger;
#[cfg(target_os = "none")]
pub mod rand;
#[cfg(target_os = "none")]
pub mod irq;
#[cfg(target_os = "none")]
pub mod shm;
#[cfg(target_os = "none")]
mod start;
#[cfg(target_os = "none")]
pub mod sys;
#[cfg(target_os = "none")]
pub mod time;

#[cfg(target_os = "none")]
pub use caps::cap;
#[cfg(target_os = "none")]
pub use exec::Local;
#[cfg(target_os = "none")]
pub use start::{arg, cmdline, flag};

/// Declare the process's `main` function.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        extern "Rust" fn __touchos_main() {
            let main: fn() = $main;
            main()
        }
    };
}
