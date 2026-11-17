//! A small TOML subset, parsed into a line-preserving [`Document`].
//!
//! Supported: `[section]`, `[[array]]`, `key = value` with basic strings
//! (`"..."`, escapes `\" \\ \n \t \r \uXXXX \UXXXXXXXX`), literal strings
//! (`'...'`, no escapes), integers (`_` separators allowed), booleans and
//! single-line lists; `#` comments anywhere a value has ended.
//!
//! Every line is kept verbatim. Editing a value rewrites only that value's
//! bytes, so comments and layout survive a save from the web UI.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
    List(Vec<Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The value as TOML source text.
    pub fn to_toml(&self) -> String {
        match self {
            Value::Str(s) => quote(s),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::List(items) => {
                let inner: Vec<String> = items.iter().map(Value::to_toml).collect();
                format!("[{}]", inner.join(", "))
            }
        }
    }
}

/// A TOML basic string with everything that needs it escaped.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub type Table = BTreeMap<String, Value>;

/// The settings as plain maps: `"section.key"` -> value, plus `[[name]]` arrays.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Raw {
    pub values: BTreeMap<String, Value>,
    pub arrays: BTreeMap<String, Vec<Table>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub line: usize,
    pub msg: &'static str,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Blank,
    Comment,
    Section(String),
    ArrayHeader(String),
    KeyValue {
        key: String,
        value: Value,
        /// Byte range of the value in the raw line.
        span: Range<usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct Line {
    raw: String,
    kind: Kind,
}

impl Line {
    fn new(raw: String) -> Line {
        let kind = parse_line(&raw).expect("generated line must parse");
        Line { raw, kind }
    }
    fn is_header(&self) -> bool {
        matches!(self.kind, Kind::Section(_) | Kind::ArrayHeader(_))
    }
}

/// A parsed touchos.toml that remembers every line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    lines: Vec<Line>,
}

impl Document {
    pub fn parse(text: &str) -> Result<Document, ParseError> {
        let mut lines = Vec::new();
        // `str::lines` drops a final empty line and handles "\r\n".
        for (n, raw) in text.lines().enumerate() {
            let kind = parse_line(raw).map_err(|msg| ParseError { line: n + 1, msg })?;
            lines.push(Line {
                raw: raw.to_string(),
                kind,
            });
        }
        let doc = Document { lines };
        doc.check_duplicates()?;
        Ok(doc)
    }

    /// TOML forbids defining a key or a `[section]` twice; we do too, so that
    /// editing a value can never be shadowed by a second copy of it.
    fn check_duplicates(&self) -> Result<(), ParseError> {
        let mut seen_keys = BTreeMap::new();
        let mut seen_sections = BTreeMap::new();
        let mut table = String::new();
        let mut in_array = false;
        for (i, l) in self.lines.iter().enumerate() {
            let err = |msg| ParseError { line: i + 1, msg };
            match &l.kind {
                Kind::Section(s) => {
                    if seen_sections.insert(s.clone(), ()).is_some() {
                        return Err(err("section defined twice"));
                    }
                    table = s.clone();
                    in_array = false;
                }
                Kind::ArrayHeader(a) => {
                    if seen_sections.contains_key(a) {
                        return Err(err("name used as both [section] and [[array]]"));
                    }
                    table = format!("[[{a}]]#{i}");
                    in_array = true;
                }
                Kind::KeyValue { key, .. } => {
                    let full = format!("{table}.{key}");
                    if seen_keys.insert(full, ()).is_some() {
                        return Err(err("key defined twice"));
                    }
                }
                _ => {}
            }
        }
        let _ = in_array;
        Ok(())
    }

    /// The whole file as text (always ends with a newline unless empty).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for l in &self.lines {
            out.push_str(&l.raw);
            out.push('\n');
        }
        out
    }

    /// All settings as maps.
    pub fn raw(&self) -> Raw {
        let mut raw = Raw::default();
        enum Target {
            Section(String),
            Array(String),
        }
        let mut target = Target::Section(String::new());
        for l in &self.lines {
            match &l.kind {
                Kind::Section(s) => target = Target::Section(s.clone()),
                Kind::ArrayHeader(a) => {
                    raw.arrays.entry(a.clone()).or_default().push(Table::new());
                    target = Target::Array(a.clone());
                }
                Kind::KeyValue { key, value, .. } => match &target {
                    Target::Section(s) if s.is_empty() => {
                        raw.values.insert(key.clone(), value.clone());
                    }
                    Target::Section(s) => {
                        raw.values.insert(format!("{s}.{key}"), value.clone());
                    }
                    Target::Array(a) => {
                        let t = raw.arrays.get_mut(a).unwrap().last_mut().unwrap();
                        t.insert(key.clone(), value.clone());
                    }
                },
                Kind::Blank | Kind::Comment => {}
            }
        }
        raw
    }

    /// Value of `key` in `[section]` ("" = top level, before any header).
    pub fn get(&self, section: &str, key: &str) -> Option<&Value> {
        let i = self.find_key(section, key)?;
        match &self.lines[i].kind {
            Kind::KeyValue { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Line range of `[section]`: header (if any) up to the next header.
    fn section_range(&self, section: &str) -> Option<Range<usize>> {
        let start = if section.is_empty() {
            0
        } else {
            self.lines
                .iter()
                .position(|l| matches!(&l.kind, Kind::Section(s) if s == section))?
        };
        let end = self.lines[start + usize::from(!section.is_empty())..]
            .iter()
            .position(Line::is_header)
            .map_or(self.lines.len(), |p| {
                p + start + usize::from(!section.is_empty())
            });
        Some(start..end)
    }

    fn find_key(&self, section: &str, key: &str) -> Option<usize> {
        self.section_range(section)?
            .find(|&i| matches!(&self.lines[i].kind, Kind::KeyValue { key: k, .. } if k == key))
    }

    /// Set `key` in `[section]`. Only the value's bytes change if the key
    /// exists. A new key goes after a matching `# key = ...` example comment
    /// or after the last key of the section; a new section goes before the
    /// first `[[array]]` block (or at the end).
    pub fn set(&mut self, section: &str, key: &str, value: Value) {
        let text = value.to_toml();
        if let Some(i) = self.find_key(section, key) {
            let line = &mut self.lines[i];
            if let Kind::KeyValue { span, .. } = &line.kind {
                let mut raw = String::new();
                raw.push_str(&line.raw[..span.start]);
                raw.push_str(&text);
                raw.push_str(&line.raw[span.end..]);
                *line = Line::new(raw);
            }
            return;
        }
        let new_line = Line::new(format!("{key} = {text}"));
        if let Some(r) = self.section_range(section) {
            let example = r.clone().find(|&i| {
                self.lines[i].kind == Kind::Comment && is_example_of(&self.lines[i].raw, key)
            });
            let last_kv = r
                .clone()
                .rfind(|&i| matches!(self.lines[i].kind, Kind::KeyValue { .. }));
            let at = match (example, last_kv) {
                (Some(e), _) => e + 1,
                (None, Some(k)) => k + 1,
                (None, None) if section.is_empty() => 0,
                (None, None) => r.start + 1,
            };
            self.lines.insert(at, new_line);
            return;
        }
        // New section: before the first [[array]] block and the comments
        // directly above it, or at the end.
        let mut at = self
            .lines
            .iter()
            .position(|l| matches!(l.kind, Kind::ArrayHeader(_)))
            .unwrap_or(self.lines.len());
        while at > 0 && self.lines[at - 1].kind == Kind::Comment {
            at -= 1;
        }
        let mut block = Vec::new();
        if at > 0 && self.lines[at - 1].kind != Kind::Blank {
            block.push(Line::new(String::new()));
        }
        block.push(Line::new(format!("[{section}]")));
        block.push(new_line);
        if at < self.lines.len() {
            block.push(Line::new(String::new()));
        }
        self.lines.splice(at..at, block);
    }

    /// Remove `key` from `[section]`. Returns whether it was there.
    pub fn unset(&mut self, section: &str, key: &str) -> bool {
        match self.find_key(section, key) {
            Some(i) => {
                self.lines.remove(i);
                true
            }
            None => false,
        }
    }

    /// Replace all `[[name]]` blocks with `items`, at the position of the
    /// first existing block (or at the end). Comments inside the old blocks
    /// are lost; comments above the first block and after the last are kept.
    pub fn set_array(&mut self, name: &str, items: &[Vec<(String, Value)>]) {
        // Blocks: header ..= last key of the block. For every block but the
        // first, blank/comment lines directly above the header belong to it.
        let mut blocks: Vec<Range<usize>> = Vec::new();
        for (h, l) in self.lines.iter().enumerate() {
            if !matches!(&l.kind, Kind::ArrayHeader(a) if a == name) {
                continue;
            }
            let mut start = h;
            if let Some(prev) = blocks.last() {
                while start > prev.end
                    && matches!(self.lines[start - 1].kind, Kind::Blank | Kind::Comment)
                {
                    start -= 1;
                }
            }
            let next_header = self.lines[h + 1..]
                .iter()
                .position(Line::is_header)
                .map_or(self.lines.len(), |p| p + h + 1);
            let end = (h + 1..next_header)
                .rfind(|&i| matches!(self.lines[i].kind, Kind::KeyValue { .. }))
                .map_or(h + 1, |k| k + 1);
            blocks.push(start..end);
        }
        let at_end = blocks.is_empty();
        let at = blocks.first().map_or(self.lines.len(), |b| b.start);
        for b in blocks.iter().rev() {
            self.lines.drain(b.clone());
        }

        let mut generated = Vec::new();
        if at_end && !items.is_empty() && at > 0 && self.lines[at - 1].kind != Kind::Blank {
            generated.push(Line::new(String::new()));
        }
        for (n, item) in items.iter().enumerate() {
            if n > 0 {
                generated.push(Line::new(String::new()));
            }
            generated.push(Line::new(format!("[[{name}]]")));
            for (k, v) in item {
                generated.push(Line::new(format!("{k} = {}", v.to_toml())));
            }
        }
        self.lines.splice(at..at, generated);
    }
}

/// `# key = ...` or `#key=...`: an example line for `key`.
fn is_example_of(comment: &str, key: &str) -> bool {
    let body = comment.trim_start().trim_start_matches('#').trim_start();
    body.strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn is_bare_key(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn is_table_name(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_bare_key)
}

/// After a value or header: only whitespace or a comment may follow.
fn trailing_ok(rest: &str) -> bool {
    let t = rest.trim_start();
    t.is_empty() || t.starts_with('#')
}

fn parse_line(raw: &str) -> Result<Kind, &'static str> {
    let t = raw.trim_start();
    let off = raw.len() - t.len();
    if t.is_empty() {
        return Ok(Kind::Blank);
    }
    if t.starts_with('#') {
        return Ok(Kind::Comment);
    }
    if let Some(inner) = t.strip_prefix("[[") {
        let end = inner.find("]]").ok_or("missing `]]`")?;
        let name = inner[..end].trim();
        if !is_table_name(name) {
            return Err("bad [[array]] name");
        }
        if !trailing_ok(&inner[end + 2..]) {
            return Err("unexpected text after `]]`");
        }
        return Ok(Kind::ArrayHeader(name.to_string()));
    }
    if let Some(inner) = t.strip_prefix('[') {
        let end = inner.find(']').ok_or("missing `]`")?;
        let name = inner[..end].trim();
        if !is_table_name(name) {
            return Err("bad [section] name");
        }
        if !trailing_ok(&inner[end + 1..]) {
            return Err("unexpected text after `]`");
        }
        return Ok(Kind::Section(name.to_string()));
    }
    let eq = t.find('=').ok_or("expected `key = value` or `[section]`")?;
    let key = t[..eq].trim();
    if !is_bare_key(key) {
        return Err("bad key (letters, digits, `_` and `-` only)");
    }
    let mut pos = off + eq + 1;
    skip_ws(raw, &mut pos);
    let start = pos;
    let value = parse_value(raw, &mut pos)?;
    if !trailing_ok(&raw[pos..]) {
        return Err("unexpected text after the value");
    }
    Ok(Kind::KeyValue {
        key: key.to_string(),
        value,
        span: start..pos,
    })
}

fn skip_ws(s: &str, pos: &mut usize) {
    while let Some(c) = s[*pos..].chars().next() {
        if c == ' ' || c == '\t' {
            *pos += 1;
        } else {
            break;
        }
    }
}

fn parse_value(s: &str, pos: &mut usize) -> Result<Value, &'static str> {
    match s[*pos..].chars().next() {
        Some('"') => parse_basic_string(s, pos),
        Some('\'') => {
            let rest = &s[*pos + 1..];
            let end = rest.find('\'').ok_or("unterminated string")?;
            let v = rest[..end].to_string();
            *pos += end + 2;
            Ok(Value::Str(v))
        }
        Some('[') => {
            *pos += 1;
            let mut items = vec![];
            loop {
                skip_ws(s, pos);
                if s[*pos..].starts_with(']') {
                    *pos += 1;
                    break;
                }
                items.push(parse_value(s, pos)?);
                skip_ws(s, pos);
                if s[*pos..].starts_with(',') {
                    *pos += 1;
                } else if s[*pos..].starts_with(']') {
                    *pos += 1;
                    break;
                } else {
                    return Err("expected `,` or `]` in list");
                }
            }
            Ok(Value::List(items))
        }
        Some(_) => {
            let rest = &s[*pos..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == ',' || c == ']' || c == '#')
                .unwrap_or(rest.len());
            let tok = &rest[..end];
            *pos += end;
            match tok {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => {
                    let digits = tok.strip_prefix(['+', '-']).unwrap_or(tok);
                    let clean: String = tok.chars().filter(|&c| c != '_').collect();
                    if digits.is_empty()
                        || digits.starts_with('_')
                        || digits.ends_with('_')
                        || digits.contains("__")
                    {
                        return Err("bad value (strings need quotes)");
                    }
                    clean
                        .parse::<i64>()
                        .map(Value::Int)
                        .map_err(|_| "bad value (strings need quotes)")
                }
            }
        }
        None => Err("missing value"),
    }
}

fn parse_basic_string(s: &str, pos: &mut usize) -> Result<Value, &'static str> {
    let body = &s[*pos + 1..];
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                *pos += 1 + i + 1;
                return Ok(Value::Str(out));
            }
            '\\' => {
                let (_, e) = chars.next().ok_or("unterminated string")?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'u' | 'U' => {
                        let n = if e == 'u' { 4 } else { 8 };
                        let mut code = 0u32;
                        for _ in 0..n {
                            let (_, h) = chars.next().ok_or("unterminated string")?;
                            code = code * 16 + h.to_digit(16).ok_or("bad \\u escape")?;
                        }
                        out.push(char::from_u32(code).ok_or("bad \\u escape")?);
                    }
                    _ => return Err("unknown escape in string (write \\\\ for a backslash)"),
                }
            }
            c => out.push(c),
        }
    }
    Err("unterminated string")
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../config/touchos.example.toml");

    #[test]
    fn example_round_trips_byte_for_byte() {
        let doc = Document::parse(EXAMPLE).unwrap();
        assert_eq!(doc.to_text(), EXAMPLE);
    }

    #[test]
    fn example_values() {
        let raw = Document::parse(EXAMPLE).unwrap().raw();
        assert_eq!(raw.values["homeassistant.port"], Value::Int(8123));
        assert_eq!(raw.values["time.zone"], Value::Str("Europe/Amsterdam".into()));
        assert_eq!(raw.arrays["tile"].len(), 2);
        assert_eq!(
            raw.arrays["tile"][1]["entity"],
            Value::Str("sensor.outside_temperature".into())
        );
        match &raw.values["dashboard.types"] {
            Value::List(l) => assert_eq!(l.len(), 8),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn nasty_strings_survive() {
        for s in [
            "plain",
            "with \"quotes\"",
            "back\\slash",
            "ends with backslash\\",
            "has # hash",
            "tab\there",
            "ünïcødé ✓",
            "",
            "\\\"#'",
        ] {
            let mut doc = Document::parse("[wifi]\npsk = \"x\" # keep me\n").unwrap();
            doc.set("wifi", "psk", Value::Str(s.into()));
            let text = doc.to_text();
            assert!(text.contains("# keep me"), "{text}");
            let again = Document::parse(&text).unwrap();
            assert_eq!(
                again.get("wifi", "psk"),
                Some(&Value::Str(s.into())),
                "{text}"
            );
        }
    }

    #[test]
    fn literal_strings_and_escapes() {
        let doc = Document::parse("a = 'C:\\path # not a comment'\nb = \"\\u00e9\\\\\"\n").unwrap();
        assert_eq!(
            doc.get("", "a"),
            Some(&Value::Str("C:\\path # not a comment".into()))
        );
        assert_eq!(doc.get("", "b"), Some(&Value::Str("é\\".into())));
        assert!(Document::parse("a = \"bad \\q\"").is_err());
    }

    #[test]
    fn ints_bools_lists() {
        let doc =
            Document::parse("a = -1_000\nb = true\nc = [ 1, \"x\" , false ]\nd = []\n").unwrap();
        assert_eq!(doc.get("", "a"), Some(&Value::Int(-1000)));
        assert_eq!(doc.get("", "b"), Some(&Value::Bool(true)));
        assert_eq!(
            doc.get("", "c"),
            Some(&Value::List(vec![
                Value::Int(1),
                Value::Str("x".into()),
                Value::Bool(false)
            ]))
        );
        assert_eq!(doc.get("", "d"), Some(&Value::List(vec![])));
        assert!(Document::parse("a = hello").is_err());
        assert!(Document::parse("a = 1_").is_err());
    }

    #[test]
    fn errors_have_line_numbers() {
        let e = Document::parse("[a]\nx = 1\ny = \"unterminated\n").unwrap_err();
        assert_eq!(e.line, 3);
        let e = Document::parse("[a]\nx = 1\nx = 2\n").unwrap_err();
        assert_eq!((e.line, e.msg), (3, "key defined twice"));
        assert!(Document::parse("[a]\n[a]\n").is_err());
        assert!(Document::parse("x = 1 2").is_err());
        // The same key in two [[array]] items is fine.
        assert!(Document::parse("[[t]]\nk = 1\n[[t]]\nk = 2\n").is_ok());
    }

    #[test]
    fn set_existing_changes_only_the_value() {
        let mut doc = Document::parse(EXAMPLE).unwrap();
        doc.set("display", "brightness", Value::Int(55));
        let before: Vec<&str> = EXAMPLE.lines().collect();
        let text = doc.to_text();
        let after: Vec<&str> = text.lines().collect();
        assert_eq!(before.len(), after.len());
        let diff: Vec<(&&str, &&str)> = before
            .iter()
            .zip(after.iter())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(diff, vec![(&"brightness = 80", &"brightness = 55")]);
    }

    #[test]
    fn set_new_key_goes_after_example_comment() {
        let mut doc = Document::parse(EXAMPLE).unwrap();
        doc.set("display", "night", Value::Str("23:00-07:00".into()));
        let text = doc.to_text();
        assert!(
            text.contains("# night = \"23:00-07:00\"\nnight = \"23:00-07:00\"\n"),
            "{text}"
        );
        assert_eq!(
            Document::parse(&text).unwrap().raw().values["display.night"],
            Value::Str("23:00-07:00".into())
        );
    }

    #[test]
    fn set_new_key_after_last_key() {
        let mut doc = Document::parse("[a]\nx = 1\n\n[b]\ny = 2\n").unwrap();
        doc.set("a", "z", Value::Bool(true));
        assert_eq!(doc.to_text(), "[a]\nx = 1\nz = true\n\n[b]\ny = 2\n");
    }

    #[test]
    fn set_new_section_goes_before_arrays() {
        let mut doc = Document::parse(EXAMPLE).unwrap();
        doc.set("web", "port", Value::Int(8080));
        let text = doc.to_text();
        let web = text.find("[web]\nport = 8080\n").expect(&text);
        let tiles = text.find("# Dashboard tiles").unwrap();
        assert!(web < tiles, "{text}");
        assert_eq!(
            Document::parse(&text).unwrap().raw().arrays["tile"].len(),
            2
        );
    }

    #[test]
    fn set_new_section_in_empty_file() {
        let mut doc = Document::parse("").unwrap();
        doc.set("wifi", "ssid", Value::Str("x".into()));
        assert_eq!(doc.to_text(), "[wifi]\nssid = \"x\"\n");
    }

    #[test]
    fn unset_removes_line() {
        let mut doc = Document::parse("[a]\nx = 1\ny = 2\n").unwrap();
        assert!(doc.unset("a", "x"));
        assert!(!doc.unset("a", "x"));
        assert_eq!(doc.to_text(), "[a]\ny = 2\n");
    }

    fn tile(e: &str, n: &str) -> Vec<(String, Value)> {
        vec![
            ("entity".into(), Value::Str(e.into())),
            ("name".into(), Value::Str(n.into())),
        ]
    }

    #[test]
    fn set_array_replaces_blocks_in_place() {
        let text = "[a]\nx = 1\n\n# Tiles:\n[[tile]]\nentity = \"a.a\"\n\n# second\n[[tile]]\nentity = \"b.b\"\n\n# after\n[z]\nq = 1\n";
        let mut doc = Document::parse(text).unwrap();
        doc.set_array("tile", &[tile("c.c", "C")]);
        assert_eq!(
            doc.to_text(),
            "[a]\nx = 1\n\n# Tiles:\n[[tile]]\nentity = \"c.c\"\nname = \"C\"\n\n# after\n[z]\nq = 1\n"
        );
        doc.set_array("tile", &[tile("d.d", "D"), tile("e.e", "E")]);
        let raw = Document::parse(&doc.to_text()).unwrap().raw();
        assert_eq!(raw.arrays["tile"].len(), 2);
        assert_eq!(raw.values["z.q"], Value::Int(1));
        doc.set_array("tile", &[]);
        assert_eq!(
            doc.to_text(),
            "[a]\nx = 1\n\n# Tiles:\n\n# after\n[z]\nq = 1\n"
        );
    }

    #[test]
    fn set_array_appends_when_missing() {
        let mut doc = Document::parse("[a]\nx = 1\n").unwrap();
        doc.set_array("tile", &[tile("a.a", "A")]);
        assert_eq!(
            doc.to_text(),
            "[a]\nx = 1\n\n[[tile]]\nentity = \"a.a\"\nname = \"A\"\n"
        );
    }

    #[test]
    fn crlf_is_accepted() {
        let doc = Document::parse("[a]\r\nx = 1\r\n").unwrap();
        assert_eq!(doc.get("a", "x"), Some(&Value::Int(1)));
    }
}
