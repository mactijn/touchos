//! The Toon's NAND (libs/nand).
//!
//! - **The config** (badge CONFIG), in the config ring (libs/nand's
//!   store): REPLACE saves touchos.toml as a new record (the one before it
//!   is the backup), and READ_FILE gives back touchos.toml, TOUCHOS.BAK
//!   and TOUCHOS.BAD. The boot stage (tools/toon/nandboot) loads the
//!   newest record at boot.
//! - **The system copies** (badge UPDATE, libs/nand's system): WRITE_COPY
//!   writes the copy that isn't running and puts it on trial;
//!   COPY_STATE and COPY_VERDICT read and settle the state record (init
//!   confirms a healthy copy, or gives up its trial); READ_FILE of
//!   `<slot>/VERSION.TXT` gives the copy's build.
//!
//! Only touchos's area is erased or written (never blocks 0-71, U-Boot's
//! and the boot stage's), and never the running copy; `mode=ro` makes
//! every write a dry run.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use log::{error, info, warn};
use touchos_abi::Tag;
use touchos_drv::mmio::Regs;
use touchos_nand::image::ConfigKind;
use touchos_nand::layout::{self, BadBlocks};
use touchos_nand::nfc::{self, Nfc};
use touchos_nand::store;
use touchos_nand::system;
use touchos_proto::storage as proto;
use touchos_rt::{caps::Mapping, exec, ipc, sys};
use touchos_storage::{Access, Mode};

use crate::{MAX_FILE, SENT, now_us};
use touchos_rt::ipc::reply;

struct Nand {
    nfc: Nfc,
    bad: BadBlocks,
    /// The registers' mapping stays as long as the driver.
    _regs: Regs,
}

fn open(regs: Regs) -> Result<Nand, String> {
    // SAFETY: the NFC's page, mapped uncached; only this server drives it
    // (the boot stage ran before the kernel).
    let nfc = unsafe { Nfc::new(regs.base()) };
    nfc.init().map_err(|e| format!("NAND: {e:?}"))?;
    let id = nfc.read_id().map_err(|e| format!("NAND: {e:?}"))?;
    if id[..2] != nfc::ID {
        return Err(format!("NAND: unknown chip {:02x?}", id));
    }
    let mut bad = BadBlocks::default();
    for b in layout::SYSTEM.first..layout::SYSTEM.end {
        if nfc.is_bad(b).map_err(|e| format!("NAND: {e:?}"))? {
            bad.mark(b);
        }
    }
    Ok(Nand { nfc, bad, _regs: regs })
}

impl Nand {
    fn status(&self) -> String {
        let r = store::scan(&mut &self.nfc, &self.bad, layout::CONFIG);
        let cur = r.current().map_or(String::from("none"), |(_, c)| format!("#{} ({} bytes)", c.seq, c.len));
        format!(
            "NAND (config ring, blocks {}-{}): {} records, current {cur}, backup {}, broken {}",
            layout::CONFIG.first,
            layout::CONFIG.end - 1,
            r.iter().count(),
            if r.backup().is_some() { "kept" } else { "none" },
            if r.broken().is_some() { "kept" } else { "none" },
        )
    }

    /// One of the three files, by which record it is.
    fn read(&self, which: Which) -> Result<Option<alloc::vec::Vec<u8>>, nfc::Error> {
        let r = store::scan(&mut &self.nfc, &self.bad, layout::CONFIG);
        let rec = match which {
            Which::Current => r.current(),
            Which::Backup => r.backup(),
            Which::Broken => r.broken(),
        };
        let Some((block, c)) = rec else { return Ok(None) };
        let mut out = vec![0u8; c.len as usize];
        if !store::read(&mut &self.nfc, block, &c, &mut out)? {
            warn!("config record #{} fails its CRC", c.seq);
            return Ok(None);
        }
        Ok(Some(out))
    }
}

#[derive(Clone, Copy)]
enum Which {
    Current,
    Backup,
    Broken,
}

fn which(a: &Access, path: &str) -> Option<Which> {
    if path.eq_ignore_ascii_case(&a.config_path) {
        Some(Which::Current)
    } else if path.eq_ignore_ascii_case(&a.beside_config(proto::BACKUP)) {
        Some(Which::Backup)
    } else if path.eq_ignore_ascii_case(&a.beside_config(proto::BROKEN)) {
        Some(Which::Broken)
    } else {
        None
    }
}

pub fn serve(ep: u32, regs: Regs, mode: Mode, access: Access) -> ! {
    let nand = open(regs);
    match &nand {
        Ok(n) => info!("{}{}", n.status(), if mode == Mode::Rw { "" } else { "; read-only (saves are dry runs)" }),
        Err(e) => error!("{e}"),
    }
    exec::serve(ep, |msg, badge| handle(&nand, mode, &access, msg, badge))
}

fn drop_caps(msg: &ipc::Msg) {
    for &c in msg.caps() {
        let _ = sys::cap_drop(c);
    }
}

fn handle(nand: &Result<Nand, String>, mode: Mode, a: &Access, msg: ipc::Msg, badge: usize) -> Tag {
    for c in SENT.lock().drain(..) {
        let _ = sys::cap_drop(c);
    }
    let allowed = match badge {
        proto::badge::CONFIG => matches!(msg.label, proto::STATUS | proto::READ_FILE | proto::REPLACE),
        proto::badge::UPDATE => matches!(
            msg.label,
            proto::STATUS | proto::READ_FILE | proto::COPY_STATE | proto::COPY_VERDICT | proto::WRITE_COPY
        ),
        _ => msg.label == proto::STATUS,
    };
    if !allowed {
        drop_caps(&msg);
        return reply(proto::NOT_ALLOWED, &[]);
    }
    let n = match (msg.label, nand) {
        (proto::STATUS, Ok(n)) => return reply(proto::OK, n.status().as_bytes()),
        (proto::STATUS, Err(e)) => return reply(proto::OK, format!("unavailable: {e}").as_bytes()),
        (_, Err(e)) => {
            drop_caps(&msg);
            return reply(proto::ERROR, e.as_bytes());
        }
        (_, Ok(n)) => n,
    };
    let d = msg.data();
    match msg.label {
        proto::READ_FILE if badge == proto::badge::UPDATE => {
            // `<slot>/VERSION.TXT`: the copy's build, from its header.
            let slot = match d {
                [s @ (b'A' | b'B'), b'/', rest @ ..] if rest.eq_ignore_ascii_case(proto::VERSION_FILE.as_bytes()) => *s,
                _ => return reply(proto::NOT_ALLOWED, &[]),
            };
            match system::header(&mut &n.nfc, &n.bad, slot) {
                Some(h) => share_reply(format!("build {}\n", h.build_str()).as_bytes()),
                None => reply(proto::NOT_FOUND, &[]),
            }
        }
        proto::COPY_STATE => match system::newest_state(&mut &n.nfc, &n.bad) {
            Some((s, _)) => {
                let st = proto::CopyState { active: s.active as char, trial: (s.trial != 0).then_some(s.trial as char), starts: s.starts };
                reply(proto::OK, &touchos_proto::encode(&st))
            }
            None => reply(proto::ERROR, b"no state record"),
        },
        proto::COPY_VERDICT => {
            let Some(v) = touchos_proto::decode::<proto::Verdict>(d) else {
                return reply(proto::ERROR, b"bad request");
            };
            verdict(n, mode, a, v)
        }
        proto::WRITE_COPY => {
            let &[mem] = msg.caps() else {
                drop_caps(&msg);
                return reply(proto::ERROR, b"bad request");
            };
            let r = write_copy(n, mode, a, d, mem);
            let _ = sys::cap_drop(mem);
            r
        }
        proto::READ_FILE => {
            let Ok(path) = core::str::from_utf8(d) else {
                return reply(proto::ERROR, b"bad path");
            };
            let path = a.resolve(path);
            let (true, Some(w)) = (a.may_read(badge, &path), which(a, &path)) else {
                return reply(proto::NOT_ALLOWED, &[]);
            };
            match n.read(w) {
                Ok(Some(data)) => match touchos_rt::caps::share(&data) {
                    Some(m) => {
                        SENT.lock().push(m);
                        ipc::prepare(proto::OK, &[], &[m])
                    }
                    None => reply(proto::ERROR, b"out of memory"),
                },
                Ok(None) => reply(proto::NOT_FOUND, &[]),
                Err(e) => reply(proto::ERROR, format!("{e:?}").as_bytes()),
            }
        }
        proto::REPLACE => {
            let &[mem] = msg.caps() else {
                drop_caps(&msg);
                return reply(proto::ERROR, b"bad request");
            };
            let r = replace(n, mode, d, mem);
            let _ = sys::cap_drop(mem);
            r
        }
        _ => reply(proto::ERROR, b"unknown request"),
    }
}

fn share_reply(data: &[u8]) -> Tag {
    match touchos_rt::caps::share(data) {
        Some(m) => {
            SENT.lock().push(m);
            ipc::prepare(proto::OK, &[], &[m])
        }
        None => reply(proto::ERROR, b"out of memory"),
    }
}

/// The running copy is healthy (it becomes the active copy, its start
/// count 0), or its trial failed (the active copy starts next).
fn verdict(n: &Nand, mode: Mode, a: &Access, v: proto::Verdict) -> Tag {
    if a.running != Some(v.slot) {
        warn!("verdict on copy {} refused: copy {:?} is running", v.slot, a.running);
        return reply(proto::NOT_ALLOWED, &[]);
    }
    let mut f = &n.nfc;
    let newest = system::newest_state(&mut f, &n.bad).map(|(s, _)| s);
    let slot = v.slot as u8;
    let record = if v.healthy {
        system::confirmed(newest, slot)
    } else {
        // Back to the active copy: nothing on trial any more.
        newest.filter(|s| s.trial == slot).map(|s| touchos_nand::image::State { trial: 0, starts: 0, ..s })
    };
    let Some(record) = record else {
        return reply(proto::OK, &[0]); // nothing to change
    };
    if mode != Mode::Rw {
        info!("dry run: would write state record {record:?}");
        return reply(proto::OK, &[0]);
    }
    match system::append_state(&mut f, &n.bad, record) {
        Ok(s) => {
            info!("state record #{}: active copy {}, {}", s.seq, s.active as char, if s.trial != 0 { "on trial" } else { "nothing on trial" });
            reply(proto::OK, &[1])
        }
        Err(e) => {
            error!("writing the state record failed: {e:?}");
            reply(proto::ERROR, format!("{e:?}").as_bytes())
        }
    }
}

/// Write the copy that isn't running from an update's parts, then put it
/// on trial: the boot stage starts it next (TRIAL_STARTS times at most,
/// until init confirms it).
fn write_copy(n: &Nand, mode: Mode, a: &Access, d: &[u8], mem: u32) -> Tag {
    let Some(r) = touchos_proto::decode::<proto::CopyWrite>(d) else {
        return reply(proto::ERROR, b"bad request");
    };
    if !matches!(r.slot, 'A' | 'B') || a.running == Some(r.slot) || a.running.is_none() {
        warn!("refused to write copy {} (running copy {:?})", r.slot, a.running);
        return reply(proto::NOT_ALLOWED, &[]);
    }
    let (k, i, b) = (r.kernel as usize, r.initrd as usize, r.bootinfo as usize);
    let len = k + i + b;
    let mapping = match Mapping::map(mem, false) {
        Ok(m) if m.len >= len => m,
        _ => return reply(proto::ERROR, b"can't read the new copy"),
    };
    let data = &mapping.bytes()[..len];
    if mode != Mode::Rw {
        info!("dry run: would write copy {} (build {}, {} KiB)", r.slot, r.build, len / 1024);
        return reply(proto::OK, &[0]);
    }
    let t0 = now_us();
    let mut f = &n.nfc;
    let slot = r.slot as u8;
    let h = match system::write_copy(&mut f, &n.bad, slot, r.build, &data[..k], &data[k..k + i], &data[k + i..]) {
        Ok(h) => h,
        Err(e) => {
            error!("writing copy {} failed: {e:?}", r.slot);
            return reply(proto::ERROR, format!("writing the copy failed ({e:?})").as_bytes());
        }
    };
    let active = system::newest_state(&mut f, &n.bad).map_or(system::other(slot), |(s, _)| s.active);
    let trial = touchos_nand::image::State { seq: 0, active, trial: slot, starts: 0 };
    match system::append_state(&mut f, &n.bad, trial) {
        Ok(s) => {
            info!(
                "copy {} is build {} ({} KiB, written and read back in {} s); on trial from the next start (state record #{})",
                r.slot,
                h.build_str(),
                len / 1024,
                (now_us() - t0) / 1_000_000,
                s.seq
            );
            reply(proto::OK, &[1])
        }
        Err(e) => {
            error!("writing the state record failed: {e:?}");
            reply(proto::ERROR, format!("the copy is written, but its state record failed ({e:?})").as_bytes())
        }
    }
}

/// Save a new touchos.toml. `Keep::Backup`: the old one stays as the
/// backup (it is the record before). `Keep::Broken`: the old one didn't
/// parse; it is kept as the broken record first, so it never counts as
/// a backup.
fn replace(n: &Nand, mode: Mode, d: &[u8], mem: u32) -> Tag {
    let Some(r) = touchos_proto::decode::<proto::Replace>(d) else {
        return reply(proto::ERROR, b"bad request");
    };
    let len = r.len as usize;
    if len > MAX_FILE {
        return reply(proto::NOT_ALLOWED, &[]);
    }
    let mapping = match Mapping::map(mem, false) {
        Ok(m) if m.len >= len => m,
        _ => return reply(proto::ERROR, b"can't read the new contents"),
    };
    let data = &mapping.bytes()[..len];
    if mode != Mode::Rw {
        info!("dry run: would save touchos.toml ({len} bytes) as a new config record");
        return reply(proto::OK, &[0]);
    }
    let t0 = now_us();
    let mut f = &n.nfc;
    let result = (|| {
        if r.keep == proto::Keep::Broken
            && let Some(old) = n.read(Which::Current)?
        {
            store::append(&mut f, &n.bad, layout::CONFIG, ConfigKind::Broken, &old)?;
        }
        store::append(&mut f, &n.bad, layout::CONFIG, ConfigKind::Current, data)
    })();
    match result {
        Ok(block) => {
            info!("saved touchos.toml: {len} bytes in block {block}, {} ms", (now_us() - t0) / 1000);
            reply(proto::OK, &[1])
        }
        Err(e) => {
            error!("saving touchos.toml failed: {e:?}");
            reply(proto::ERROR, format!("{e:?}").as_bytes())
        }
    }
}
