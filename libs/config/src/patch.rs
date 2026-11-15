//! Changing settings: a small text patch format, checked against the schema
//! and applied to a [`Document`] (so the file keeps its comments).
//!
//! ```text
//! display.brightness = 60
//! wifi.psk = "a new password"
//! -display.night                      # remove a setting (back to default)
//! tiles = [["light.kitchen", "Kitchen"], ["sensor.outside", ""]]
//! ```

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::schema::{self, Apply, Check, MAX_TILES, Service, TILE_FIELDS};
use crate::toml::{Document, Value};
use crate::typed::{Config, Severity};

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Set(String, Value),
    Unset(String),
    /// Replace all `[[tile]]` entries: (entity, name).
    Tiles(Vec<(String, String)>),
}

/// What has to happen for a change to take effect.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Effects {
    pub restart_ha: bool,
    pub restart_wifi: bool,
    pub reboot: bool,
}

impl Effects {
    pub fn bits(self) -> u32 {
        self.restart_ha as u32
            | (self.restart_wifi as u32) << 1
            | (self.reboot as u32) << 3 // (bit 2 was a web restart that nothing did)
    }
    pub fn from_bits(b: u32) -> Effects {
        Effects {
            restart_ha: b & 1 != 0,
            restart_wifi: b & 2 != 0,
            reboot: b & 8 != 0,
        }
    }
    fn add(&mut self, a: Apply) {
        match a {
            Apply::Live => {}
            Apply::Restart(Service::Ha) => self.restart_ha = true,
            Apply::Restart(Service::Wifi) => self.restart_wifi = true,
            Apply::Reboot => self.reboot = true,
        }
    }
}

/// A problem with one line of a patch, or with the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchError {
    pub field: String,
    pub msg: String,
}

impl core::fmt::Display for PatchError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}: {}", self.field, self.msg)
    }
}

fn err(field: &str, msg: impl Into<String>) -> PatchError {
    PatchError {
        field: field.to_string(),
        msg: msg.into(),
    }
}

/// Parse a patch (one operation per line; `#` comments and blank lines
/// are ignored).
pub fn parse(text: &str) -> Result<Vec<Op>, PatchError> {
    let mut ops = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(id) = line.strip_prefix('-') {
            ops.push(Op::Unset(id.trim().to_string()));
            continue;
        }
        let (id, v) = line
            .split_once('=')
            .ok_or_else(|| err(line, "expected `section.key = value`"))?;
        let id = id.trim();
        let doc = Document::parse(&format!("x = {}", v.trim())).map_err(|e| err(id, e.msg))?;
        let value = doc
            .get("", "x")
            .cloned()
            .ok_or_else(|| err(id, "no value"))?;
        if id == "tiles" {
            ops.push(Op::Tiles(parse_tiles(&value)?));
        } else {
            ops.push(Op::Set(id.to_string(), value));
        }
    }
    Ok(ops)
}

fn parse_tiles(v: &Value) -> Result<Vec<(String, String)>, PatchError> {
    let Value::List(items) = v else {
        return Err(err("tiles", "expected [[\"entity\", \"name\"], ...]"));
    };
    items
        .iter()
        .map(|it| match it {
            Value::List(pair) if pair.len() == 2 => match (&pair[0], &pair[1]) {
                (Value::Str(e), Value::Str(n)) => Ok((e.clone(), n.clone())),
                _ => Err(err("tiles", "entity and name must be text")),
            },
            _ => Err(err("tiles", "each tile is [\"entity\", \"name\"]")),
        })
        .collect()
}

/// The patch as text (the inverse of [`parse`]).
pub fn to_text(ops: &[Op]) -> String {
    let mut out = String::new();
    for op in ops {
        match op {
            Op::Set(id, v) => out.push_str(&format!("{id} = {}\n", v.to_toml())),
            Op::Unset(id) => out.push_str(&format!("-{id}\n")),
            Op::Tiles(t) => {
                let items: Vec<Value> = t
                    .iter()
                    .map(|(e, n)| {
                        Value::List(alloc::vec![Value::Str(e.clone()), Value::Str(n.clone())])
                    })
                    .collect();
                out.push_str(&format!("tiles = {}\n", Value::List(items).to_toml()));
            }
        }
    }
    out
}

/// Check every operation against the schema and apply them to a copy of
/// `doc`. All or nothing: on any error, nothing changes.
pub fn apply(doc: &Document, ops: &[Op]) -> Result<(Document, Effects), Vec<PatchError>> {
    let mut new = doc.clone();
    let mut fx = Effects::default();
    let mut errors = Vec::new();
    for op in ops {
        match op {
            Op::Set(id, value) => {
                let Some(f) = schema::field(id) else {
                    errors.push(err(id, "unknown setting"));
                    continue;
                };
                if f.kind.is_secret() && value.as_str() == Some(REDACTED) {
                    errors.push(err(id, "type the new value (the old one is never shown)"));
                    continue;
                }
                match schema::check(f.kind, value) {
                    Check::Ok(v) => {
                        new.set(f.section(), f.key(), v);
                        fx.add(f.apply);
                    }
                    // A patch must say exactly what it means: no clamping.
                    Check::Fixed(..) if let schema::Kind::Int { min, max } = f.kind => {
                        errors.push(err(id, format!("must be between {min} and {max}")))
                    }
                    Check::Fixed(_, msg) | Check::Bad(msg) => errors.push(err(id, msg)),
                }
            }
            Op::Unset(id) => match schema::field(id) {
                Some(f) => {
                    new.unset(f.section(), f.key());
                    fx.add(f.apply);
                }
                None => errors.push(err(id, "unknown setting")),
            },
            Op::Tiles(tiles) => {
                if tiles.len() > MAX_TILES {
                    errors.push(err("tiles", format!("at most {MAX_TILES} tiles")));
                    continue;
                }
                let mut items = Vec::new();
                for (n, (entity, name)) in tiles.iter().enumerate() {
                    let mut item = Vec::new();
                    for (f, v) in TILE_FIELDS.iter().zip([entity, name]) {
                        if f.key() == "name" && v.is_empty() {
                            continue;
                        }
                        match schema::check(f.kind, &Value::Str(v.clone())) {
                            Check::Ok(x) => item.push((f.key().to_string(), x)),
                            Check::Fixed(_, m) | Check::Bad(m) => {
                                errors.push(err(&format!("tiles[{}].{}", n + 1, f.key()), m))
                            }
                        }
                    }
                    items.push(item);
                }
                new.set_array("tile", &items);
            }
        }
    }
    if errors.is_empty() {
        // The file must not get new problems (old ones, e.g. a placeholder
        // token, don't block unrelated changes).
        let before: Vec<(String, String)> = match Config::parse(&doc.to_text()) {
            Ok((_, issues)) => issues.into_iter().map(|i| (i.field, i.msg)).collect(),
            Err(_) => Vec::new(),
        };
        match Config::parse(&new.to_text()) {
            Ok((_, issues)) => errors.extend(
                issues
                    .into_iter()
                    .filter(|i| i.severity == Severity::Error)
                    .filter(|i| !before.contains(&(i.field.clone(), i.msg.clone())))
                    .map(|i| err(&i.field, i.msg)),
            ),
            Err(e) => errors.push(err("", e.to_string())),
        }
    }
    if errors.is_empty() {
        Ok((new, fx))
    } else {
        Err(errors)
    }
}

/// What readers without the right to see secrets get instead of them.
pub const REDACTED: &str = "********";

/// A copy of `doc` with every secret that is set replaced by [`REDACTED`]
/// (so readers can tell "set" from "not set" without seeing the value).
pub fn redact(doc: &Document) -> Document {
    let mut out = doc.clone();
    for f in schema::FIELDS.iter().filter(|f| f.kind.is_secret()) {
        if out.get(f.section(), f.key()).is_some() {
            out.set(f.section(), f.key(), Value::Str(REDACTED.into()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../config/touchos.example.toml");

    fn doc() -> Document {
        Document::parse(EXAMPLE).unwrap()
    }

    #[test]
    fn parse_and_print() {
        let text = "display.brightness = 60\n-display.night\nwifi.psk = \"a \\\"b\\\" #c\"\ntiles = [[\"light.a\", \"A\"]]\n";
        let ops = parse(text).unwrap();
        assert_eq!(
            ops,
            alloc::vec![
                Op::Set("display.brightness".into(), Value::Int(60)),
                Op::Unset("display.night".into()),
                Op::Set("wifi.psk".into(), Value::Str("a \"b\" #c".into())),
                Op::Tiles(alloc::vec![("light.a".into(), "A".into())]),
            ]
        );
        assert_eq!(parse(&to_text(&ops)).unwrap(), ops);
    }

    #[test]
    fn apply_keeps_comments_and_reports_effects() {
        let ops = parse("display.brightness = 60\nhomeassistant.token = \"abc.def\"\n").unwrap();
        let (new, fx) = apply(&doc(), &ops).unwrap();
        let text = new.to_text();
        assert!(text.contains("brightness = 60"));
        assert!(text.contains("# Profile -> Security -> Long-lived access tokens"));
        assert_eq!(
            fx,
            Effects {
                restart_ha: true,
                ..Default::default()
            }
        );
        let (cfg, _) = Config::parse(&text).unwrap();
        assert_eq!(cfg.display.brightness, 60);
        assert_eq!(cfg.homeassistant.token.as_deref(), Some("abc.def"));
    }

    #[test]
    fn apply_is_all_or_nothing() {
        let ops =
            parse("display.brightness = 60\ndisplay.dim_brightness = 150\nfoo.bar = 1\n").unwrap();
        let errs = apply(&doc(), &ops).unwrap_err();
        let fields: Vec<&str> = errs.iter().map(|e| e.field.as_str()).collect();
        assert_eq!(fields, ["display.dim_brightness", "foo.bar"]);
    }

    #[test]
    fn tiles() {
        let ops = parse("tiles = [[\"light.k\", \"Kitchen\"], [\"sensor.t\", \"\"]]").unwrap();
        let (new, _) = apply(&doc(), &ops).unwrap();
        let (cfg, _) = Config::parse(&new.to_text()).unwrap();
        assert_eq!(cfg.tiles.len(), 2);
        assert_eq!(cfg.tiles[1].name, None);
        let bad = parse("tiles = [[\"Kitchen\", \"x\"]]").unwrap();
        assert_eq!(apply(&doc(), &bad).unwrap_err()[0].field, "tiles[1].entity");
    }

    #[test]
    fn redaction_removes_secrets() {
        let ops =
            parse("wifi.ssid = \"home\"\nwifi.psk = \"secret123\"\nhomeassistant.token = \"tok\"")
                .unwrap();
        let (new, _) = apply(&doc(), &ops).unwrap();
        let red = redact(&new).to_text();
        assert!(!red.contains("secret123") && !red.contains("\"tok\""));
        assert!(red.contains("ssid = \"home\""));
        let (cfg, _) = Config::parse(&red).unwrap();
        assert_eq!(cfg.wifi.psk.as_deref(), Some(REDACTED));
        assert_eq!(cfg.homeassistant.token.as_deref(), Some(REDACTED));
        // A secret that isn't set stays absent.
        let red = redact(&Document::parse("[wifi]\nssid = \"x\"\n").unwrap()).to_text();
        assert!(!red.contains("psk"));
        // The placeholder can't be saved as a value.
        let ops = parse("wifi.psk = \"********\"").unwrap();
        assert!(apply(&new, &ops).is_err());
    }

    #[test]
    fn effects_bits_round_trip() {
        let fx = Effects {
            restart_wifi: true,
            reboot: true,
            ..Default::default()
        };
        assert_eq!(Effects::from_bits(fx.bits()), fx);
    }
}
