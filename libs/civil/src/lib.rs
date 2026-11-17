//! Calendar time: UNIX seconds -> local date and time in a time zone, and
//! back (UTC only).
//!
//! A zone is a POSIX TZ rule ([`Tz::parse`], e.g. `CET-1CEST,M3.5.0,M10.5.0/3`):
//! the zone's current offsets and when summer time starts and ends. The
//! rules of every IANA zone are in [`ZONES`], made from tzdata by
//! `cargo xtask tzdata`; [`zone`] looks one up by name.
//!
//! No dependencies: the config and update servers (file stamps), the
//! dashboard's clock, the Toon's RTC and the certificate checks all use it.

#![cfg_attr(not(test), no_std)]

mod zones;

pub use zones::{TZDATA_VERSION, ZONES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday
    pub weekday: u32,
}

/// Days since 1970-01-01 -> (year, month, day). (Howard Hinnant's algorithm.)
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// UTC date and time -> UNIX seconds.
pub fn unix_from_civil(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    days_from_civil(year, month, day) * 86_400 + (hour * 3600 + minute * 60 + second) as i64
}

/// Seconds since the epoch, already shifted to local time -> the date and time.
fn civil_at(l: i64) -> Civil {
    let days = l.div_euclid(86_400);
    let secs = l.rem_euclid(86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: secs / 3600,
        minute: secs / 60 % 60,
        second: secs % 60,
        weekday: (days + 4).rem_euclid(7) as u32, // 1970-01-01 was a Thursday
    }
}

/// A time zone: a standard offset and maybe a summer time with its rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tz {
    /// Seconds east of UTC (POSIX writes them the other way round).
    pub std: i32,
    pub dst: Option<Dst>,
}

/// Summer time (or, as in Ireland, winter time: the offset may be smaller).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dst {
    /// Seconds east of UTC.
    pub off: i32,
    /// When it starts, in standard time.
    pub start: Rule,
    /// When it ends, in summer time.
    pub end: Rule,
}

/// A day of the year and a local time on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub date: Date,
    /// Seconds after local midnight; -167..=167 hours (RFC 8536).
    pub time: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Date {
    /// `Jn`: day 1..=365, February 29 not counted.
    J(u16),
    /// `n`: day 0..=365, February 29 counted.
    N(u16),
    /// `Mm.w.d`: day `d` (0 = Sunday) of week `w` (1..=5, 5 = the last) of month `m`.
    M { m: u8, w: u8, d: u8 },
}

impl Date {
    /// Days since 1970-01-01 of this date in `year`.
    fn day(self, year: i32) -> i64 {
        let jan1 = days_from_civil(year, 1, 1);
        match self {
            Date::J(n) => jan1 + n as i64 - 1 + (is_leap(year) && n >= 60) as i64,
            Date::N(n) => jan1 + n as i64,
            Date::M { m, w, d } => {
                let m = m as u32;
                let first = days_from_civil(year, m, 1);
                let next = if m == 12 { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, m + 1, 1) };
                let wd = (first + 4).rem_euclid(7);
                let mut day = first + (d as i64 - wd).rem_euclid(7) + (w as i64 - 1) * 7;
                while day >= next {
                    day -= 7;
                }
                day
            }
        }
    }
}

impl Tz {
    pub const UTC: Tz = Tz { std: 0, dst: None };

    /// A POSIX TZ rule: `std offset [dst [offset] ,start[/time],end[/time]]`,
    /// e.g. `CET-1CEST,M3.5.0,M10.5.0/3` or `<+0545>-5:45`. Names may be
    /// quoted (`<-03>`); times may run from -167 to 167 hours (RFC 8536).
    /// A summer time without rules is refused (POSIX leaves its rules to
    /// the system).
    pub fn parse(s: &str) -> Option<Tz> {
        let mut p = Parser { s: s.as_bytes(), i: 0 };
        p.name()?;
        let std = -p.hms(24)?;
        if p.done() {
            return Some(Tz { std, dst: None });
        }
        p.name()?;
        let off = if p.peek() == Some(b',') { std + 3600 } else { -p.hms(24)? };
        p.eat(b',')?;
        let start = p.rule()?;
        p.eat(b',')?;
        let end = p.rule()?;
        p.done().then_some(Tz { std, dst: Some(Dst { off, start, end }) })
    }

    /// Seconds east of UTC at UNIX time `unix`.
    pub fn offset(&self, unix: i64) -> i32 {
        let Some(dst) = self.dst else {
            return self.std;
        };
        let (year, _, _) = civil_from_days((unix + self.std as i64).div_euclid(86_400));
        // The last switch at or before `unix`, among the neighbouring years'
        // (start wins a tie: all-year summer time ends where it starts again).
        let mut last: Option<(i64, bool)> = None;
        for y in year - 1..=year + 1 {
            let start = dst.start.date.day(y) * 86_400 + dst.start.time as i64 - self.std as i64;
            let end = dst.end.date.day(y) * 86_400 + dst.end.time as i64 - dst.off as i64;
            for sw in [(start, true), (end, false)] {
                if sw.0 <= unix && last.is_none_or(|l| sw > l) {
                    last = Some(sw);
                }
            }
        }
        match last {
            Some((_, true)) => dst.off,
            _ => self.std,
        }
    }

    /// The local date and time at UNIX time `unix`.
    pub fn local(&self, unix: u64) -> Civil {
        let t = unix as i64;
        civil_at(t + self.offset(t) as i64)
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn done(&self) -> bool {
        self.i == self.s.len()
    }
    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek() == Some(c)).then(|| self.i += 1)
    }
    /// A zone abbreviation: 3 or more letters, or `<...>` with letters,
    /// digits, `+` and `-`.
    fn name(&mut self) -> Option<()> {
        let quoted = self.eat(b'<').is_some();
        let from = self.i;
        while let Some(c) = self.peek() {
            let ok = c.is_ascii_alphabetic() || (quoted && (c.is_ascii_digit() || c == b'+' || c == b'-'));
            if !ok {
                break;
            }
            self.i += 1;
        }
        let long_enough = self.i - from >= 3;
        if quoted {
            self.eat(b'>')?;
        }
        long_enough.then_some(())
    }
    fn number(&mut self, max: u32) -> Option<u32> {
        let from = self.i;
        let mut n: u32 = 0;
        while let Some(c @ b'0'..=b'9') = self.peek() {
            n = n.checked_mul(10)?.checked_add((c - b'0') as u32)?;
            self.i += 1;
        }
        (self.i > from && n <= max).then_some(n)
    }
    /// `[+-]hh[:mm[:ss]]` in seconds, hours up to `max_h`.
    fn hms(&mut self, max_h: u32) -> Option<i32> {
        let neg = match self.peek() {
            Some(b'-') => {
                self.i += 1;
                true
            }
            Some(b'+') => {
                self.i += 1;
                false
            }
            _ => false,
        };
        let mut secs = self.number(max_h)? * 3600;
        if self.eat(b':').is_some() {
            secs += self.number(59)? * 60;
            if self.eat(b':').is_some() {
                secs += self.number(59)?;
            }
        }
        Some(if neg { -(secs as i32) } else { secs as i32 })
    }
    fn rule(&mut self) -> Option<Rule> {
        let date = match self.peek()? {
            b'J' => {
                self.i += 1;
                let n = self.number(365)?;
                (n >= 1).then_some(Date::J(n as u16))?
            }
            b'M' => {
                self.i += 1;
                let m = self.number(12)?;
                self.eat(b'.')?;
                let w = self.number(5)?;
                self.eat(b'.')?;
                let d = self.number(6)?;
                (m >= 1 && w >= 1).then_some(Date::M { m: m as u8, w: w as u8, d: d as u8 })?
            }
            _ => Date::N(self.number(365)? as u16),
        };
        let time = if self.eat(b'/').is_some() { self.hms(167)? } else { 7200 };
        Some(Rule { date, time })
    }
}

/// A zone's name as tzdata spells it and its rule: an exact match, else
/// one that differs only in case.
pub fn find(name: &str) -> Option<(&'static str, &'static str)> {
    match ZONES.binary_search_by(|z| z.0.cmp(name)) {
        Ok(i) => Some((ZONES[i].0, ZONES[i].1)),
        Err(_) => ZONES.iter().find(|z| z.0.eq_ignore_ascii_case(name)).map(|z| (z.0, z.1)),
    }
}

/// The rule of an IANA zone (`Europe/Amsterdam`), by [`find`].
pub fn zone(name: &str) -> Option<Tz> {
    find(name).and_then(|(_, rule)| Tz::parse(rule))
}

/// The zones worth offering in a list: tzdata's zones, not its links or
/// the `Etc/` ones, plus `UTC`.
pub fn listed() -> impl Iterator<Item = &'static str> {
    ZONES.iter().filter(|z| z.2).map(|z| z.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (local y, m, d, h, min) of `unix` in `tz`.
    fn at(tz: &Tz, unix: i64) -> (i32, u32, u32, u32, u32) {
        let t = tz.local(unix as u64);
        (t.year, t.month, t.day, t.hour, t.minute)
    }

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        unix_from_civil(y, m, d, h, min, 0)
    }

    #[test]
    fn eu_summer_time() {
        let ams = Tz::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        assert_eq!(at(&ams, utc(2026, 7, 1, 12, 0)), (2026, 7, 1, 14, 0));
        assert_eq!(at(&ams, utc(2026, 1, 15, 12, 0)), (2026, 1, 15, 13, 0));
        // 2026-03-29 and 2026-10-25, at 01:00 UTC.
        assert_eq!(ams.offset(utc(2026, 3, 29, 0, 59) + 59), 3600);
        assert_eq!(ams.offset(utc(2026, 3, 29, 1, 0)), 7200);
        assert_eq!(ams.offset(utc(2026, 10, 25, 0, 59) + 59), 7200);
        assert_eq!(ams.offset(utc(2026, 10, 25, 1, 0)), 3600);
        assert_eq!(Tz::UTC.local(0).weekday, 4); // Thursday
    }

    #[test]
    fn new_york() {
        let ny = Tz::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        // 2026-03-08 02:00 EST = 07:00 UTC; 2026-11-01 02:00 EDT = 06:00 UTC.
        assert_eq!(ny.offset(utc(2026, 3, 8, 7, 0) - 1), -5 * 3600);
        assert_eq!(ny.offset(utc(2026, 3, 8, 7, 0)), -4 * 3600);
        assert_eq!(ny.offset(utc(2026, 11, 1, 6, 0) - 1), -4 * 3600);
        assert_eq!(ny.offset(utc(2026, 11, 1, 6, 0)), -5 * 3600);
        assert_eq!(at(&ny, utc(2027, 1, 1, 3, 0)), (2026, 12, 31, 22, 0));
    }

    #[test]
    fn southern_hemisphere() {
        let syd = Tz::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        // Summer over New Year: +11.
        assert_eq!(at(&syd, utc(2026, 12, 31, 13, 0)), (2027, 1, 1, 0, 0));
        assert_eq!(syd.offset(utc(2026, 7, 1, 0, 0)), 10 * 3600);
        // 2026-04-05 03:00 AEDT = 2026-04-04 16:00 UTC; 2026-10-04 02:00 AEST = 10-03 16:00 UTC.
        assert_eq!(syd.offset(utc(2026, 4, 4, 16, 0) - 1), 11 * 3600);
        assert_eq!(syd.offset(utc(2026, 4, 4, 16, 0)), 10 * 3600);
        assert_eq!(syd.offset(utc(2026, 10, 3, 16, 0) - 1), 10 * 3600);
        assert_eq!(syd.offset(utc(2026, 10, 3, 16, 0)), 11 * 3600);
        // Lord Howe: half an hour of summer time.
        let lhi = Tz::parse("<+1030>-10:30<+11>-11,M10.1.0,M4.1.0").unwrap();
        assert_eq!(lhi.offset(utc(2026, 7, 1, 0, 0)), 37_800);
        assert_eq!(lhi.offset(utc(2026, 1, 1, 0, 0)), 39_600);
    }

    #[test]
    fn negative_summer_time() {
        // Ireland: IST is the standard time, GMT the "summer" one in winter.
        let dub = Tz::parse("IST-1GMT0,M10.5.0,M3.5.0/1").unwrap();
        assert_eq!(dub.offset(utc(2026, 7, 1, 12, 0)), 3600);
        assert_eq!(dub.offset(utc(2026, 1, 15, 12, 0)), 0);
        assert_eq!(dub.offset(utc(2026, 3, 29, 1, 0) - 1), 0);
        assert_eq!(dub.offset(utc(2026, 3, 29, 1, 0)), 3600);
        assert_eq!(dub.offset(utc(2026, 10, 25, 1, 0) - 1), 3600);
        assert_eq!(dub.offset(utc(2026, 10, 25, 1, 0)), 0);
    }

    #[test]
    fn odd_offsets_and_times() {
        let ktm = Tz::parse("<+0545>-5:45").unwrap();
        assert_eq!(at(&ktm, utc(2026, 1, 1, 0, 0)), (2026, 1, 1, 5, 45));
        let fixed = Tz::parse("<-03>3").unwrap();
        assert_eq!(fixed, Tz { std: -3 * 3600, dst: None });
        // Greenland: switches at -1:00 and 0:00 local, i.e. 01:00 UTC like the EU.
        let nuuk = Tz::parse("<-02>2<-01>,M3.5.0/-1,M10.5.0/0").unwrap();
        assert_eq!(nuuk.offset(utc(2026, 3, 29, 1, 0) - 1), -2 * 3600);
        assert_eq!(nuuk.offset(utc(2026, 3, 29, 1, 0)), -3600);
        assert_eq!(nuuk.offset(utc(2026, 10, 25, 1, 0) - 1), -3600);
        assert_eq!(nuuk.offset(utc(2026, 10, 25, 1, 0)), -2 * 3600);
        // Summer time all year: it ends (Dec 31, 25:00) where it starts again.
        let always = Tz::parse("EST5EDT,0/0,J365/25").unwrap();
        for t in [utc(2026, 1, 1, 5, 0), utc(2026, 7, 1, 0, 0), utc(2026, 12, 31, 23, 59), utc(2027, 1, 1, 5, 0)] {
            assert_eq!(always.offset(t), -4 * 3600, "{t}");
        }
        // Julian days skip February 29: J60 is March 1, also in a leap year.
        let j = Tz::parse("AAA0BBB,J60/0,J300/0").unwrap();
        assert_eq!(j.offset(utc(2028, 3, 1, 0, 0) - 1), 0);
        assert_eq!(j.offset(utc(2028, 3, 1, 0, 0)), 3600);
    }

    #[test]
    fn bad_rules_are_refused() {
        for s in [
            "",
            "UTC",
            "Europe/Amsterdam",
            "CET",
            "CE-1",
            "CET-1CEST",
            "CET-1CEST,M3.5.0",
            "CET-1CEST,M3.5.0,M13.5.0",
            "CET-1CEST,M3.6.0,M10.5.0",
            "CET-1CEST,M3.5.0,M10.5.0/168",
            "CET-25",
            "<+03-3",
            "<+3>-3",
            "CET-1 ",
            "CET-1:60",
        ] {
            assert_eq!(Tz::parse(s), None, "{s:?}");
        }
        assert_eq!(Tz::parse("UTC0"), Some(Tz::UTC));
        assert_eq!(Tz::parse("<UTC>+0"), Some(Tz::UTC));
    }

    #[test]
    fn every_zone_parses_and_the_table_is_sorted() {
        for z in ZONES {
            assert!(Tz::parse(z.1).is_some(), "{} {}", z.0, z.1);
        }
        assert!(ZONES.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn zones_by_name() {
        if ZONES.is_empty() {
            return; // not generated yet
        }
        assert_eq!(zone("Europe/Amsterdam"), Tz::parse("CET-1CEST,M3.5.0,M10.5.0/3"));
        assert_eq!(zone("UTC"), Some(Tz::UTC));
        assert_eq!(find("europe/amsterdam").map(|z| z.0), Some("Europe/Amsterdam"));
        assert_eq!(zone("Asia/Calcutta"), zone("Asia/Kolkata")); // a link
        assert_eq!(zone("Mars/Olympus"), None);
        assert!(listed().any(|z| z == "UTC"));
        assert!(listed().any(|z| z == "America/New_York"));
        assert!(!listed().any(|z| z == "Asia/Calcutta" || z.starts_with("Etc/")));
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(unix_from_civil(2026, 7, 1, 12, 0, 0), 1_782_907_200);
        assert_eq!(unix_from_civil(1970, 1, 1, 0, 0, 0), 0);
        let t = Tz::UTC.local(1_794_623_601);
        let u = unix_from_civil(t.year, t.month, t.day, t.hour, t.minute, t.second);
        assert_eq!(u, 1_794_623_601);
    }
}
