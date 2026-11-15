//! Calendar time: UNIX seconds -> local date and time (with EU summer time).
//!
//! No dependencies: the kernel (the firmware clock at boot), the config
//! and update servers (file stamps), the dashboard's clock and the Toon's
//! RTC all use it.

#![cfg_attr(not(test), no_std)]

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

/// UTC date and time -> UNIX seconds.
pub fn unix_from_civil(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> i64 {
    days_from_civil(year, month, day) * 86_400 + (hour * 3600 + minute * 60 + second) as i64
}

/// Unix time of the last Sunday of a month at 01:00 UTC (EU DST switch).
fn last_sunday_0100(year: i32, month: u32) -> i64 {
    let next = if month == 12 {
        days_from_civil(year + 1, 1, 1)
    } else {
        days_from_civil(year, month + 1, 1)
    };
    let last = next - 1;
    let wd = (last + 4).rem_euclid(7); // 1970-01-01 was a Thursday
    (last - wd) * 86_400 + 3600
}

/// Local time: UTC + `offset_min`, plus one hour in EU summer time if `eu_dst`.
pub fn local(unix: u64, offset_min: i64, eu_dst: bool) -> Civil {
    let t = unix as i64;
    let (y, _, _) = civil_from_days(t.div_euclid(86_400));
    let dst = eu_dst && t >= last_sunday_0100(y, 3) && t < last_sunday_0100(y, 10);
    let l = t + offset_min * 60 + if dst { 3600 } else { 0 };
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
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eu_summer_time() {
        // 2026-07-01 12:00 UTC -> 14:00 in Amsterdam (UTC+1, DST).
        let t = local(1_782_907_200, 60, true);
        assert_eq!((t.year, t.month, t.day, t.hour), (2026, 7, 1, 14));
        // 2026-01-15 12:00 UTC -> 13:00.
        let t = local(1_768_478_400, 60, true);
        assert_eq!((t.month, t.day, t.hour), (1, 15, 13));
        assert_eq!(local(0, 0, false).weekday, 4); // Thursday
    }

    #[test]
    fn civil_round_trip() {
        assert_eq!(unix_from_civil(2026, 7, 1, 12, 0, 0), 1_782_907_200);
        assert_eq!(unix_from_civil(1970, 1, 1, 0, 0, 0), 0);
        let t = local(1_794_623_601, 0, false);
        let u = unix_from_civil(t.year, t.month, t.day, t.hour, t.minute, t.second);
        assert_eq!(u, 1_794_623_601);
    }
}
