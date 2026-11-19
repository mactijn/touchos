//! The typed configuration that services use, built from a [`Raw`] through
//! the schema: every value is checked, bad values fall back to the default
//! and are reported as [`Issue`]s.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::schema::{self, Check, FIELDS, MAX_FILE_BYTES, MAX_TILES, TILE_FIELDS};
use crate::toml::{Document, ParseError, Raw, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub homeassistant: HomeAssistant,
    pub wifi: Wifi,
    pub display: Display,
    pub time: Time,
    pub net: Net,
    pub dashboard: Dashboard,
    pub debug: Debug,
    pub web: Web,
    pub metrics: Metrics,
    /// `[[tile]]` list; empty = automatic tiles from `dashboard.types`.
    pub tiles: Vec<Tile>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HomeAssistant {
    pub host: Option<String>,
    pub port: u16,
    pub token: Option<String>,
    pub tls: Option<bool>,
}

impl HomeAssistant {
    pub fn use_tls(&self) -> bool {
        self.tls.unwrap_or(self.port == 443)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Wifi {
    pub ssid: Option<String>,
    pub psk: Option<String>,
    /// Associate as an 802.11n (and WMM) station where the access point is one.
    pub ht: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// Follow the accelerometer.
    Auto,
    /// Fixed, in quarter turns clockwise (0..=3).
    Fixed(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// Percent.
    pub brightness: u8,
    pub dim_brightness: u8,
    pub dim_after_seconds: u32,
    /// Screen off when idle between these minutes of the day (may wrap).
    pub night: Option<(u16, u16)>,
    pub rotation: Rotation,
    /// Draw the dashboard (false: leave the text console on screen).
    pub ui: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Time {
    pub ntp_server: String,
    /// `time.zone`'s rule.
    pub zone: touchos_civil::Tz,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Net {
    /// `net.hostname`; `None` = `touchos-` and the MAC's last three bytes
    /// (`touchos_proto::net::default_hostname`).
    pub hostname: Option<String>,
    /// Answer mDNS queries and announce services.
    pub mdns: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dashboard {
    pub types: Vec<String>,
    /// Rooms to show, in order; `None` = all areas alphabetically.
    pub rooms: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Debug {
    pub log_host: Option<([u8; 4], u16)>,
    /// Write pairing PINs to the log (development: `cargo xtask pair`).
    pub log_pin: bool,
    pub http_test: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Metrics {
    /// Serve `/metrics` (Prometheus), without a session.
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Web {
    pub enabled: bool,
    pub port: u16,
    pub session_hours: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub entity: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The value was used after a correction, or is unknown and ignored.
    Warning,
    /// The value could not be used; the default applies.
    Error,
}

/// A problem with one setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// `"section.key"`, or `"tile[3].entity"`.
    pub field: String,
    pub msg: String,
    pub severity: Severity,
}

impl core::fmt::Display for Issue {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}: {}", self.field, self.msg)
    }
}

impl core::default::Default for Config {
    fn default() -> Config {
        Config::from_raw(&Raw::default()).0
    }
}

impl Config {
    /// Parse touchos.toml. A syntax error rejects the whole file; problems
    /// with single values are returned as issues.
    pub fn parse(text: &str) -> Result<(Config, Vec<Issue>), ParseError> {
        let doc = Document::parse(text)?;
        let (cfg, mut issues) = Config::from_raw(&doc.raw());
        if text.len() > MAX_FILE_BYTES {
            issues.push(Issue {
                field: String::new(),
                msg: format!("file is larger than {} KiB", MAX_FILE_BYTES / 1024),
                severity: Severity::Warning,
            });
        }
        Ok((cfg, issues))
    }

    pub fn from_raw(raw: &Raw) -> (Config, Vec<Issue>) {
        let mut issues = Vec::new();
        let issue = |issues: &mut Vec<Issue>, field: &str, msg: String, severity| {
            issues.push(Issue {
                field: field.to_string(),
                msg,
                severity,
            })
        };

        // Every known field: the checked value, or its default.
        let mut v: BTreeMap<&'static str, Value> = BTreeMap::new();
        for f in FIELDS {
            let given = raw.values.get(f.id);
            let value = match given.map(|g| schema::check(f.kind, g)) {
                Some(Check::Ok(x)) => Some(x),
                Some(Check::Fixed(x, msg)) => {
                    issue(&mut issues, f.id, msg, Severity::Warning);
                    Some(x)
                }
                Some(Check::Bad(msg)) => {
                    let msg = match f.default.value() {
                        Some(d) => format!("{msg}; using the default {}", d.to_toml()),
                        None => format!("{msg}; ignored"),
                    };
                    issue(&mut issues, f.id, msg, Severity::Error);
                    f.default.value()
                }
                None => f.default.value(),
            };
            if let Some(x) = value {
                v.insert(f.id, x);
            }
        }
        for key in raw.values.keys() {
            if schema::field(key).is_none() {
                let msg = match schema::RETIRED.iter().find(|r| r.0 == key) {
                    Some((_, now)) => format!("no longer used; set {now} instead"),
                    None => "unknown setting; ignored".into(),
                };
                issue(&mut issues, key, msg, Severity::Warning);
            }
        }
        for name in raw.arrays.keys().filter(|n| *n != "tile") {
            issue(
                &mut issues,
                &format!("[[{name}]]"),
                "unknown list; ignored".into(),
                Severity::Warning,
            );
        }

        let mut tiles = Vec::new();
        for (n, t) in raw.arrays.get("tile").into_iter().flatten().enumerate() {
            let mut checked: BTreeMap<&str, Value> = BTreeMap::new();
            let mut usable = true;
            for f in TILE_FIELDS {
                let id = format!("tile[{}].{}", n + 1, f.key());
                match t.get(f.key()).map(|x| schema::check(f.kind, x)) {
                    Some(Check::Ok(x)) => {
                        checked.insert(f.key(), x);
                    }
                    Some(Check::Fixed(x, msg)) => {
                        issue(&mut issues, &id, msg, Severity::Warning);
                        checked.insert(f.key(), x);
                    }
                    Some(Check::Bad(msg)) => {
                        issue(&mut issues, &id, msg, Severity::Error);
                        usable &= f.key() != "entity";
                    }
                    None if f.key() == "entity" => {
                        issue(&mut issues, &id, "missing".into(), Severity::Error);
                        usable = false;
                    }
                    None => {}
                }
            }
            for k in t.keys() {
                if !TILE_FIELDS.iter().any(|f| f.key() == k) {
                    issue(
                        &mut issues,
                        &format!("tile[{}].{k}", n + 1),
                        "unknown setting; ignored".into(),
                        Severity::Warning,
                    );
                }
            }
            if !usable {
                continue;
            }
            if tiles.len() == MAX_TILES {
                issue(
                    &mut issues,
                    "tile",
                    format!("more than {MAX_TILES} tiles; the rest are ignored"),
                    Severity::Warning,
                );
                break;
            }
            tiles.push(Tile {
                entity: str_of(checked.get("entity")).unwrap(),
                name: str_of(checked.get("name")).filter(|s| !s.is_empty()),
            });
        }

        let int = |id: &str| v.get(id).and_then(Value::as_int).unwrap_or(0);
        let string = |id: &str| str_of(v.get(id));
        let list = |id: &str| -> Option<Vec<String>> {
            match v.get(id) {
                Some(Value::List(items)) => {
                    Some(items.iter().filter_map(|i| str_of(Some(i))).collect())
                }
                _ => None,
            }
        };

        let cfg = Config {
            homeassistant: HomeAssistant {
                host: string("homeassistant.host"),
                port: int("homeassistant.port") as u16,
                token: string("homeassistant.token"),
                tls: v.get("homeassistant.tls").and_then(Value::as_bool),
            },
            wifi: Wifi {
                ssid: string("wifi.ssid"),
                psk: string("wifi.psk"),
                ht: v.get("wifi.ht").and_then(Value::as_bool).unwrap_or(true),
            },
            display: Display {
                brightness: int("display.brightness") as u8,
                dim_brightness: int("display.dim_brightness") as u8,
                dim_after_seconds: int("display.dim_after_seconds") as u32,
                night: string("display.night").and_then(|s| schema::parse_time_range(&s)),
                rotation: match string("display.rotation").as_deref() {
                    Some("90") => Rotation::Fixed(1),
                    Some("180") => Rotation::Fixed(2),
                    Some("270") => Rotation::Fixed(3),
                    Some("0") => Rotation::Fixed(0),
                    _ => Rotation::Auto,
                },
                ui: string("display.ui").as_deref() != Some("off"),
            },
            time: Time {
                ntp_server: string("time.ntp_server").unwrap_or_default(),
                zone: schema::time_zone(&string("time.zone").unwrap_or_default()),
            },
            net: Net {
                hostname: string("net.hostname").filter(|s| !s.is_empty()),
                mdns: v.get("net.mdns").and_then(Value::as_bool).unwrap_or(true),
            },
            dashboard: Dashboard {
                types: list("dashboard.types").unwrap_or_default(),
                rooms: list("dashboard.rooms").filter(|r| !r.is_empty()),
            },
            debug: Debug {
                log_host: string("debug.log_host").and_then(|s| schema::parse_endpoint(&s)),
                log_pin: v
                    .get("debug.log_pin")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                http_test: string("debug.http_test").filter(|s| !s.is_empty()),
            },
            web: Web {
                enabled: v
                    .get("web.enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                port: int("web.port") as u16,
                session_hours: int("web.session_hours") as u32,
            },
            metrics: Metrics {
                enabled: v.get("metrics.enabled").and_then(Value::as_bool).unwrap_or(false),
            },
            tiles,
        };
        (cfg, issues)
    }
}

fn str_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../config/touchos.example.toml");
    const QEMU: &str = include_str!("../../../config/qemu.toml");

    #[test]
    fn defaults_match_the_old_behaviour() {
        let c = Config::default();
        assert_eq!(c.homeassistant.port, 8123);
        assert!(!c.homeassistant.use_tls());
        assert_eq!(c.homeassistant.host, None);
        assert_eq!(c.display.brightness, 80);
        assert_eq!(c.display.dim_brightness, 15);
        assert_eq!(c.display.dim_after_seconds, 60);
        assert_eq!(c.display.rotation, Rotation::Auto);
        assert_eq!(c.time.zone, touchos_civil::Tz::UTC);
        assert!(c.display.ui);
        assert_eq!(c.time.ntp_server, "time.google.com");
        assert_eq!(c.dashboard.types.len(), 8);
        assert_eq!(c.dashboard.rooms, None);
        assert_eq!(c.debug.log_host, None);
        assert!(!c.debug.log_pin);
        assert!(c.web.enabled);
        assert!(!c.metrics.enabled);
        assert_eq!(c.web.port, 80);
        assert!(c.wifi.ht);
        assert!(c.tiles.is_empty());
    }

    #[test]
    fn example_file() {
        let (c, issues) = Config::parse(EXAMPLE).unwrap();
        // Only the placeholder token is a problem.
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].field, "homeassistant.token");
        assert_eq!(c.homeassistant.token, None);
        assert_eq!(c.homeassistant.host.as_deref(), Some("192.168.1.10"));
        assert_eq!(c.tiles.len(), 2);
        assert_eq!(c.tiles[0].name.as_deref(), Some("Living room"));
    }

    #[test]
    fn qemu_file_rotation_zero_is_fixed_now() {
        let (c, issues) = Config::parse(QEMU).unwrap();
        assert_eq!(c.display.rotation, Rotation::Fixed(0));
        assert_eq!(c.debug.log_host, Some(([10, 0, 2, 2], 5555)));
        // The empty token is reported, not used.
        assert!(issues.iter().any(|i| i.field == "homeassistant.token"));
        assert_eq!(c.homeassistant.token, None);
    }

    #[test]
    fn bad_values_fall_back_and_are_reported() {
        let text = "[display]\nbrightness = 150\nnight = \"late\"\nfoo = 1\n\n[time]\nzone = \"Mars/Olympus\"\n\n[[tile]]\nentity = \"nope\"\n\n[[tile]]\nentity = \"light.a\"\nname = \"\"\ncolor = 1\n";
        let (c, issues) = Config::parse(text).unwrap();
        assert_eq!(c.display.brightness, 100);
        assert_eq!(c.display.night, None);
        assert_eq!(c.time.zone, touchos_civil::Tz::UTC);
        assert_eq!(
            c.tiles,
            alloc::vec![Tile {
                entity: "light.a".into(),
                name: None
            }]
        );
        let fields: Vec<&str> = issues.iter().map(|i| i.field.as_str()).collect();
        assert_eq!(
            fields,
            [
                "display.brightness",
                "display.night",
                "time.zone",
                "display.foo",
                "tile[1].entity",
                "tile[2].color"
            ]
        );
    }

    #[test]
    fn tls_default_follows_port() {
        let (c, _) = Config::parse("[homeassistant]\nport = 443\n").unwrap();
        assert!(c.homeassistant.use_tls());
        let (c, _) = Config::parse("[homeassistant]\nport = 443\ntls = false\n").unwrap();
        assert!(!c.homeassistant.use_tls());
    }

    #[test]
    fn time_zones() {
        let (c, issues) = Config::parse("[time]\nzone = \"Europe/Amsterdam\"\n").unwrap();
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(c.time.zone.std, 3600);
        assert!(c.time.zone.dst.is_some());
        // A POSIX rule works too; the case of a name is corrected.
        let (c, issues) = Config::parse("[time]\nzone = \"<+0545>-5:45\"\n").unwrap();
        assert!(issues.is_empty() && c.time.zone.std == 20_700);
        let (c, issues) = Config::parse("[time]\nzone = \"america/new_york\"\n").unwrap();
        assert_eq!(c.time.zone.std, -18_000);
        assert_eq!(issues[0].severity, Severity::Warning);
        // The old settings say what replaced them.
        let (c, issues) = Config::parse("[display]\nutc_offset_minutes = 60\ndst = \"eu\"\n").unwrap();
        assert_eq!(c.time.zone, touchos_civil::Tz::UTC);
        assert_eq!(issues.len(), 2);
        assert!(issues.iter().all(|i| i.msg.contains("time.zone")), "{issues:?}");
    }

    #[test]
    fn net() {
        let c = Config::default();
        assert_eq!(c.net.hostname, None);
        assert!(c.net.mdns);
        let (c, issues) = Config::parse("[net]\nhostname = \"Hal\"\nmdns = false\n").unwrap();
        assert_eq!(c.net.hostname.as_deref(), Some("hal"));
        assert!(!c.net.mdns);
        assert_eq!(issues.len(), 1, "{issues:?}");
        let (c, issues) = Config::parse("[net]\nhostname = \"a.local\"\n").unwrap();
        assert_eq!(c.net.hostname, None);
        assert_eq!(issues[0].severity, Severity::Error);
    }

    #[test]
    fn empty_rooms_means_all() {
        let (c, _) = Config::parse("[dashboard]\nrooms = []\n").unwrap();
        assert_eq!(c.dashboard.rooms, None);
    }
}
