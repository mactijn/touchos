//! `cargo xtask tzdata [DIR] [--check]`: the rules of every IANA time
//! zone (`libs/civil/src/zones.rs`), from the host's compiled tzdata.
//!
//! Every TZif file (version 2 and up) ends in the POSIX TZ rule for the
//! times after its table: the zone's current rule. The names come from
//! `tzdata.zi` in the same directory: its zones (`Z`) and links (`L`), so
//! old names (`Asia/Calcutta`, `US/Eastern`) still work. The picker on the
//! settings page lists only the zones.
//!
//! `--check` writes nothing; it says whether the file is what the host's
//! tzdata gives. `cargo xtask check` runs that and warns.

use anyhow::{Context, Result, bail};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use crate::root;

/// Where Debian and Ubuntu (and most others) keep the compiled zones.
pub const DEFAULT_DIR: &str = "/usr/share/zoneinfo";

fn out_path() -> PathBuf {
    root().join("libs/civil/src/zones.rs")
}

pub fn run(dir: Option<&str>, check: bool) -> Result<()> {
    let dir = Path::new(dir.unwrap_or(DEFAULT_DIR));
    let made = make(dir)?;
    for s in &made.skipped {
        println!("skipped: {s}");
    }
    if check {
        if current()? == made.text {
            println!("libs/civil/src/zones.rs is up to date with tzdata {} ({} names)", made.version, made.count);
            return Ok(());
        }
        bail!(
            "libs/civil/src/zones.rs (tzdata {}) is not what {} (tzdata {}) gives; run `cargo xtask tzdata`",
            touchos_civil::TZDATA_VERSION,
            dir.display(),
            made.version
        );
    }
    std::fs::write(out_path(), &made.text)?;
    println!("wrote libs/civil/src/zones.rs: tzdata {}, {} names", made.version, made.count);
    Ok(())
}

/// For `cargo xtask check`: what is worth a warning (nothing if the host
/// has no tzdata to compare with).
pub fn warnings() -> Vec<String> {
    let dir = Path::new(DEFAULT_DIR);
    if !dir.join("tzdata.zi").exists() {
        return vec![format!("note: no {DEFAULT_DIR}/tzdata.zi; zones.rs not compared")];
    }
    match make(dir).and_then(|m| Ok((current()? == m.text, m.version))) {
        Ok((true, _)) => Vec::new(),
        Ok((false, v)) => vec![format!(
            "tzdata: libs/civil/src/zones.rs (tzdata {}) differs from the host's tzdata {v}; run `cargo xtask tzdata`",
            touchos_civil::TZDATA_VERSION
        )],
        Err(e) => vec![format!("tzdata: {e:#}")],
    }
}

fn current() -> Result<String> {
    std::fs::read_to_string(out_path()).context("reading libs/civil/src/zones.rs")
}

struct Made {
    text: String,
    version: String,
    count: usize,
    skipped: Vec<String>,
}

fn make(dir: &Path) -> Result<Made> {
    let zi_path = dir.join("tzdata.zi");
    let zi = std::fs::read_to_string(&zi_path).with_context(|| format!("reading {}", zi_path.display()))?;
    let (version, names) = names(&zi)?;
    let mut skipped = Vec::new();
    let mut zones: Vec<(String, String, bool)> = Vec::new();
    for (name, target) in &names {
        // A link's own file may be missing (Debian moved the old names to
        // tzdata-legacy): its target has the same rule.
        let read = |n: &str| std::fs::read(dir.join(n));
        let tzif = match read(name).or_else(|e| target.as_deref().map_or(Err(e), read)) {
            Ok(b) => b,
            Err(e) => {
                skipped.push(format!("{name}: {e}"));
                continue;
            }
        };
        let Some(rule) = footer(&tzif) else {
            skipped.push(format!("{name}: no TZ rule (not a TZif v2+ file)"));
            continue;
        };
        if rule.is_empty() {
            skipped.push(format!("{name}: no current rule"));
            continue;
        }
        if touchos_civil::Tz::parse(&rule).is_none() {
            bail!("{name}: can't use its rule {rule:?}");
        }
        let listed = name == "UTC" || (target.is_none() && name.contains('/') && !name.starts_with("Etc/"));
        zones.push((name.clone(), rule, listed));
    }
    zones.sort();
    zones.dedup_by(|a, b| a.0 == b.0);
    if !zones.iter().any(|z| z.0 == "UTC") {
        bail!("{} has no UTC", dir.display());
    }
    Ok(Made { text: render(&version, &zones), version, count: zones.len(), skipped })
}

/// tzdata.zi -> (version, [(name, the target if it is a link)]).
fn names(zi: &str) -> Result<(String, Vec<(String, Option<String>)>)> {
    let mut version = None;
    let mut names = Vec::new();
    for line in zi.lines() {
        let mut w = line.split_whitespace();
        match w.next() {
            Some("#") if version.is_none() => {
                if w.next() == Some("version") {
                    version = w.next().map(str::to_string);
                }
            }
            Some("Z") => names.extend(w.next().map(|n| (n.to_string(), None))),
            Some("L") => {
                if let (Some(target), Some(n)) = (w.next(), w.next()) {
                    names.push((n.to_string(), Some(target.to_string())));
                }
            }
            _ => {}
        }
    }
    let version = version.context("tzdata.zi has no `# version` line")?;
    if names.is_empty() {
        bail!("tzdata.zi names no zones");
    }
    Ok((version, names))
}

/// The POSIX TZ rule at the end of a TZif v2+ file (RFC 8536 3.3):
/// `\n<rule>\n`. `None` if it isn't one.
fn footer(tzif: &[u8]) -> Option<String> {
    if !tzif.starts_with(b"TZif") || tzif.get(4).is_none_or(|v| *v < b'2') || tzif.last() != Some(&b'\n') {
        return None;
    }
    let body = &tzif[..tzif.len() - 1];
    let start = body.iter().rposition(|b| *b == b'\n')? + 1;
    String::from_utf8(body[start..].to_vec()).ok().filter(|s| s.is_ascii())
}

fn render(version: &str, zones: &[(String, String, bool)]) -> String {
    let mut s = String::new();
    s.push_str("// Made by `cargo xtask tzdata` from the host's compiled tzdata: do not edit.\n\n");
    let _ = writeln!(s, "pub const TZDATA_VERSION: &str = {version:?};\n");
    s.push_str("/// (name, POSIX TZ rule, listed), sorted by name.\n");
    s.push_str("pub static ZONES: &[(&str, &str, bool)] = &[\n");
    for (name, rule, listed) in zones {
        let _ = writeln!(s, "    ({name:?}, {rule:?}, {listed}),");
    }
    s.push_str("];\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tzdata_zi_names() {
        let zi = "# version 2026e\n# redo posix_only\nR E 1981 ma - Mar lastSu 1u 1 S\nZ Europe/Amsterdam 0:19:32 - LMT 1835\n0:20 - AMT 1937\nL Europe/Amsterdam Europe/Brussels\nZ UTC 0 - UTC\n";
        let (v, n) = names(zi).unwrap();
        assert_eq!(v, "2026e");
        assert_eq!(
            n,
            [
                ("Europe/Amsterdam".to_string(), None),
                ("Europe/Brussels".to_string(), Some("Europe/Amsterdam".to_string())),
                ("UTC".to_string(), None)
            ]
        );
        assert!(names("Z UTC 0 - UTC\n").is_err());
    }

    #[test]
    fn tzif_footer() {
        let mut f = b"TZif2".to_vec();
        f.extend_from_slice(&[0; 40]);
        f.extend_from_slice(b"\nCET-1CEST,M3.5.0,M10.5.0/3\n");
        assert_eq!(footer(&f).as_deref(), Some("CET-1CEST,M3.5.0,M10.5.0/3"));
        f[4] = 0; // version 1: no footer
        assert_eq!(footer(&f), None);
        assert_eq!(footer(b"TZif2\n\n").as_deref(), Some(""));
    }

    #[test]
    fn checked_in_table_is_sorted_and_usable() {
        assert!(!touchos_civil::ZONES.is_empty(), "run `cargo xtask tzdata`");
        assert!(touchos_civil::ZONES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(touchos_civil::listed().count() > 300);
    }
}
