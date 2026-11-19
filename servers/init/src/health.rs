//! Whether this copy of the system is healthy, and what follows from it:
//! the hardware watchdog gets signs of life while it is (so the machine
//! restarts if we hang, the kernel hangs, or a critical program gave up),
//! and a copy confirms itself or, on trial, goes back (the rule:
//! touchos_update::trial; the state record is the storage server's).

use alloc::vec::Vec;
use log::{error, info, warn};
use touchos_update::trial::{self, CONFIRM_AFTER_MS, Health, TRIAL_LIMIT_MS, Why};
use touchos_rt::shm::{Board, Shared};
use touchos_rt::{sys, time};

use crate::Opts;
use crate::supervise::{CRITICAL, Init, SECOND};

/// How often init shows the watchdog a sign of life (it restarts the
/// machine after about 60 s without one).
const PET_EVERY: u64 = 10 * SECOND;

pub struct Watchdog {
    pet_at: Option<u64>,
    /// `test_fail=init`: stop petting at this time.
    stop_at: Option<u64>,
}

impl Watchdog {
    pub fn new(system: u32, opts: &Opts, boot_time: u64) -> Watchdog {
        let armed = sys::watchdog(system, touchos_abi::watchdog::QUERY).unwrap_or(false);
        let test = opts.test_fail == Some("init");
        if armed && test {
            warn!("test_fail=init: no more signs of life to the watchdog after 20 s");
        }
        Watchdog { pet_at: armed.then(time::now_us), stop_at: test.then(|| boot_time + 20 * SECOND) }
    }

    pub fn deadline(&self) -> Option<u64> {
        self.pet_at
    }

    pub fn step(&mut self, init: &Init, system: u32, now: u64) {
        if !self.pet_at.is_some_and(|t| t <= now) {
            return;
        }
        let gave_up = init.children.iter().find(|c| c.gave_up && CRITICAL.contains(&c.prog.name));
        if self.stop_at.is_some_and(|t| t <= now) {
            warn!("test_fail=init: not petting the watchdog any more");
            self.pet_at = None;
        } else if let Some(c) = gave_up {
            error!(
                "{} isn't running any more: no more signs of life to the watchdog, so it restarts the machine",
                c.prog.name
            );
            self.pet_at = None;
        } else {
            let _ = sys::watchdog(system, touchos_abi::watchdog::PET);
            self.pet_at = Some(now + PET_EVERY);
        }
    }
}

/// The copy's health checks: a copy on trial goes back at its first
/// failure (at once); the rest is checked every 5 s from CONFIRM_AFTER_MS
/// on.
pub struct Trial {
    /// The storage server, which keeps the state record (on trial or not
    /// is read at the first check: storage may still be starting at boot,
    /// and the boot stage limits a trial's starts). None without a NAND.
    nand: Option<u32>,
    nand_asked: bool,
    slot: Option<char>,
    on_trial: bool,
    check_at: Option<u64>,
    boot_time: u64,
    test_fail: Option<&'static str>,
    /// `setup_ap=force`: a test boot too.
    setup_ap_force: bool,
    needs_net: bool,
    net: Option<Shared<Board>>,
    net_seq: u64,
    net_buf: Vec<u8>,
    net_status: touchos_proto::net::Status,
}

impl Trial {
    pub fn new(opts: &Opts, init: &Init, net_state: u32, nand: Option<u32>, boot_time: u64) -> Trial {
        Trial {
            nand,
            nand_asked: false,
            slot: opts.slot,
            on_trial: false,
            check_at: opts.slot.filter(|_| !opts.selftest).map(|_| boot_time + CONFIRM_AFTER_MS * 1000),
            boot_time,
            test_fail: opts.test_fail,
            setup_ap_force: opts.setup_ap_force,
            needs_net: init.children.iter().any(|c| c.prog.name == "net"),
            net: touchos_rt::shm::board_of(net_state, false),
            net_seq: 0,
            net_buf: Vec::new(),
            net_status: Default::default(),
        }
    }

    pub fn deadline(&self) -> Option<u64> {
        self.check_at
    }

    pub fn step(&mut self, failures: u32, system: u32, now: u64) {
        let due = self.check_at.is_some_and(|t| t <= now);
        let Some(s) = self.slot.filter(|_| due || (self.on_trial && failures > 0)) else {
            return;
        };
        if let Some(st) = self.net.as_ref().and_then(|n| n.read_as(&mut self.net_seq, &mut self.net_buf)) {
            self.net_status = st;
        }
        // The setup network's address doesn't count: it reaches only the
        // phone being set up, so a copy that ends up there (no Wi-Fi it
        // can join, no cable) goes back on trial.
        let has_net = self.net_status.ipv4.is_some()
            && self.net_status.setup_ap.is_none()
            && self.test_fail != Some("nonet");
        if let Some(storage) = self.nand.filter(|_| due && !self.nand_asked) {
            self.nand_asked = true;
            match nand_state(storage) {
                Some(st) => {
                    self.on_trial = st.trial == Some(s);
                    info!(
                        "copy {s}: active copy {}, {} (start {})",
                        st.active,
                        match st.trial {
                            Some(t) if t == s => "this copy is on trial",
                            Some(_) => "another copy on trial",
                            None => "nothing on trial",
                        },
                        st.starts
                    );
                }
                None => warn!("copy {s}: can't read the NAND's state record"),
            }
        }
        let elapsed_ms = (now - self.boot_time) / 1000;
        match trial::health(self.on_trial, failures, has_net || !self.needs_net, elapsed_ms) {
            Health::GoBack(Why::Failure) => {
                error!("copy {s} failed on trial ({failures} program failures): restarting into the active copy");
                self.give_up(s);
                restart(system);
            }
            Health::GoBack(Why::NoNetwork) => {
                error!(
                    "copy {s} isn't healthy after {} s (no network address): restarting into the active copy",
                    TRIAL_LIMIT_MS / 1000
                );
                self.give_up(s);
                restart(system);
            }
            Health::Confirm => {
                self.check_at = None;
                self.on_trial = false;
                match self.test_fail {
                    Some(t) => warn!("copy {s}: a test boot (test_fail={t}) is never made the active copy"),
                    None if self.setup_ap_force => {
                        warn!("copy {s}: a test boot (setup_ap=force) is never made the active copy")
                    }
                    None => match self.nand {
                        Some(storage) => nand_verdict(storage, s, true),
                        None => info!("copy {s} is healthy (no storage to record it in)"),
                    },
                }
            }
            Health::Stop => {
                self.check_at = None;
                warn!("copy {s}: {failures} program failures since boot; not confirming it");
            }
            Health::Wait => {
                if due {
                    self.check_at = Some(now + 5 * SECOND);
                }
            }
        }
    }
}

impl Trial {
    /// A failed trial is recorded, so the boot stage starts the active
    /// copy next.
    fn give_up(&self, slot: char) {
        if let Some(storage) = self.nand {
            nand_verdict(storage, slot, false);
        }
    }
}

/// The NAND's state record, through the storage server.
fn nand_state(storage: u32) -> Option<touchos_proto::storage::CopyState> {
    use touchos_proto::storage as sp;
    let m = touchos_rt::ipc::call(storage, sp::COPY_STATE, &[], &[]).ok()?;
    (m.label == sp::OK).then(|| touchos_proto::decode(m.data())).flatten()
}

/// Tell the storage server this copy is healthy (it becomes the active
/// copy, its start count 0) or its trial failed.
fn nand_verdict(storage: u32, slot: char, healthy: bool) {
    use touchos_proto::storage as sp;
    let v = touchos_proto::encode(&sp::Verdict { slot, healthy });
    match touchos_rt::ipc::call(storage, sp::COPY_VERDICT, &v, &[]) {
        Ok(m) if m.label == sp::OK && healthy => info!(
            "copy {slot} is healthy (no failures for {} s, network up): it is the active copy{}",
            CONFIRM_AFTER_MS / 1000,
            if m.data() == [1] { " now" } else { "" }
        ),
        Ok(m) if m.label == sp::OK => info!("copy {slot}: its trial is over; the active copy starts next"),
        Ok(m) => error!("copy {slot}: storage answered {} {}", m.label, alloc::string::String::from_utf8_lossy(m.data())),
        Err(e) => error!("copy {slot}: storage not reachable ({e:?})"),
    }
}

/// Restart (after a moment, so the log line reaches the network).
fn restart(system: u32) -> ! {
    time::sleep_ms(1000);
    let _ = sys::reboot(system);
    error!("restart failed; stopping, so the watchdog restarts the machine");
    loop {
        let _ = sys::wait(u64::MAX);
    }
}
