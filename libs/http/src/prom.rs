//! The Prometheus text exposition format (version 0.0.4): samples are
//! collected in any order and written grouped by metric, each with its
//! `# HELP` and `# TYPE` line once.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The `Content-Type` of the format.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Gauge,
}

struct Family {
    kind: Kind,
    help: &'static str,
    lines: Vec<String>,
}

#[derive(Default)]
pub struct Writer {
    families: BTreeMap<String, Family>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer::default()
    }

    /// Add one sample. A metric's type and help come from its first
    /// sample. Values are written as integers when they are whole.
    pub fn sample(&mut self, name: &str, kind: Kind, help: &'static str, labels: &[(&str, &str)], value: f64) {
        let f = self
            .families
            .entry(name.into())
            .or_insert_with(|| Family { kind, help, lines: Vec::new() });
        let mut line = String::from(name);
        if !labels.is_empty() {
            line.push('{');
            for (i, (k, v)) in labels.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                let _ = write!(line, "{k}=\"");
                escape(&mut line, v, true);
                line.push('"');
            }
            line.push('}');
        }
        line.push(' ');
        number(&mut line, value);
        f.lines.push(line);
    }

    pub fn finish(self) -> String {
        let mut out = String::new();
        for (name, f) in self.families {
            out.push_str("# HELP ");
            out.push_str(&name);
            out.push(' ');
            escape(&mut out, f.help, false);
            out.push('\n');
            let _ = writeln!(out, "# TYPE {name} {}", if f.kind == Kind::Counter { "counter" } else { "gauge" });
            for l in f.lines {
                out.push_str(&l);
                out.push('\n');
            }
        }
        out
    }
}

/// Backslash and newline are escaped in help texts; in label values also
/// the double quote.
fn escape(out: &mut String, s: &str, quote: bool) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '"' if quote => out.push_str("\\\""),
            c => out.push(c),
        }
    }
}

fn number(out: &mut String, v: f64) {
    if v.is_nan() {
        out.push_str("NaN");
    } else if v.is_infinite() {
        out.push_str(if v > 0.0 { "+Inf" } else { "-Inf" });
    } else if v == (v as i64) as f64 && v.abs() < 9.0e15 {
        let _ = write!(out, "{}", v as i64);
    } else {
        let _ = write!(out, "{v}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_with_help_and_type_once() {
        let mut w = Writer::new();
        w.sample("b_total", Kind::Counter, "B things.", &[("x", "1")], 3.0);
        w.sample("a", Kind::Gauge, "A \\ thing\nnext", &[], 0.5);
        w.sample("b_total", Kind::Counter, "ignored", &[("x", "2")], 4.0);
        assert_eq!(
            w.finish(),
            "# HELP a A \\\\ thing\\nnext\n# TYPE a gauge\na 0.5\n\
             # HELP b_total B things.\n# TYPE b_total counter\nb_total{x=\"1\"} 3\nb_total{x=\"2\"} 4\n"
        );
    }

    #[test]
    fn label_values_and_numbers() {
        let mut w = Writer::new();
        w.sample("m", Kind::Gauge, "h", &[("name", "a\"b\\c\nd"), ("k", "")], -61.0);
        w.sample("m", Kind::Gauge, "h", &[], 1e20);
        w.sample("m", Kind::Gauge, "h", &[], 0.000123);
        w.sample("m", Kind::Gauge, "h", &[], f64::NAN);
        w.sample("m", Kind::Gauge, "h", &[], f64::NEG_INFINITY);
        let out = w.finish();
        let lines: Vec<&str> = out.lines().skip(2).collect();
        assert_eq!(
            lines,
            [
                "m{name=\"a\\\"b\\\\c\\nd\",k=\"\"} -61",
                "m 100000000000000000000",
                "m 0.000123",
                "m NaN",
                "m -Inf"
            ]
        );
    }

    #[test]
    fn empty() {
        assert_eq!(Writer::new().finish(), "");
    }
}
