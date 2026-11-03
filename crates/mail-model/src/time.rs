//! Proleptic Gregorian calendar ↔ days since the Unix epoch (Howard Hinnant's algorithms),
//! valid for every `i64` day count a millisecond timestamp can express.

/// Milliseconds in one (UTC, leap-second-free) day.
pub const MS_PER_DAY: i64 = 86_400_000;

/// Days since 1970-01-01 of the date `y`-`m`-`d` (`m` 1–12, `d` 1–31; out-of-range days
/// roll over arithmetically, e.g. Feb 30 = Mar 1 or 2).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `(year, month 1–12, day 1–31)` of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(-719_468), (0, 3, 1));
    }

    #[test]
    fn round_trips_across_eras() {
        for days in (-1_000_000..1_000_000).step_by(97) {
            let (y, m, d) = civil_from_days(days);
            assert!((1..=12).contains(&m) && (1..=31).contains(&d), "{days}");
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d}");
        }
    }
}
