//! The config server: the one owner of the settings.
//!
//! It reads touchos.toml (the boot file init gives it), serves it to the
//! other processes (without secrets unless their capability says so),
//! notifies watchers of changes, and applies checked patches from the web
//! interface. Patches change the text line by line, so comments survive,
//! and are saved through the storage server (the Toon's NAND).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use log::{error, info, warn};
use touchos_abi::Tag;
use touchos_config::proto::{self, badge, req};
use touchos_config::{Config, Document, patch};
use touchos_proto::storage as sp;
use touchos_rt::{cap, caps, exec, ipc, sys};

touchos_rt::entry!(main);

struct State {
    doc: Document,
    version: u64,
    source: u32,
    problems: Vec<String>,
    /// Read-only snapshots of the current version: (full, public).
    snapshots: Option<(u32, u32)>,
    watchers: Vec<(u32, usize)>,
    /// The storage server (to save changes), if there is one.
    storage: Option<u32>,
    /// The boot file didn't parse: the first save keeps it as TOUCHOS.BAD
    /// (and leaves the good TOUCHOS.BAK alone).
    boot_file_broken: bool,
}

impl State {
    fn load(text: &str, source: u32) -> State {
        let (doc, source, mut problems) = match Document::parse(text) {
            Ok(d) => (d, source, Vec::new()),
            Err(e) => {
                error!("touchos.toml {e}; using defaults");
                let mut p = Vec::new();
                p.push(alloc::format!("touchos.toml: {e}"));
                (Document::default(), proto::source::DEFAULTS, p)
            }
        };
        if let Ok((_, issues)) = Config::parse(&doc.to_text()) {
            for i in issues {
                warn!("touchos.toml: {i}");
                problems.push(alloc::format!("{i}"));
            }
        }
        State {
            doc,
            version: 1,
            source,
            problems,
            snapshots: None,
            watchers: Vec::new(),
            storage: None,
            boot_file_broken: false,
        }
    }

    /// Save the current text through the storage server. Returns 1 if it
    /// was written, 0 if not (no storage, or storage is read-only: a dry
    /// run), 2 if writing failed.
    fn persist(&mut self) -> u8 {
        let Some(storage) = self.storage else {
            return 0;
        };
        let text = self.doc.to_text();
        let Some(mem) = Self::snapshot(&text) else {
            return 2;
        };
        let cfg = Config::parse(&text).map(|(c, _)| c).unwrap_or_default();
        let unix = touchos_rt::time::unix_now();
        let t = touchos_civil::local(
            unix,
            cfg.display.utc_offset_minutes as i64,
            cfg.display.eu_dst,
        );
        let req = sp::Replace {
            keep: if self.boot_file_broken { sp::Keep::Broken } else { sp::Keep::Backup },
            stamp: sp::Stamp::from_civil(&t),
            len: text.len() as u32,
        };
        let r = ipc::call(storage, sp::REPLACE, &touchos_proto::encode(&req), &[mem]);
        let _ = sys::cap_drop(mem);
        match r {
            Ok(m) if m.label == sp::OK => {
                let written = m.data().first() == Some(&1);
                if written {
                    self.boot_file_broken = false;
                    info!("saved to storage");
                } else {
                    info!("not saved (storage is read-only: dry run)");
                }
                written as u8
            }
            Ok(m) => {
                error!("saving failed: {}", String::from_utf8_lossy(m.data()));
                2
            }
            Err(e) => {
                error!("saving failed: storage not reachable ({e:?})");
                2
            }
        }
    }

    /// The backup copy next to the config file, if storage has one.
    fn read_backup(storage: u32) -> Option<String> {
        let m = ipc::call(storage, sp::READ_FILE, sp::BACKUP.as_bytes(), &[]).ok()?;
        if m.label != sp::OK || m.caps().is_empty() {
            return None;
        }
        let text = caps::read_shared(m.caps()[0])?;
        Some(String::from_utf8_lossy(&text).into_owned())
    }

    /// A read-only memory object holding `text`.
    fn snapshot(text: &str) -> Option<u32> {
        caps::share(text.as_bytes())
    }

    fn snapshots(&mut self) -> Option<(u32, u32)> {
        if self.snapshots.is_none() {
            let full = Self::snapshot(&self.doc.to_text())?;
            let public = Self::snapshot(&patch::redact(&self.doc).to_text())?;
            self.snapshots = Some((full, public));
        }
        self.snapshots
    }

    fn changed(&mut self, doc: Document) {
        self.doc = doc;
        self.version += 1;
        if let Some((a, b)) = self.snapshots.take() {
            let _ = sys::cap_drop(a);
            let _ = sys::cap_drop(b);
        }
        // Drop watchers whose process is gone (signal fails: Dead).
        self.watchers.retain(|&(n, bits)| {
            let ok = sys::signal(n, bits).is_ok();
            if !ok {
                let _ = sys::cap_drop(n);
            }
            ok
        });
    }
}

fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?))
}

fn handle(st: &mut State, msg: ipc::Msg, who: usize) -> Tag {
    match msg.label {
        req::GET => {
            let Some((full, public)) = st.snapshots() else {
                return ipc::prepare(proto::REPLY_BAD_REQUEST, &[], &[]);
            };
            let snap = if who == badge::FULL { full } else { public };
            ipc::prepare(proto::REPLY_OK, &st.version.to_le_bytes(), &[snap])
        }
        req::WATCH => {
            let (Some(&n), Some(bits)) = (msg.caps().first(), u64_at(msg.data(), 0)) else {
                return ipc::prepare(proto::REPLY_BAD_REQUEST, &[], &[]);
            };
            st.watchers.push((n, bits as usize));
            ipc::prepare(proto::REPLY_OK, &[], &[])
        }
        req::UPDATE => {
            if who != badge::ADMIN {
                return ipc::prepare(proto::REPLY_DENIED, &[], &[]);
            }
            let Some(base) = u64_at(msg.data(), 0) else {
                return ipc::prepare(proto::REPLY_BAD_REQUEST, &[], &[]);
            };
            if base != st.version {
                return ipc::prepare(proto::REPLY_CONFLICT, &st.version.to_le_bytes(), &[]);
            }
            let text = String::from_utf8_lossy(&msg.data()[8..]).into_owned();
            let result = patch::parse(&text)
                .map_err(|e| alloc::vec![e])
                .and_then(|ops| patch::apply(&st.doc, &ops));
            match result {
                Ok((doc, fx)) => {
                    st.changed(doc);
                    info!("settings changed (version {})", st.version);
                    let saved = st.persist();
                    let mut out = Vec::new();
                    out.extend_from_slice(&st.version.to_le_bytes());
                    out.extend_from_slice(&fx.bits().to_le_bytes());
                    out.push(saved);
                    ipc::prepare(proto::REPLY_OK, &out, &[])
                }
                Err(errors) => {
                    let mut out = String::new();
                    for e in errors {
                        let _ = writeln!(out, "{e}");
                    }
                    ipc::prepare(proto::REPLY_INVALID, out.as_bytes(), &[])
                }
            }
        }
        req::STATUS => {
            let mut out = Vec::new();
            out.extend_from_slice(&st.version.to_le_bytes());
            out.extend_from_slice(&st.source.to_le_bytes());
            for p in &st.problems {
                out.extend_from_slice(p.as_bytes());
                out.push(b'\n');
            }
            out.truncate(touchos_abi::UTCB_DATA);
            ipc::prepare(proto::REPLY_OK, &out, &[])
        }
        _ => ipc::prepare(proto::REPLY_BAD_REQUEST, &[], &[]),
    }
}

fn main() {
    let ep = cap("ep").expect("config: no endpoint");
    let text = cap("file")
        .and_then(|c| caps::Mapping::map(c, false).ok())
        .map(|m| String::from_utf8_lossy(m.bytes()).into_owned());
    let storage = cap("storage");
    let mut st = match text {
        Some(t) => State::load(&t, proto::source::BOOT_FILE),
        None => {
            warn!("no touchos.toml; using defaults");
            State::load("", proto::source::DEFAULTS)
        }
    };
    // A boot file with a syntax error: use the last good copy instead.
    let broken = st.source == proto::source::DEFAULTS && !st.problems.is_empty();
    if broken && let Some(bak) = storage.and_then(State::read_backup) {
        if Document::parse(&bak).is_ok() {
            warn!("touchos.toml is broken; using the backup TOUCHOS.BAK");
            let problems = core::mem::take(&mut st.problems);
            st = State::load(&bak, proto::source::BACKUP);
            st.problems.splice(0..0, problems);
        }
    }
    st.storage = storage;
    st.boot_file_broken = broken;
    info!(
        "serving touchos.toml ({} lines, {} problems)",
        st.doc.to_text().lines().count(),
        st.problems.len()
    );
    exec::serve(ep, |msg, who| handle(&mut st, msg, who))
}
