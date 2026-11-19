//! Running the programs: start them, notice when they end, restart them
//! (at once when they ask for it, after a backoff when they failed), and
//! give up on one that keeps failing.

use alloc::string::String;
use alloc::vec::Vec;
use log::{error, info, warn};
use touchos_abi::{CapEntry, MAX_BOOT_CAPS, SpawnArgs, status};
use touchos_rt::sys;

use crate::programs::{Policy, Program};

pub struct Child {
    pub prog: Program,
    pub bit: usize,
    pub proc: Option<u32>,
    /// Times of recent crashes (µs).
    crashes: Vec<u64>,
    pub restart_at: Option<u64>,
    /// Times of recent restarts it asked for (exit::RESTART, µs).
    restarts: Vec<u64>,
    /// Crashed too often: not restarted any more.
    pub gave_up: bool,
    /// Since boot, for the metrics: times it was started again, and
    /// times it failed.
    pub restarts_total: u32,
    pub crashes_total: u32,
    /// How it ended last ("exit", "crash" or "killed") and the exit code
    /// or CPU exception.
    pub last_exit: Option<(&'static str, u32)>,
}

/// Programs the panel can't do without: if one of them gives up, init stops
/// petting the watchdog, so the machine restarts.
pub const CRITICAL: &[&str] = &["config", "storage", "net", "wifi", "ui", "platform", "web"];

pub const SECOND: u64 = 1_000_000;
const GIVE_UP_CRASHES: usize = 5;
const GIVE_UP_WINDOW: u64 = 60 * SECOND;
/// Restarts a program may ask for (exit::RESTART) within GIVE_UP_WINDOW
/// before they count as failures.
const FREE_RESTARTS: usize = 2;

pub struct Init {
    initrd_cap: u32,
    files: Vec<(String, u64, u64)>,
    pub children: Vec<Child>,
    /// Programs that crashed (or failed) since boot.
    pub failures: u32,
    /// `test_fail=crash:NAME`: init's kills of this program count as crashes.
    test_crash: Option<&'static str>,
}


impl Init {
    /// Each program's death is one notification bit: at most usize::BITS
    /// (32 on ARM) of them.
    pub fn new(mut programs: Vec<Program>, initrd_cap: u32, files: Vec<(String, u64, u64)>, test_crash: Option<&'static str>) -> Init {
        if programs.len() > usize::BITS as usize {
            let extra: Vec<&str> = programs[usize::BITS as usize..].iter().map(|p| p.name).collect();
            error!("{} programs, but only {} notification bits: not starting {extra:?}", programs.len(), usize::BITS);
            programs.truncate(usize::BITS as usize);
        }
        Init {
            initrd_cap,
            files,
            failures: 0,
            test_crash,
            children: programs
                .into_iter()
                .enumerate()
                .map(|(i, prog)| Child {
                    prog,
                    bit: 1 << i,
                    proc: None,
                    crashes: Vec::new(),
                    restart_at: None,
                    restarts: Vec::new(),
                    gave_up: false,
                    restarts_total: 0,
                    crashes_total: 0,
                    last_exit: None,
                })
                .collect(),
        }
    }

    /// The next restart due.
    pub fn deadline(&self) -> Option<u64> {
        self.children.iter().filter_map(|c| c.restart_at).min()
    }

    /// After a wake-up: the programs that ended (their bits are in
    /// `bits`), and the restarts that are due; `restarted(name)` for each
    /// program started again.
    pub fn step(&mut self, bits: usize, now: u64, mut restarted: impl FnMut(&'static str)) {
        for i in 0..self.children.len() {
            if bits & self.children[i].bit != 0 {
                self.died(i, now);
            }
            if self.children[i].restart_at.is_some_and(|t| t <= now) {
                self.children[i].restart_at = None;
                self.children[i].restarts_total += 1;
                self.start(i);
                restarted(self.children[i].prog.name);
            }
        }
    }

    pub fn start(&mut self, i: usize) {
        let c = &mut self.children[i];
        let Some(&(_, off, len)) = self.files.iter().find(|(n, _, _)| n == c.prog.file) else {
            error!("no program \"{}\" in the initrd", c.prog.file);
            return;
        };
        let mut args: SpawnArgs = unsafe { core::mem::zeroed() };
        let n = c.prog.name.len().min(16);
        args.name[..n].copy_from_slice(&c.prog.name.as_bytes()[..n]);
        let a = c.prog.args.len().min(args.cmdline.len());
        args.cmdline[..a].copy_from_slice(&c.prog.args.as_bytes()[..a]);
        args.cmdline_len = a as u32;
        args.priority = c.prog.priority;
        args.death_notification = touchos_abi::slot::SELF_NOTIFICATION;
        args.death_bits = c.bit;
        for (k, (name, slot)) in c.prog.caps.iter().take(MAX_BOOT_CAPS).enumerate() {
            let mut e = CapEntry {
                name: [0; 16],
                slot: *slot,
                kind: 0,
            };
            let n = name.len().min(16);
            e.name[..n].copy_from_slice(&name.as_bytes()[..n]);
            args.caps[k] = e;
            args.ncaps = k as u32 + 1;
        }
        match sys::proc_spawn(self.initrd_cap, off, len, &args) {
            Ok((p, _pid)) => c.proc = Some(p),
            Err(e) => error!("can't start {}: {e:?}", c.prog.name),
        }
    }

    fn died(&mut self, i: usize, now: u64) {
        let c = &mut self.children[i];
        let Some(p) = c.proc.take() else { return };
        let (st, code) = sys::proc_status(p).unwrap_or((status::KILLED, 0));
        let _ = sys::cap_drop(p);
        let ok = st == status::EXITED && code == 0;
        let test_crash = self.test_crash == Some(c.prog.name);
        c.last_exit = Some(match (st, code) {
            (status::EXITED, code) => ("exit", code as u32),
            (_, status::KILLED_BY_REQUEST) if !test_crash => ("killed", 0),
            (_, status::KILLED_BY_REQUEST) => ("crash", 0),
            (_, vector) => ("crash", vector as u32),
        });
        // A restart it asked for: at once, and not a failure (a few times).
        if st == status::EXITED && code == touchos_abi::exit::RESTART && c.prog.policy == Policy::Restart {
            c.restarts.retain(|&t| now - t < GIVE_UP_WINDOW);
            c.restarts.push(now);
            if c.restarts.len() <= FREE_RESTARTS {
                info!("{} asked to be restarted", c.prog.name);
                c.restart_at = Some(now);
                return;
            }
            warn!("{} asked to be restarted {} times within {} s: counting it as a failure", c.prog.name, c.restarts.len(), GIVE_UP_WINDOW / SECOND);
        }
        match (st, code) {
            (status::EXITED, 0) => info!("{} finished", c.prog.name),
            (status::EXITED, code) => warn!("{} exited with code {code}", c.prog.name),
            (_, status::KILLED_BY_REQUEST) if test_crash => {
                warn!("{} crashed (test_fail=crash)", c.prog.name)
            }
            (_, status::KILLED_BY_REQUEST) => info!("{} was stopped", c.prog.name),
            (_, vector) => warn!("{} crashed (CPU exception {vector})", c.prog.name),
        }
        if c.prog.policy == Policy::Once || ok {
            return;
        }
        self.failures += 1;
        c.crashes_total += 1;
        c.crashes.retain(|&t| now - t < GIVE_UP_WINDOW);
        c.crashes.push(now);
        if c.crashes.len() >= GIVE_UP_CRASHES {
            c.gave_up = true;
            error!(
                "{} failed {} times within {} s; not restarting it",
                c.prog.name,
                c.crashes.len(),
                GIVE_UP_WINDOW / SECOND
            );
            return;
        }
        let backoff = (SECOND << (c.crashes.len() - 1)).min(60 * SECOND);
        info!("restarting {} in {} s", c.prog.name, backoff / SECOND);
        c.restart_at = Some(now + backoff);
    }
}


impl Init {
    /// What init publishes about the programs (its metrics board).
    pub fn metrics(&self) -> touchos_proto::metrics::Metrics {
        let mut m = touchos_proto::metrics::Metrics::new();
        for c in &self.children {
            let l = [("server", c.prog.name)];
            m.with("touchos_server_up", &l, c.proc.is_some() as i64)
                .with("touchos_server_gave_up", &l, c.gave_up as i64)
                .with("touchos_server_restarts_total", &l, c.restarts_total as i64)
                .with("touchos_server_crashes_total", &l, c.crashes_total as i64);
            if let Some((how, code)) = c.last_exit {
                m.with("touchos_server_last_exit", &[("server", c.prog.name), ("how", how)], code as i64);
            }
        }
        m
    }
}
