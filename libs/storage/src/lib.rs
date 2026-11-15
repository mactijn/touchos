//! The storage server's rules, as pure functions with tests: the
//! `storage=` mode ([`Mode`]), and which client may read which file
//! ([`Access`]). The server (servers/storage) does the I/O.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use touchos_proto::storage::{BACKUP, BROKEN, VERSION_FILE, badge};

/// `storage=` on the kernel command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Storage isn't used.
    Off,
    /// Write nothing (as `Ro`).
    Probe,
    /// Read; a write is a logged dry run (also any unknown word).
    Ro,
    Rw,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "off" => Mode::Off,
            "probe" => Mode::Probe,
            "rw" => Mode::Rw,
            _ => Mode::Ro,
        }
    }
}

/// The files storage serves and who may read them.
pub struct Access {
    /// The config file's path on the partition (e.g. "touchos.toml").
    pub config_path: String,
    /// Its directory ("" = the root): names without a '/' are there.
    pub dir: String,
    /// The running copy, if known: the update server may only write the
    /// other one.
    pub running: Option<char>,
}

impl Access {
    pub fn new(config_path: &str, running: Option<char>) -> Access {
        let config_path = config_path.trim_start_matches('/');
        let dir = config_path.rsplit_once('/').map_or(String::new(), |(d, _)| d.into());
        Access { config_path: config_path.into(), dir, running }
    }

    /// `name` next to the config file.
    pub fn beside_config(&self, name: &str) -> String {
        if self.dir.is_empty() { name.into() } else { format!("{}/{name}", self.dir) }
    }

    /// A path from a client: names without a '/' are next to the config
    /// file ("" = the config file itself).
    pub fn resolve(&self, p: &str) -> String {
        let p = p.trim_start_matches('/');
        match (p.contains('/'), p.is_empty()) {
            (_, true) => self.config_path.clone(),
            (false, false) => self.beside_config(p),
            (true, false) => p.into(),
        }
    }

    /// What each client may read: the config server its file and the two
    /// kept copies; the update server each copy's version file.
    pub fn may_read(&self, client: usize, p: &str) -> bool {
        let beside = |name: &str| p.eq_ignore_ascii_case(&self.beside_config(name));
        match client {
            badge::CONFIG => p.eq_ignore_ascii_case(&self.config_path) || beside(BACKUP) || beside(BROKEN),
            badge::UPDATE => {
                ['A', 'B'].iter().any(|s| p.eq_ignore_ascii_case(&format!("{s}/{VERSION_FILE}")))
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(Mode::parse("rw"), Mode::Rw);
        assert_eq!(Mode::parse("off"), Mode::Off);
        assert_eq!(Mode::parse("RW"), Mode::Ro); // anything else is read-only
    }

    #[test]
    fn who_reads_what() {
        let a = Access::new("/touchos.toml", Some('A'));
        assert_eq!(a.resolve(""), "touchos.toml");
        assert_eq!(a.resolve("TOUCHOS.BAK"), "TOUCHOS.BAK");
        assert!(a.may_read(badge::CONFIG, "touchos.toml"));
        assert!(a.may_read(badge::CONFIG, &a.resolve("touchos.bak")));
        assert!(!a.may_read(badge::CONFIG, "A/VERSION.TXT"));
        assert!(a.may_read(badge::UPDATE, "B/VERSION.TXT"));
        assert!(!a.may_read(badge::UPDATE, "touchos.toml")); // the token
        assert!(!a.may_read(99, "touchos.toml"));
        let nested = Access::new("cfg/touchos.toml", None);
        assert_eq!(nested.resolve("TOUCHOS.BAK"), "cfg/TOUCHOS.BAK");
    }
}
