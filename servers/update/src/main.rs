//! The update server: installs a signed update (libs/update) from the
//! settings page into the copy of the system that isn't running, then
//! restarts into it. The storage server writes the copy into the NAND and
//! puts it on trial in the state record; the boot stage starts it, and
//! init confirms it once healthy (or it goes back).
//!
//! Nothing is written before the whole bundle checks out: the signature
//! against the public key this build carries (config/update-key.pub, see
//! `cargo xtask keygen`), every file's hash, and the list of files. The
//! storage server refuses to write the running copy whatever we ask.
//!
//! Capabilities: "ep" (the web server calls it), "storage" (badge UPDATE),
//! "system" (REBOOT), "state" (a board: progress for the settings page).
//! Arguments: `slot=A|B build=<id>` (the running copy).

#![no_std]
#![no_main]

extern crate alloc;

include!(concat!(env!("OUT_DIR"), "/key.rs"));

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use log::{error, info, warn};
use touchos_abi::Tag;
use touchos_proto::storage as sp;
use touchos_proto::update::{self as proto, Phase, State};
use touchos_rt::caps::{self, Mapping, share};
use touchos_rt::shm::{self, Board, Shared};
use touchos_rt::{arg, cap, exec, ipc, sys, time};
use touchos_update::nand_copy;
use touchos_update::trial::other;

touchos_rt::entry!(main);

static BUSY: AtomicBool = AtomicBool::new(false);
static STATE: spin::Mutex<Option<(State, Shared<Board>)>> = spin::Mutex::new(None);

/// Change the published state.
fn publish(f: impl FnOnce(&mut State)) {
    if let Some((st, board)) = STATE.lock().as_mut() {
        f(st);
        board.write_as(st);
    }
}

fn running() -> Option<char> {
    match arg("slot") {
        Some("A") => Some('A'),
        Some("B") => Some('B'),
        _ => None,
    }
}

/// The other copy's VERSION.TXT.
fn other_version(storage: u32, slot: char) -> Result<String, String> {
    let m = ipc::call(
        storage,
        sp::READ_FILE,
        format!("{slot}/{}", sp::VERSION_FILE).as_bytes(),
        &[],
    )
    .map_err(|e| format!("{e:?}"))?;
    if m.label != sp::OK {
        return Err(format!(
            "storage answered {} {}",
            m.label,
            String::from_utf8_lossy(m.data())
        ));
    }
    let mem = *m.caps().first().ok_or("no contents")?;
    let text = caps::read_shared(mem).ok_or("can't read the contents")?;
    Ok(String::from_utf8_lossy(&text).trim_end_matches('\0').trim().to_string())
}

/// Find out what the other copy is (storage may still be starting).
async fn learn_other(storage: u32, slot: char) {
    for attempt in 0..15 {
        match other_version(storage, slot) {
            Ok(v) => {
                info!("copy {slot}: {v}");
                publish(|s| {
                    if s.other.is_none() {
                        s.other = Some(v)
                    }
                });
                return;
            }
            Err(e) if attempt == 14 => warn!("can't read {slot}/VERSION.TXT: {e}"),
            Err(_) => exec::sleep_ms(2000).await,
        }
    }
}

/// Everything about a bundle except writing it: signature, hashes, files.
fn check(bundle: &[u8]) -> Result<touchos_update::Bundle<'_>, String> {
    let pk =
        PUBLIC_KEY.ok_or("this build accepts no updates (it was built without an update key)")?;
    let b = touchos_update::verify(bundle, &pk).map_err(|e| match e {
        touchos_update::Error::Signature => {
            String::from("the update isn't signed with this panel's key")
        }
        touchos_update::Error::Hash(f) => format!("file {f} in the update is damaged"),
        touchos_update::Error::Format(w) => format!("not a valid update file ({w})"),
    })?;
    nand_copy::check(&b)?;
    Ok(b)
}

/// Write the bundle into the other copy, then restart into it.
async fn install(map: Mapping, len: usize) {
    let r = install_inner(&map.bytes()[..len]).await;
    drop(map);
    if let Err(why) = r {
        error!("update failed: {why}");
        publish(|s| s.phase = Phase::Failed(why));
        BUSY.store(false, Ordering::Relaxed);
    }
}

async fn install_inner(bundle: &[u8]) -> Result<(), String> {
    let (Some(storage), Some(system)) = (cap("storage"), cap("system")) else {
        return Err("no access to storage or the restart".into());
    };
    let b = check(bundle)?;
    let slot = other(running().ok_or("don't know which copy is running")?);
    install_nand(&b, storage, system, slot).await
}

/// The storage server writes the copy (kernel, initrd, boot info, one
/// after the other in one piece of memory) and puts it on trial; then
/// restart into it.
async fn install_nand(b: &touchos_update::Bundle<'_>, storage: u32, system: u32, slot: char) -> Result<(), String> {
    let parts: Vec<&[u8]> = nand_copy::FILES.iter().map(|n| b.file(n).unwrap_or_default()).collect();
    let total: usize = parts.iter().map(|p| p.len()).sum();
    info!("installing build {} into copy {slot} ({} KiB)", b.build, total / 1024);
    publish(|s| s.phase = Phase::Writing { name: format!("copy {slot}"), done: 0, total: 1 });
    exec::sleep_ms(1).await; // let the board update be seen
    let mut all = Vec::with_capacity(total);
    for p in &parts {
        all.extend_from_slice(p);
    }
    let req = sp::CopyWrite {
        slot,
        build: b.build,
        kernel: parts[0].len() as u32,
        initrd: parts[1].len() as u32,
        bootinfo: parts[2].len() as u32,
    };
    let mem = share(&all).ok_or("out of memory")?;
    drop(all);
    let t0 = time::now_ms();
    let r = ipc::call(storage, sp::WRITE_COPY, &touchos_proto::encode(&req), &[mem]);
    let _ = sys::cap_drop(mem);
    match r {
        Ok(m) if m.label == sp::OK && m.data() == [1] => {}
        Ok(m) if m.label == sp::OK => return Err("storage is read-only (storage=ro): nothing was written".into()),
        Ok(m) if m.label == sp::NOT_ALLOWED => return Err(format!("storage refused to write copy {slot}")),
        Ok(m) => return Err(format!("writing copy {slot} failed: {}", String::from_utf8_lossy(m.data()))),
        Err(e) => return Err(format!("storage not reachable ({e:?})")),
    }
    let build = b.build.to_string();
    info!("copy {slot} has build {build} ({} s); restarting into it (on trial)", (time::now_ms() - t0) / 1000);
    publish(|s| {
        s.other = Some(version_line(&build));
        s.phase = Phase::Restarting { build };
    });
    exec::sleep_ms(3000).await;
    let _ = sys::reboot(system);
    Err("restart failed".into())
}

fn version_line(build: &str) -> String {
    format!("build {build}, just installed")
}

use touchos_rt::ipc::reply;

fn handle(msg: ipc::Msg) -> Tag {
    match msg.label {
        proto::INSTALL => {
            let caps = msg.caps().to_vec();
            let Some(&mem) = caps.first() else {
                return reply(proto::INVALID, b"no bundle");
            };
            let len = msg
                .data()
                .get(..4)
                .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
            if BUSY.swap(true, Ordering::Relaxed) {
                let _ = sys::cap_drop(mem);
                return reply(proto::BUSY, &[]);
            }
            let map = Mapping::map(mem, false);
            let _ = sys::cap_drop(mem);
            let map = match map {
                Ok(m) if m.len >= len && len > 0 => m,
                _ => {
                    BUSY.store(false, Ordering::Relaxed);
                    return reply(proto::INVALID, b"can't read the upload");
                }
            };
            let t0 = time::now_ms();
            match check(&map.bytes()[..len]) {
                Ok(b) => {
                    let build = b.build.to_string();
                    info!(
                        "update {build} ({} KiB) checked in {} ms",
                        len / 1024,
                        time::now_ms() - t0
                    );
                    publish(|s| s.phase = Phase::Idle);
                    exec::spawn(install(map, len));
                    reply(proto::OK, build.as_bytes())
                }
                Err(why) => {
                    warn!("update refused: {why}");
                    BUSY.store(false, Ordering::Relaxed);
                    reply(proto::INVALID, why.as_bytes())
                }
            }
        }
        _ => reply(proto::INVALID, b"unknown request"),
    }
}

fn main() {
    let Some(ep) = cap("ep") else { return };
    let running = running();
    if PUBLIC_KEY.is_none() {
        warn!("built without an update key (cargo xtask keygen): updates are refused");
    }
    if let Some(board) = shm::board("state", true) {
        let st = State {
            running,
            build: arg("build").unwrap_or("-").into(),
            other: None,
            enabled: PUBLIC_KEY.is_some() && running.is_some(),
            phase: Phase::Idle,
        };
        board.write_as(&st);
        *STATE.lock() = Some((st, board));
    }
    if let (Some(st), Some(r)) = (cap("storage"), running) {
        exec::spawn(learn_other(st, other(r)));
    }
    exec::serve(ep, |msg, _| handle(msg))
}
