//! Every setting in touchos.toml, described once.
//!
//! The typed [`crate::Config`], validation, and the settings page's form
//! (the web server's /api/schema) all come from [`FIELDS`] and
//! [`TILE_FIELDS`].

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::toml::Value;

/// What kind of value a setting holds, with its limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An integer; values outside the range are clamped (with a warning).
    Int {
        min: i64,
        max: i64,
    },
    Bool,
    /// Free text up to `max` characters.
    Str {
        max: usize,
    },
    /// A Home Assistant long-lived access token. Never shown back to the web UI.
    Token,
    /// A WPA2 passphrase (8..=63 printable ASCII) or raw key (64 hex digits).
    /// Never shown back to the web UI.
    Psk,
    /// One of these words. An integer is accepted when it matches one
    /// (`rotation = 90` means `rotation = "90"`).
    Enum(&'static [&'static str]),
    /// A list of Home Assistant domains (`light`, `media_player`, ...).
    DomainList,
    /// A list of free-text names.
    NameList,
    /// A hostname or IPv4 address (no scheme, no port).
    Host,
    /// `IPv4:port`.
    Endpoint,
    /// `HH:MM-HH:MM` (may wrap past midnight).
    TimeRange,
    /// `domain.object_id`.
    EntityId,
}

impl Kind {
    pub fn is_secret(self) -> bool {
        matches!(self, Kind::Token | Kind::Psk)
    }
}

/// A service that has to restart to pick up a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Ha,
    Wifi,
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    Live,
    Restart(Service),
    Reboot,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DefaultValue {
    None,
    Int(i64),
    Bool(bool),
    Str(&'static str),
    List(&'static [&'static str]),
}

impl DefaultValue {
    pub fn value(self) -> Option<Value> {
        match self {
            DefaultValue::None => None,
            DefaultValue::Int(i) => Some(Value::Int(i)),
            DefaultValue::Bool(b) => Some(Value::Bool(b)),
            DefaultValue::Str(s) => Some(Value::Str(s.to_string())),
            DefaultValue::List(l) => Some(Value::List(
                l.iter().map(|s| Value::Str(s.to_string())).collect(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Field {
    /// `"section.key"`.
    pub id: &'static str,
    pub kind: Kind,
    pub default: DefaultValue,
    pub label: &'static str,
    pub help: &'static str,
    pub apply: Apply,
    /// Developer settings that the web UI doesn't show.
    pub hidden: bool,
}

impl Field {
    pub fn section(&self) -> &'static str {
        self.id.split_once('.').map_or("", |(s, _)| s)
    }
    pub fn key(&self) -> &'static str {
        self.id.split_once('.').map_or(self.id, |(_, k)| k)
    }
}

pub const DEFAULT_TYPES: &[&str] = &[
    "light",
    "switch",
    "climate",
    "fan",
    "cover",
    "lock",
    "media_player",
    "sensor",
];

const fn f(
    id: &'static str,
    kind: Kind,
    default: DefaultValue,
    label: &'static str,
    help: &'static str,
    apply: Apply,
) -> Field {
    Field {
        id,
        kind,
        default,
        label,
        help,
        apply,
        hidden: false,
    }
}

const fn hidden(mut field: Field) -> Field {
    field.hidden = true;
    field
}

use Apply::{Live, Reboot, Restart};
use DefaultValue as D;

pub static FIELDS: &[Field] = &[
    // [homeassistant]
    f(
        "homeassistant.host",
        Kind::Host,
        D::None,
        "Host",
        "IP address (or DNS name) of Home Assistant. The panel can't resolve .local names.",
        Restart(Service::Ha),
    ),
    f(
        "homeassistant.port",
        Kind::Int { min: 1, max: 65535 },
        D::Int(8123),
        "Port",
        "Usually 8123, or 443 behind a TLS proxy.",
        Restart(Service::Ha),
    ),
    f(
        "homeassistant.token",
        Kind::Token,
        D::None,
        "Access token",
        "A long-lived access token: in Home Assistant, Profile -> Security.",
        Restart(Service::Ha),
    ),
    f(
        "homeassistant.tls",
        Kind::Bool,
        D::None,
        "Use TLS",
        "Connect with TLS (wss://). Default: only when the port is 443.",
        Restart(Service::Ha),
    ),
    // [wifi]
    f(
        "wifi.ssid",
        Kind::Str { max: 32 },
        D::None,
        "Network name",
        "The Wi-Fi network (SSID). Only WPA2-PSK with AES is supported.",
        Restart(Service::Wifi),
    ),
    f(
        "wifi.psk",
        Kind::Psk,
        D::None,
        "Password",
        "The Wi-Fi password (8-63 characters).",
        Restart(Service::Wifi),
    ),
    // [display]
    f(
        "display.brightness",
        Kind::Int { min: 0, max: 100 },
        D::Int(80),
        "Brightness",
        "Backlight in percent while the panel is in use.",
        Live,
    ),
    f(
        "display.dim_brightness",
        Kind::Int { min: 0, max: 100 },
        D::Int(15),
        "Idle brightness",
        "Backlight in percent after a while without touches.",
        Live,
    ),
    f(
        "display.dim_after_seconds",
        Kind::Int {
            min: 5,
            max: 86_400,
        },
        D::Int(60),
        "Dim after (seconds)",
        "How long without touches before the screen dims.",
        Live,
    ),
    f(
        "display.night",
        Kind::TimeRange,
        D::None,
        "Night hours",
        "Switch the screen off when idle during these hours, e.g. 23:00-07:00. A touch wakes it.",
        Live,
    ),
    f(
        "display.rotation",
        Kind::Enum(&["auto", "0", "90", "180", "270"]),
        D::Str("auto"),
        "Rotation",
        "\"auto\" follows an accelerometer where the board has one (the Toon has none: as 0).",
        Live,
    ),
    f(
        "display.utc_offset_minutes",
        Kind::Int {
            min: -720,
            max: 840,
        },
        D::Int(60),
        "UTC offset (minutes)",
        "Time zone: 60 for Central European Time.",
        Live,
    ),
    f(
        "display.dst",
        Kind::Enum(&["eu", "none"]),
        D::Str("eu"),
        "Summer time",
        "\"eu\": European summer time rules. \"none\": no summer time.",
        Live,
    ),
    hidden(f(
        "display.ui",
        Kind::Enum(&["on", "off"]),
        D::Str("on"),
        "Dashboard",
        "\"off\" keeps the text console on screen (for debugging).",
        Reboot,
    )),
    // [time]
    f(
        "time.ntp_server",
        Kind::Host,
        D::Str("time.google.com"),
        "NTP server",
        "Where the panel gets the time from when the network's own time server (from DHCP) doesn't answer, or there is none.",
        Live,
    ),
    // [dashboard]
    f(
        "dashboard.types",
        Kind::DomainList,
        D::List(DEFAULT_TYPES),
        "Entity types",
        "Which kinds of entities get tiles when no tiles are listed.",
        Live,
    ),
    f(
        "dashboard.rooms",
        Kind::NameList,
        D::None,
        "Rooms",
        "Which Home Assistant areas to show, in this order. \"Other\" = entities without an area. Empty: all.",
        Live,
    ),
    // [debug]
    f(
        "debug.log_host",
        Kind::Endpoint,
        D::None,
        "Log host",
        "Send the log as UDP lines to IP:port (`cargo xtask log` receives them).",
        Live,
    ),
    f(
        "debug.log_pin",
        Kind::Bool,
        D::Bool(false),
        "Log pairing codes",
        "Also write each pairing code to the log, so `cargo xtask pair` can pair without looking at the screen. The log travels over the network unencrypted: for development only.",
        Live,
    ),
    hidden(f(
        "debug.http_test",
        Kind::Str { max: 256 },
        D::None,
        "HTTP test URL",
        "Fetch this URL once after boot and log the result.",
        Reboot,
    )),
    // [web]
    f(
        "web.enabled",
        Kind::Bool,
        D::Bool(true),
        "Web interface",
        "Serve the settings page on the panel's IP address.",
        Reboot,
    ),
    f(
        "web.port",
        Kind::Int { min: 1, max: 65535 },
        D::Int(80),
        "Web port",
        "TCP port of the settings page.",
        Reboot,
    ),
    f(
        "web.session_hours",
        Kind::Int { min: 1, max: 8760 },
        D::Int(168),
        "Session length (hours)",
        "How long a paired browser stays logged in.",
        Live,
    ),
];

/// Keys of each `[[tile]]`.
pub static TILE_FIELDS: &[Field] = &[
    f(
        "tile.entity",
        Kind::EntityId,
        D::None,
        "Entity",
        "Home Assistant entity id, e.g. light.kitchen.",
        Live,
    ),
    f(
        "tile.name",
        Kind::Str { max: 40 },
        D::None,
        "Name",
        "Label on the tile. Empty: the entity's name in Home Assistant.",
        Live,
    ),
];

pub const MAX_TILES: usize = 64;
pub const MAX_FILE_BYTES: usize = 32 * 1024;

pub fn field(id: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.id == id)
}

/// Result of checking one value against its [`Kind`].
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    Ok(Value),
    /// Usable after a correction (clamped, normalized), with a warning.
    Fixed(Value, String),
    Bad(String),
}

pub fn check(kind: Kind, v: &Value) -> Check {
    use Check::*;
    match kind {
        Kind::Int { min, max } => match v {
            Value::Int(i) if (min..=max).contains(i) => Ok(v.clone()),
            Value::Int(i) => Fixed(
                Value::Int((*i).clamp(min, max)),
                format!(
                    "{i} is outside {min}..{max}; using {}",
                    (*i).clamp(min, max)
                ),
            ),
            _ => Bad("expected a number".into()),
        },
        Kind::Bool => match v {
            Value::Bool(_) => Ok(v.clone()),
            _ => Bad("expected true or false".into()),
        },
        Kind::Str { max } => match v {
            Value::Str(s) if s.chars().count() > max => {
                Bad(format!("longer than {max} characters"))
            }
            Value::Str(s) if s.chars().any(char::is_control) => {
                Bad("contains control characters".into())
            }
            Value::Str(_) => Ok(v.clone()),
            _ => Bad("expected text in quotes".into()),
        },
        Kind::Token => match v {
            Value::Str(s) if s.is_empty() => Bad("empty".into()),
            Value::Str(s) if s.starts_with("PASTE") => Bad("still the placeholder".into()),
            Value::Str(s) if s.chars().any(|c| c.is_whitespace() || c.is_control()) => {
                Bad("contains spaces".into())
            }
            Value::Str(_) => Ok(v.clone()),
            _ => Bad("expected text in quotes".into()),
        },
        Kind::Psk => match v {
            Value::Str(s) if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) => {
                Ok(v.clone())
            }
            Value::Str(s) if !(8..=63).contains(&s.len()) => {
                Bad("must be 8 to 63 characters (or 64 hex digits)".into())
            }
            Value::Str(s) if !s.bytes().all(|b| (0x20..0x7f).contains(&b)) => {
                Bad("only printable ASCII characters are allowed".into())
            }
            Value::Str(_) => Ok(v.clone()),
            _ => Bad("expected text in quotes".into()),
        },
        Kind::Enum(opts) => {
            let s = match v {
                Value::Str(s) => s.clone(),
                Value::Int(i) => i.to_string(),
                _ => return Bad(format!("expected one of {}", opts.join(", "))),
            };
            if opts.contains(&s.as_str()) {
                match v {
                    Value::Str(_) => Ok(v.clone()),
                    _ => Ok(Value::Str(s)),
                }
            } else {
                Bad(format!("expected one of {}", opts.join(", ")))
            }
        }
        Kind::DomainList | Kind::NameList => {
            let Value::List(items) = v else {
                return Bad("expected a list like [\"a\", \"b\"]".into());
            };
            for it in items {
                match it {
                    Value::Str(s) if kind == Kind::DomainList && !is_ident(s) => {
                        return Bad(format!("\"{s}\" is not an entity type"));
                    }
                    Value::Str(s) if s.is_empty() || s.chars().count() > 64 => {
                        return Bad("names must be 1 to 64 characters".into());
                    }
                    Value::Str(_) => {}
                    _ => return Bad("list items must be text in quotes".into()),
                }
            }
            Ok(v.clone())
        }
        Kind::Host => match v {
            Value::Str(s) if is_host(s) => Ok(v.clone()),
            Value::Str(s) if s.contains("://") || s.contains(':') || s.contains('/') => {
                Bad("just the host name or IP address, without http:// or a port".into())
            }
            Value::Str(_) => Bad("not a valid host name or IP address".into()),
            _ => Bad("expected text in quotes".into()),
        },
        Kind::Endpoint => match v {
            Value::Str(s) if parse_endpoint(s).is_some() => Ok(v.clone()),
            _ => Bad("expected IP:port, e.g. \"192.168.1.20:5555\"".into()),
        },
        Kind::TimeRange => match v {
            Value::Str(s) if parse_time_range(s).is_some() => Ok(v.clone()),
            _ => Bad("expected HH:MM-HH:MM, e.g. \"23:00-07:00\"".into()),
        },
        Kind::EntityId => match v {
            Value::Str(s) if is_entity_id(s) => Ok(v.clone()),
            _ => Bad("expected an entity id like light.kitchen".into()),
        },
    }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub fn is_entity_id(s: &str) -> bool {
    s.split_once('.')
        .is_some_and(|(d, o)| is_ident(d) && is_ident(o))
}

fn is_host(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 253
        && s.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for b in &mut out {
        let p = parts.next()?;
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        *b = p.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

/// `"a.b.c.d:port"`.
pub fn parse_endpoint(s: &str) -> Option<([u8; 4], u16)> {
    let (ip, port) = s.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    (port != 0).then_some((parse_ipv4(ip)?, port))
}

fn parse_hhmm(s: &str) -> Option<u16> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// `"HH:MM-HH:MM"` -> (start, end) in minutes after midnight.
pub fn parse_time_range(s: &str) -> Option<(u16, u16)> {
    let (a, b) = s.split_once('-')?;
    Some((parse_hhmm(a)?, parse_hhmm(b)?))
}

/// All field ids, for tests and the web UI.
pub fn ids() -> Vec<&'static str> {
    FIELDS.iter().map(|f| f.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> Value {
        Value::Str(x.into())
    }

    #[test]
    fn defaults_pass_their_own_checks() {
        for fld in FIELDS.iter().chain(TILE_FIELDS) {
            if let Some(v) = fld.default.value() {
                assert_eq!(check(fld.kind, &v), Check::Ok(v), "{}", fld.id);
            }
        }
    }

    #[test]
    fn ids_are_unique_and_well_formed() {
        let mut ids = ids();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
        for f in FIELDS {
            assert!(!f.section().is_empty() && !f.key().is_empty(), "{}", f.id);
        }
    }

    #[test]
    fn ints_clamp() {
        let k = Kind::Int { min: 0, max: 100 };
        assert_eq!(check(k, &Value::Int(50)), Check::Ok(Value::Int(50)));
        assert!(matches!(
            check(k, &Value::Int(150)),
            Check::Fixed(Value::Int(100), _)
        ));
        assert!(matches!(check(k, &s("50")), Check::Bad(_)));
    }

    #[test]
    fn rotation_accepts_integers() {
        let k = field("display.rotation").unwrap().kind;
        assert_eq!(check(k, &Value::Int(0)), Check::Ok(s("0")));
        assert_eq!(check(k, &Value::Int(90)), Check::Ok(s("90")));
        assert!(matches!(check(k, &Value::Int(45)), Check::Bad(_)));
        assert_eq!(check(k, &s("auto")), Check::Ok(s("auto")));
    }

    #[test]
    fn secrets() {
        assert!(matches!(
            check(Kind::Token, &s("PASTE-TOKEN-HERE")),
            Check::Bad(_)
        ));
        assert!(matches!(check(Kind::Token, &s("")), Check::Bad(_)));
        assert!(matches!(
            check(Kind::Token, &s("eyJ0eXAi.abc")),
            Check::Ok(_)
        ));
        assert!(matches!(check(Kind::Psk, &s("short")), Check::Bad(_)));
        assert!(matches!(
            check(Kind::Psk, &s("a \"tricky\" #\\pass")),
            Check::Ok(_)
        ));
        assert!(matches!(
            check(Kind::Psk, &s(&"ab".repeat(32))),
            Check::Ok(_)
        ));
        assert!(matches!(
            check(Kind::Psk, &s(&"x".repeat(64))),
            Check::Bad(_)
        ));
        assert!(Kind::Token.is_secret() && Kind::Psk.is_secret() && !Kind::Host.is_secret());
    }

    #[test]
    fn hosts_endpoints_times_entities() {
        assert!(matches!(
            check(Kind::Host, &s("192.168.1.10")),
            Check::Ok(_)
        ));
        assert!(matches!(
            check(Kind::Host, &s("ha.example.org")),
            Check::Ok(_)
        ));
        assert!(matches!(
            check(Kind::Host, &s("http://ha:8123")),
            Check::Bad(_)
        ));
        assert_eq!(parse_endpoint("10.0.2.2:5555"), Some(([10, 0, 2, 2], 5555)));
        assert_eq!(parse_endpoint("10.0.2:5555"), None);
        assert_eq!(parse_endpoint("10.0.2.256:1"), None);
        assert_eq!(parse_time_range("23:00-07:30"), Some((1380, 450)));
        assert_eq!(parse_time_range("24:00-07:00"), None);
        assert!(is_entity_id("light.kitchen_2"));
        assert!(!is_entity_id("Light.kitchen") && !is_entity_id("light") && !is_entity_id("a.b.c"));
    }
}
