//! UTC timestamp formatting (ms since epoch) without a calendar dependency.

use mail_model::time::{civil_from_days, MS_PER_DAY};

/// Broken-down UTC time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Convert UTC milliseconds since the Unix epoch to a civil date (proleptic Gregorian).
pub fn civil(ms: i64) -> Civil {
    let days = ms.div_euclid(MS_PER_DAY);
    let sod = (ms.rem_euclid(MS_PER_DAY) / 1000) as u32;
    let (year, month, day) = civil_from_days(days);
    Civil {
        year,
        month,
        day,
        hour: sod / 3600,
        minute: sod / 60 % 60,
        second: sod % 60,
        weekday: (days + 4).rem_euclid(7) as u32,
    }
}

/// `2024-03-05 14:07` (UTC).
pub fn format_short(ms: i64) -> String {
    let c = civil(ms);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", c.year, c.month, c.day, c.hour, c.minute)
}

/// `2024-03-05 14:07:09` (UTC).
pub fn format_long(ms: i64) -> String {
    let c = civil(ms);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

/// `2024-03-05T14:07:09Z`.
pub fn format_rfc3339(ms: i64) -> String {
    let c = civil(ms);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

/// `Tue, 5 Mar 2024 14:07:09 +0000`.
pub fn format_rfc2822(ms: i64) -> String {
    let c = civil(ms);
    format!(
        "{}, {} {} {:04} {:02}:{:02}:{:02} +0000",
        DAYS[c.weekday as usize],
        c.day,
        MONTHS[(c.month - 1) as usize],
        c.year,
        c.hour,
        c.minute,
        c.second
    )
}

/// C `asctime` layout used by mbox `From ` lines: `Tue Mar  5 14:07:09 2024`.
pub fn format_asctime(ms: i64) -> String {
    let c = civil(ms);
    format!(
        "{} {} {:2} {:02}:{:02}:{:02} {:04}",
        DAYS[c.weekday as usize],
        MONTHS[(c.month - 1) as usize],
        c.day,
        c.hour,
        c.minute,
        c.second,
        c.year
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_dates() {
        assert_eq!(format_asctime(0), "Thu Jan  1 00:00:00 1970");
        // 2024-02-29T23:59:59Z (leap day)
        assert_eq!(format_rfc3339(1_709_251_199_000), "2024-02-29T23:59:59Z");
        assert_eq!(format_rfc2822(1_709_251_199_000), "Thu, 29 Feb 2024 23:59:59 +0000");
        // Before the epoch: 1969-12-31T23:59:59Z
        assert_eq!(format_rfc3339(-1000), "1969-12-31T23:59:59Z");
        assert_eq!(civil(-1000).weekday, 3);
        // 2000-03-01 (century leap year boundary)
        assert_eq!(format_short(951_868_800_000), "2000-03-01 00:00");
    }
}
