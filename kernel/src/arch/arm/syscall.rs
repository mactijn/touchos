//! The trap frame: the registers of an interrupted thread as the exception
//! handlers in `vectors.rs` save them (syscalls, IRQs, faults), and the
//! syscall arguments and results in it.

/// r0-r12, the interrupted mode's sp and lr (the user's, or for a trap
/// from the kernel the kernel's), the return address, the saved CPSR, and
/// padding to 72 bytes (see `vectors.rs`). Also the frame of IRQs and
/// faults.
#[repr(C)]
pub struct SyscallFrame {
    pub r: [u32; 13],
    pub sp_usr: u32,
    pub lr_usr: u32,
    pub pc: u32,
    pub spsr: u32,
    pub _pad: u32,
}

const _: () = assert!(core::mem::size_of::<SyscallFrame>() == 72);

/// What generic code (`sys::dispatch`) uses: number in r7, arguments in
/// r0-r3, results in r0 and r1.
impl SyscallFrame {
    pub fn number(&self) -> u64 {
        self.r[7] as u64
    }
    pub fn args(&self) -> [u64; 4] {
        [self.r[0] as u64, self.r[1] as u64, self.r[2] as u64, self.r[3] as u64]
    }
    pub fn set_result(&mut self, first: u64, second: u64) {
        self.r[0] = first as u32;
        self.r[1] = second as u32;
    }
    pub fn set_error(&mut self, raw: u64) {
        self.r[0] = raw as u32;
    }
}
