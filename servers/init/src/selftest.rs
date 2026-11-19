//! The `selftest` boot: the test programs (servers/hello, servers/selftest)
//! with what they need, the checks init does itself, and when the run is
//! over. Also the `test_fail=crash:NAME` hook.

use alloc::vec::Vec;
use log::{error, info, warn};
use touchos_abi::{Error, rights, status};
use touchos_rt::{cap, sys};

use crate::board::{self, Board};
use crate::programs::{ConfigCaps, Policy, Program, mint};
use crate::supervise::{Child, Init, SECOND};

/// What init itself checks while the tests run: programs it stops ("forever"
/// checks the status; the UI must come back, and the console show
/// meanwhile), and the end line.
pub struct Tests {
    kills: Vec<(usize, u64)>,
    from: usize,
    done: bool,
}

impl Tests {
    /// The test programs are init's children from `from` on.
    pub fn new(enabled: bool, from: usize) -> Tests {
        Tests { kills: Vec::new(), from, done: !enabled }
    }

    /// Child `i` was started at `now`.
    pub fn started(&mut self, c: &Child, i: usize, now: u64) {
        if self.done {
            return;
        }
        if c.prog.name == "forever" {
            self.kills.push((i, now + SECOND));
        }
        // (Only if it runs: a board may not have the UI yet.)
        if c.prog.name == "ui" && c.proc.is_some() {
            self.kills.push((i, now + 4 * SECOND));
        }
    }

    pub fn deadline(&self) -> Option<u64> {
        self.kills.iter().map(|&(_, t)| t).min()
    }

    pub fn step(&mut self, init: &Init, now: u64) {
        self.kills.retain(|&(i, t)| {
            if t <= now {
                check_kill(&init.children[i]);
                false
            } else {
                true
            }
        });
        if !self.done && self.kills.is_empty() && tests_finished(&init.children[self.from.min(init.children.len())..]) {
            self.done = true;
            // `cargo xtask selftest` stops the machine at this line.
            info!("selftest: finished");
        }
    }
}

/// `test_fail=crash:NAME`: kill program NAME after 20 s, and 2 s after
/// each of its restarts.
pub struct CrashTest {
    target: Option<&'static str>,
    at: Option<u64>,
}

impl CrashTest {
    pub fn new(target: Option<&'static str>, boot_time: u64) -> CrashTest {
        if let Some(name) = target {
            warn!("test_fail=crash:{name}: crashing it after 20 s, and again after each restart");
        }
        CrashTest { target, at: target.map(|_| boot_time + 20 * SECOND) }
    }

    pub fn deadline(&self) -> Option<u64> {
        self.at
    }

    /// Program `name` was started again at `now`.
    pub fn restarted(&mut self, name: &str, now: u64) {
        if self.target == Some(name) {
            self.at = Some(now + 2 * SECOND);
        }
    }

    pub fn step(&mut self, init: &Init, now: u64) {
        if self.at.is_some_and(|t| t <= now) {
            self.at = None;
            if let Some(c) = init.children.iter().find(|c| Some(c.prog.name) == self.target)
                && let Some(p) = c.proc
            {
                let _ = sys::proc_kill(p);
            }
        }
    }
}

pub fn programs(board: &Board, config: &ConfigCaps, system: u32) -> Vec<Program> {
    warn!("selftest: starting test processes");
    let mut v = Vec::new();
    for (name, args) in [
        ("fpu-a", "fpu 1"),
        ("fpu-b", "fpu 2"),
        ("sleep", "sleep 500"),
        ("vm", board.vm_test),
        ("crash", "crash"),
        ("bad-syscall", "bad-syscall"),
    ] {
        v.push(Program {
            name,
            file: "hello",
            args,
            policy: Policy::Once,
            caps: Vec::new(),
            priority: 0,
        });
    }
    // IPC: pong serves an endpoint; ping calls it through a badged,
    // call-only copy.
    let ep = sys::ep_create().expect("ep_create");
    let ep_call = sys::cap_mint(ep, rights::CALL, 42).expect("cap_mint");
    v.push(Program {
        name: "random",
        file: "selftest",
        args: "random",
        policy: Policy::Once,
        caps: alloc::vec![],
        priority: 0,
    });
    // Two processes adding to the same counters (atomics under preemption).
    let shared = sys::mem_create(4096, 0).expect("mem_create");
    for name in ["atomics-a", "atomics-b"] {
        v.push(Program {
            name,
            file: "selftest",
            args: "atomics 300000",
            policy: Policy::Once,
            caps: alloc::vec![("shared", shared)],
            priority: 0,
        });
    }
    v.push(Program {
        name: "pong",
        file: "selftest",
        args: "pong",
        policy: Policy::Restart,
        caps: alloc::vec![("ep", ep)],
        priority: 0,
    });
    v.push(Program {
        name: "ping",
        file: "selftest",
        args: "ping",
        policy: Policy::Once,
        caps: alloc::vec![("ep", ep_call)],
        priority: 0,
    });
    // Device access: a hardware counter's registers, but never RAM (our
    // own memory is RAM on any machine).
    let root = cap("mmio_root").expect("mmio_root");
    let ram = sys::mem_create(4096, touchos_abi::mem::CONTIGUOUS)
        .and_then(|m| sys::mem_phys(m, 0))
        .expect("mem_phys");
    match sys::mmio_derive(root, ram, 0x1000) {
        Err(Error::NoRights) => info!("PASS: MMIO capability for RAM refused"),
        r => error!("FAIL: MMIO capability for RAM: {r:?}"),
    }
    let (phys, args) = board.counter;
    if let Some(counter) = board::regs("the counter", phys) {
        v.push(Program {
            name: "mmio",
            file: "selftest",
            args,
            policy: Policy::Once,
            caps: alloc::vec![("counter", counter)],
            priority: 0,
        });
    }
    // A system capability with another right than the watchdog's.
    v.push(Program {
        name: "rights",
        file: "selftest",
        args: "rights",
        policy: Policy::Once,
        caps: alloc::vec![("system", mint(system, rights::TIME_SET, 0))],
        priority: 0,
    });
    // A program that asks for restarts (exit::RESTART) twice.
    v.push(Program {
        name: "restart",
        file: "selftest",
        args: "restart",
        policy: Policy::Restart,
        caps: alloc::vec![("shared", sys::mem_create(4096, 0).expect("mem_create"))],
        priority: 0,
    });
    // Malformed syscall input: a bad ELF image, an empty log buffer; and
    // the kernel's STATS report.
    v.push(Program {
        name: "kernel-input",
        file: "selftest",
        args: "kernel-input",
        policy: Policy::Once,
        caps: alloc::vec![("system", mint(system, rights::KLOG_READ | rights::STATS, 0))],
        priority: 0,
    });
    v.push(Program {
        name: "cfg",
        file: "selftest",
        args: "cfg",
        policy: Policy::Once,
        caps: alloc::vec![
            ("config_pub", config.public),
            ("config_full", config.full),
            ("config_admin", config.admin),
        ],
        priority: 0,
    });
    // Restart with backoff, then give up.
    v.push(Program {
        name: "crashy",
        file: "selftest",
        args: "crashy",
        policy: Policy::Restart,
        caps: Vec::new(),
        priority: 0,
    });
    // Stopped by init after a second.
    v.push(Program {
        name: "forever",
        file: "hello",
        args: "spin 1000000",
        policy: Policy::Once,
        caps: Vec::new(),
        priority: 0,
    });
    v
}

/// Test programs that serve the others and run until the machine stops.
const TEST_SERVERS: &[&str] = &["pong"];

/// Whether every test program has ended for good: finished, crashed
/// without a restart to come, or given up on.
fn tests_finished(tests: &[Child]) -> bool {
    tests
        .iter()
        .all(|c| TEST_SERVERS.contains(&c.prog.name) || (c.proc.is_none() && c.restart_at.is_none()))
}

fn check_kill(c: &Child) {
    let Some(p) = c.proc else {
        error!("FAIL: {} already ended", c.prog.name);
        return;
    };
    let _ = sys::proc_kill(p);
    match sys::proc_status(p) {
        Ok((status::KILLED, status::KILLED_BY_REQUEST)) => info!("PASS: {} stopped", c.prog.name),
        r => error!("FAIL: status after kill: {r:?}"),
    }
}
