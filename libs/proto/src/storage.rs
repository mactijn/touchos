//! The storage server's protocol. Two clients, told apart by the badge on
//! their capability: the config server (the config file) and the update
//! server (the copy of the system that isn't running, and the state
//! record).

use serde::{Deserialize, Serialize};

/// Badges init puts on the storage capabilities.
pub mod badge {
    pub const CONFIG: usize = 1;
    pub const UPDATE: usize = 2;
}

/// → OK, data: a status line (mode, the NAND, whether writes are on).
pub const STATUS: u32 = 1;
/// data: path. → OK, caps[0]: read-only memory with the contents (its
/// length is the file's) | NOT_FOUND | NOT_ALLOWED.
pub const READ_FILE: u32 = 2;
/// (CONFIG) data: [`Replace`]; caps[0]: memory with the new contents.
/// → OK (data: written u8: 1 = written, 0 = dry run) | NOT_ALLOWED |
/// ERROR (data: message).
pub const REPLACE: u32 = 3;
// 4, 5: reserved (files of a copy on a FAT partition, the start counts).

/// (UPDATE) Which copy is active, which is on trial, and starts
/// without a confirmation, from the newest state record. → OK, data:
/// [`CopyState`] | ERROR.
pub const COPY_STATE: u32 = 6;
/// (UPDATE) data: [`Verdict`]: the running copy is healthy (it
/// becomes the active copy, its start count 0), or its trial failed (the
/// active copy starts next). → OK (data: written u8) | NOT_ALLOWED | ERROR.
pub const COPY_VERDICT: u32 = 7;
/// (UPDATE) data: [`CopyWrite`]; caps[0]: memory with the kernel,
/// initrd and boot info, one after the other. Writes the copy that isn't
/// running and puts it on trial. → OK (data: written u8) | NOT_ALLOWED |
/// ERROR (data: message).
pub const WRITE_COPY: u32 = 8;

/// [`COPY_STATE`]'s answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyState {
    pub active: char,
    /// The copy on trial, if any.
    pub trial: Option<char>,
    /// Starts without a confirmation (of the copy on trial, if any).
    pub starts: u8,
}

/// [`COPY_VERDICT`]'s request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub slot: char,
    pub healthy: bool,
}

/// [`WRITE_COPY`]'s request: the parts' lengths, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyWrite<'a> {
    pub slot: char,
    pub build: &'a str,
    pub kernel: u32,
    pub initrd: u32,
    pub bootinfo: u32,
}

/// Next to the config file: its previous contents.
pub const BACKUP: &str = "TOUCHOS.BAK";
/// Next to the config file: a config file that didn't parse, kept aside.
pub const BROKEN: &str = "TOUCHOS.BAD";
/// `<slot>/VERSION.TXT`: a copy's build.
pub const VERSION_FILE: &str = "VERSION.TXT";

pub const OK: u32 = 0;
pub const NOT_ALLOWED: u32 = 1;
pub const ERROR: u32 = 2;
pub const NOT_FOUND: u32 = 3;

/// The time a saved config gets, in the panel's local time (the config
/// server chooses; no zone is kept).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Stamp {
    pub fn from_civil(t: &touchos_civil::Civil) -> Stamp {
        Stamp {
            year: t.year.clamp(0, u16::MAX as i32) as u16,
            month: t.month as u8,
            day: t.day as u8,
            hour: t.hour as u8,
            minute: t.minute as u8,
            second: t.second as u8,
        }
    }
}

/// Where REPLACE puts the old contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keep {
    /// As [`BACKUP`] (the usual save).
    Backup,
    /// As [`BROKEN`]: the old file didn't parse; the good backup stays.
    Broken,
}

/// [`REPLACE`]: the config file gets new contents (`len` bytes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replace {
    pub keep: Keep,
    pub stamp: Stamp,
    pub len: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, encode};

    #[test]
    fn requests_round_trip() {
        let stamp = Stamp { year: 2026, month: 11, day: 14, hour: 23, minute: 5, second: 59 };
        let r = Replace { keep: Keep::Broken, stamp, len: 1234 };
        assert_eq!(decode::<Replace>(&encode(&r)), Some(r));
        let c = CopyWrite { slot: 'A', build: "abc-dirty", kernel: 600_000, initrd: 8 << 20, bootinfo: 536 };
        assert_eq!(decode::<CopyWrite>(&encode(&c)), Some(c));
        let v = Verdict { slot: 'B', healthy: false };
        assert_eq!(decode::<Verdict>(&encode(&v)), Some(v));
        let st = CopyState { active: 'A', trial: Some('B'), starts: 2 };
        assert_eq!(decode::<CopyState>(&encode(&st)), Some(st));
    }

    #[test]
    fn malformed_requests_are_refused() {
        let c = encode(&CopyWrite { slot: 'B', build: "x", kernel: 1, initrd: 2, bootinfo: 3 });
        for n in 0..c.len() {
            assert_eq!(decode::<CopyWrite>(&c[..n]), None, "cut at {n}");
        }
        assert_eq!(decode::<Replace>(&[7, 0, 0, 0, 0, 0, 0, 0, 0]), None); // no such Keep
    }

    #[test]
    fn stamps_from_civil_time() {
        let cet = touchos_civil::Tz::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        let t = cet.local(1_763_161_559); // 2025-11-14 23:05:59 UTC
        let s = Stamp::from_civil(&t);
        assert_eq!((s.year, s.month, s.day, s.hour, s.minute, s.second), (2025, 11, 15, 0, 5, 59));
    }
}
