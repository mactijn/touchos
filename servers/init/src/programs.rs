//! What init starts, and with which capabilities: one builder per
//! program, and the whole table in start order (`table`). The capabilities
//! are minted here, at run time, so the table is code, not data.

use alloc::string::String;
use alloc::vec::Vec;
use touchos_abi::rights;
use touchos_rt::{arg, cap, caps::Mapping, sys};

use crate::Opts;
use crate::board::{self, Board, Wifi};

#[derive(Clone, Copy, PartialEq)]
pub enum Policy {
    /// Run once; don't restart.
    Once,
    /// Restart when it ends with an error or a crash.
    Restart,
}

pub struct Program {
    pub name: &'static str,
    pub file: &'static str,
    pub args: &'static str,
    pub policy: Policy,
    pub caps: Vec<(&'static str, u32)>,
    /// Scheduling priority (0 = default; lower runs first).
    pub priority: u32,
}

/// The programs in start order; the self-tests (with `selftest`) from
/// `tests_from` on.
pub struct Table {
    pub programs: Vec<Program>,
    pub tests_from: usize,
    /// The network status board (init reads it to judge the copy's health).
    pub net_state: u32,
    /// On the NAND: storage (badge UPDATE), where init reads whether this
    /// copy is on trial and settles it (see `health`).
    pub nand_storage: Option<u32>,
}

pub fn table(board: &'static Board, opts: &Opts, system: u32, initrd: &Mapping, files: &[(String, u64, u64)]) -> Table {
    // The config server: init keeps the endpoint, so a restarted server
    // serves the same capabilities; clients get badged copies.
    let config_ep = sys::ep_create().expect("ep_create");
    let mut config_caps = alloc::vec![("ep", config_ep)];
    if let Some(file) = cap("config") {
        config_caps.push(("file", file));
    }
    // The NAND, for storage and updates.
    let storage_ep = sys::ep_create().expect("ep_create");
    let storage_dev = board.nand.and_then(|page| board::regs("storage", page)).map(|c| ("regs", c));
    let nand = storage_dev.is_some();
    if storage_dev.is_some() {
        config_caps.push((
            "storage",
            mint(
                storage_ep,
                rights::CALL,
                touchos_proto::storage::badge::CONFIG,
            ),
        ));
    }
    // Which copy of the system this is (the boot stage puts `slot=A|B`,
    // the build `build=<git describe>` on the command line), for the
    // settings page.
    let (slot, build) = (opts.slot_arg, opts.build);
    let web_args: &'static str =
        alloc::boxed::Box::leak(alloc::format!("slot={slot} build={build}").into_boxed_str());
    let storage_args: &'static str = alloc::boxed::Box::leak(
        alloc::format!(
            "mode={} slot={}",
            // A selftest changes settings: they must not be saved for good.
            arg("storage").unwrap_or(if opts.selftest { "ro" } else { "rw" }),
            slot,
        )
        .into_boxed_str(),
    );
    let config = ConfigCaps {
        public: mint(
            config_ep,
            rights::CALL,
            touchos_config::proto::badge::PUBLIC,
        ),
        full: mint(config_ep, rights::CALL, touchos_config::proto::badge::FULL),
        admin: mint(config_ep, rights::CALL, touchos_config::proto::badge::ADMIN),
    };
    // The platform drivers (touch, accelerometer, buttons, RTC) talk to
    // the dashboard through a ring that init creates.
    let touch_ring = if board.platform.is_some() {
        sys::mem_create(input_ring_size(), 0).ok()
    } else {
        None
    };
    let shared = Shared::new();
    let net_ep = sys::ep_create().expect("ep_create");
    let ui_ep = sys::ep_create().expect("ep_create");
    let mut programs = alloc::vec![];
    // Updates from the settings page: the update server writes the other
    // copy of the system (through storage) and publishes its progress on a
    // board the web server reads.
    let mut update_for_web: Vec<(&'static str, u32)> = Vec::new();
    if let Some(dev) = storage_dev {
        programs.push(Program {
            name: "storage",
            file: "storage",
            args: storage_args,
            policy: Policy::Restart,
            caps: alloc::vec![("ep", storage_ep), dev],
            priority: 0,
        });
    }
    // Updates: the NAND's system copies and state record.
    if storage_dev.is_some() {
        let update_ep = sys::ep_create().expect("ep_create");
        let state = sys::mem_create(touchos_proto::update::STATE_BYTES, 0).expect("mem_create");
        update_for_web.push(("update", mint(update_ep, rights::CALL, 0)));
        update_for_web.push(("update_state", mint(state, rights::MAP_READ, 0)));
        programs.push(Program {
            name: "update",
            file: "update",
            args: web_args,
            policy: Policy::Restart,
            caps: alloc::vec![
                ("ep", update_ep),
                (
                    "storage",
                    mint(
                        storage_ep,
                        rights::CALL,
                        touchos_proto::storage::badge::UPDATE
                    )
                ),
                ("system", mint(system, rights::REBOOT, 0)),
                ("state", state),
            ],
            priority: 0,
        });
    }
    programs.extend([
        Program {
            name: "config",
            file: "config",
            args: "",
            policy: Policy::Restart,
            caps: config_caps,
            priority: 0,
        },
        Program {
            name: "ui",
            file: "ui",
            args: "",
            policy: Policy::Restart,
            caps: {
                let mut c = ui_caps(board, &config, touch_ring, &shared, system);
                c.push(("ep", ui_ep));
                c
            },
            priority: 3,
        },
    ]);
    // The network stack, then the board's devices.
    programs.extend([
        Program {
            name: "net",
            file: "net",
            args: "",
            policy: Policy::Restart,
            caps: net_caps(net_ep, &config, &shared, system),
            priority: 3,
        },
        Program {
            name: "ha",
            file: "ha",
            args: "",
            policy: Policy::Restart,
            caps: alloc::vec![
                ("net", mint(net_ep, rights::CALL, 0)),
                ("config", config.full),
                ("state", shared.ha_state),
                ("calls", shared.ha_calls),
            ],
            priority: 0,
        },
        Program {
            name: "web",
            file: "web",
            args: web_args,
            policy: Policy::Restart,
            caps: {
                let mut c = alloc::vec![
                    ("net", mint(net_ep, rights::CALL, 0)),
                    ("config", config.admin),
                    ("ui", mint(ui_ep, rights::CALL, 0)),
                    ("ha_state", mint(shared.ha_state, rights::MAP_READ, 0)),
                    (
                        "system",
                        mint(system, rights::KLOG_READ | rights::REBOOT, 0)
                    ),
                ];
                c.extend(update_for_web.iter().copied());
                c
            },
            priority: 0,
        },
        Program {
            name: "hello",
            file: "hello",
            args: "",
            policy: Policy::Once,
            caps: Vec::new(),
            priority: 0,
        },
    ]);
    programs.extend(platform(board, touch_ring, system));
    programs.extend(usb_ehci(board));
    programs.extend(wifi_rt5370(board, &config, &shared, initrd, files));
    programs.extend(fec(board, &shared));
    programs.extend(boiler(board));
    programs.extend(zwave(board));
    let tests_from = programs.len();
    if opts.selftest {
        programs.extend(crate::selftest::programs(board, &config, system));
    }
    // `test_fail=usb`: the usb server makes a driver fail once (its
    // recovery is the test).
    if opts.test_fail == Some("usb") {
        for p in programs.iter_mut().filter(|p| p.name == "usb") {
            p.args = alloc::boxed::Box::leak(alloc::format!("{} test_fail=usb", p.args).into_boxed_str());
        }
    }
    let nand_storage = nand.then(|| mint(storage_ep, rights::CALL, touchos_proto::storage::badge::UPDATE));
    Table { programs, tests_from, net_state: shared.net_state, nand_storage }
}

/// The dashboard: the framebuffer, the display engine (backlight), the
/// shared memory with ha, net, update and the input drivers, and settings.
fn ui_caps(
    board: &Board,
    config: &ConfigCaps,
    touch_ring: Option<u32>,
    sh: &Shared,
    system: u32,
) -> Vec<(&'static str, u32)> {
    let mut caps = alloc::vec![
        ("config", config.public),
        // The power button's menu: restart and switch off.
        ("system", mint(system, rights::REBOOT | rights::POWEROFF, 0)),
        ("ha_state", mint(sh.ha_state, rights::MAP_READ, 0)),
        ("ha_calls", sh.ha_calls),
        ("net_state", mint(sh.net_state, rights::MAP_READ, 0)),
    ];
    if let Some(r) = touch_ring {
        caps.push(("touch", r));
    }
    caps.push(("input", sh.input));
    if let Some(c) = cap("framebuffer") {
        caps.push(("framebuffer", c));
    }
    if let Some(c) = board.backlight.and_then(|p| board::regs("the backlight", p)) {
        caps.push(("backlight", c));
    }
    caps
}

/// A driver: restarted when it fails, and scheduled ahead of the services.
fn driver(name: &'static str, args: &'static str, caps: Vec<(&'static str, u32)>) -> Program {
    Program {
        name,
        file: name,
        args,
        policy: Policy::Restart,
        caps,
        priority: 3,
    }
}

/// The board's EHCI, when the usb server drives it (enumeration only).
fn usb_ehci(board: &Board) -> Option<Program> {
    let ehci = board.usb?;
    let regs = board::regs("USB", ehci.page)?;
    let mut caps = alloc::vec![("regs", regs)];
    caps.extend(board::irq("USB", ehci.irq).map(|c| ("irq", c)));
    Some(driver("usb", ehci.args, caps))
}

/// The RT5370 with its EHCI, and its firmware from the initrd.
fn wifi_rt5370(
    board: &Board,
    config: &ConfigCaps,
    sh: &Shared,
    initrd: &Mapping,
    files: &[(String, u64, u64)],
) -> Option<Program> {
    let Wifi::Rt5370 { ehci, firmware_file } = board.wifi else { return None };
    let regs = board::regs("USB", ehci.page)?;
    let mut caps = alloc::vec![("config", config.full), ("regs", regs)];
    caps.extend(board::irq("USB", ehci.irq).map(|c| ("irq", c)));
    caps.extend(driver_link(sh.wlan_link));
    caps.extend(initrd_file(initrd, files, firmware_file).map(|fw| ("firmware", fw)));
    Some(driver("wifi", ehci.args, caps))
}

/// The I²C devices: input to the
/// dashboard, and the clock from the RTC where there is one. The server
/// picks its drivers by the board's name.
fn platform(board: &Board, touch_ring: Option<u32>, system: u32) -> Option<Program> {
    let p = board.platform.as_ref()?;
    let mut caps = alloc::vec![("input", touch_ring?)];
    if p.sets_time {
        caps.push(("system", mint(system, rights::TIME_SET, 0)));
    }
    for &(name, phys) in p.mmio {
        caps.extend(board::regs(name, phys).map(|c| (name, c)));
    }
    for &(name, line) in p.irqs {
        caps.extend(board::irq(name, line).map(|c| (name, c)));
    }
    let args = alloc::format!("board={}", board.name);
    Some(driver("platform", alloc::boxed::Box::leak(args.into_boxed_str()), caps))
}

/// The Toon's boiler link: UART2 and its interrupt, received only.
fn boiler(board: &Board) -> Option<Program> {
    let (page, line) = board.boiler?;
    let mut caps = alloc::vec![("uart", board::regs("the boiler link", page)?)];
    caps.extend(board::irq("the boiler link", line).map(|c| ("irq", c)));
    Some(driver("boiler", "", caps))
}

/// The Toon's Z-Wave controller: UART3 and its interrupt.
fn zwave(board: &Board) -> Option<Program> {
    let (page, line) = board.zwave?;
    let mut caps = alloc::vec![("uart", board::regs("the Z-Wave controller", page)?)];
    caps.extend(board::irq("the Z-Wave controller", line).map(|c| ("irq", c)));
    // `zwave=pair`, `reset-pair` or `include` (a test switch): servers/zwave.
    let args = match touchos_rt::arg("zwave") {
        Some(m) => alloc::boxed::Box::leak(alloc::format!("zwave={m}").into_boxed_str()),
        None => "",
    };
    Some(driver("zwave", args, caps))
}

/// The ARM boards' Ethernet controller.
fn fec(board: &Board, sh: &Shared) -> Option<Program> {
    let (page, line) = board.fec?;
    let mut caps = driver_link(sh.wired_link).to_vec();
    caps.extend(board::regs("the FEC", page).map(|r| ("regs", r)));
    caps.extend(board::irq("the FEC", line).map(|r| ("irq", r)));
    Some(driver("fec", "", caps))
}

/// Shared memory between services that init creates (and keeps, so a
/// restarted service finds the same objects).
pub struct Shared {
    /// HA state, written by ha, read by the dashboard.
    ha_state: u32,
    /// Service calls from the dashboard to ha.
    ha_calls: u32,
    /// Network status, written by net, read by the dashboard.
    net_state: u32,
    /// USB keyboard/mouse events to the dashboard.
    input: u32,
    /// Frames and link state between each network driver and net.
    wired_link: [u32; 3],
    wlan_link: [u32; 3],
}

impl Shared {
    fn new() -> Shared {
        let mem = |len: u64| sys::mem_create(len, 0).expect("mem_create");
        let frames = touchos_proto::shm::ring_size(
            touchos_proto::net::FRAME_SLOTS,
            touchos_proto::net::FRAME_SLOT,
        ) as u64;
        let link = || [mem(frames), mem(frames), mem(touchos_proto::net::LINK_BYTES)];
        let (wired_link, wlan_link) = (link(), link());
        Shared {
            ha_state: mem(touchos_proto::ha::STATE_BYTES),
            ha_calls: mem(touchos_proto::shm::ring_size(
                touchos_proto::ha::CALL_SLOTS,
                touchos_proto::ha::CALL_SLOT,
            ) as u64),
            net_state: mem(touchos_proto::net::STATUS_BYTES),
            input: mem(input_ring_size()),
            wired_link,
            wlan_link,
        }
    }
}

/// Bytes for an input ring (events to the dashboard).
fn input_ring_size() -> u64 {
    use touchos_proto::input::{RING_SLOT, RING_SLOTS};
    touchos_proto::shm::ring_size(RING_SLOTS, RING_SLOT) as u64
}

/// A network driver's side of its link: rings and the state board.
fn driver_link(l: [u32; 3]) -> [(&'static str, u32); 3] {
    [("link_rx", l[0]), ("link_tx", l[1]), ("link_state", l[2])]
}

/// The network stack: the drivers' link rings, our endpoint, the status
/// board, and the clock and log rights of the system capability.
fn net_caps(ep: u32, config: &ConfigCaps, sh: &Shared, system: u32) -> Vec<(&'static str, u32)> {
    let mut caps = alloc::vec![
        ("ep", ep),
        ("config", config.public),
        ("status", sh.net_state),
        (
            "system",
            mint(system, rights::TIME_SET | rights::KLOG_READ, 0)
        ),
    ];
    caps.extend([
        ("wired_rx", sh.wired_link[0]),
        ("wired_tx", sh.wired_link[1]),
        ("wired_state", mint(sh.wired_link[2], rights::MAP_READ, 0)),
        ("wlan_rx", sh.wlan_link[0]),
        ("wlan_tx", sh.wlan_link[1]),
        ("wlan_state", mint(sh.wlan_link[2], rights::MAP_READ, 0)),
    ]);
    caps
}

/// Badged capabilities to the config server.
pub struct ConfigCaps {
    pub public: u32,
    pub full: u32,
    pub admin: u32,
}

pub fn mint(slot: u32, rights: u32, badge: usize) -> u32 {
    sys::cap_mint(slot, rights, badge).expect("cap_mint")
}

/// A data file of the initrd (firmware) as a memory object of its own,
/// read-only: a program gets just that file, not the whole initrd.
fn initrd_file(initrd: &Mapping, files: &[(String, u64, u64)], name: &str) -> Option<u32> {
    let &(_, off, len) = files.iter().find(|(n, _, _)| n == name)?;
    touchos_rt::caps::share(initrd.bytes().get(off as usize..(off + len) as usize)?)
}

