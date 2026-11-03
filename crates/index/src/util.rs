//! Small helpers: query dates and sizes, subject normalisation, text trimming.

use mail_model::time::{civil_from_days, days_from_civil, MS_PER_DAY};

/// UTC calendar year of a timestamp in ms since the epoch.
pub(crate) fn year_of_ms(ms: i64) -> i64 {
    civil_from_days(ms.div_euclid(MS_PER_DAY)).0
}

/// Parses `YYYY-MM-DD`, `YYYY/MM/DD`, `YYYY-MM` or `YYYY` into UTC midnight (ms) of the first day.
pub(crate) fn parse_date_ms(s: &str) -> Option<i64> {
    let mut parts = s.split(['-', '/', '.']);
    let y: i64 = parse_digits(parts.next()?, 4, 4)?;
    let m: u32 = match parts.next() {
        Some(p) => parse_digits(p, 1, 2)?,
        None => 1,
    };
    let d: u32 = match parts.next() {
        Some(p) => parse_digits(p, 1, 2)?,
        None => 1,
    };
    if parts.next().is_some() || !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
        return None;
    }
    Some(days_from_civil(y, m, d) * MS_PER_DAY)
}

fn parse_digits<T: std::str::FromStr>(s: &str, min: usize, max: usize) -> Option<T> {
    if s.len() < min || s.len() > max || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Parses `5M`, `500k`, `2MB`, `1G`, `1234` into bytes.
pub(crate) fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mul: f64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    Some((n * mul) as u64)
}

/// Strips any number of leading reply/forward prefixes (`Re:`, `Fwd:`, `AW:`, `RE[2]:` ...),
/// lowercases and trims. Used for subject sorting.
pub(crate) fn normalize_subject(subject: &str) -> String {
    let mut s = subject.trim();
    while let Some(colon) = s.find(':') {
        let prefix = s[..colon].trim_end();
        let base = prefix.split('[').next().unwrap_or(prefix);
        let is_reply = matches!(
            base.to_ascii_lowercase().as_str(),
            "re" | "fw" | "fwd" | "aw" | "wg" | "sv" | "vs" | "antw" | "tr" | "rif" | "r"
        );
        if !is_reply {
            break;
        }
        s = s[colon + 1..].trim_start();
    }
    s.to_lowercase()
}

/// Appends `src` to `out` with every whitespace run collapsed to a single space,
/// stopping once `out` reaches `max_bytes` (on a char boundary).
pub(crate) fn push_collapsed(out: &mut String, src: &str, max_bytes: usize) {
    let mut pending_space = !out.is_empty() && !out.ends_with(' ');
    for c in src.chars() {
        if out.len() >= max_bytes {
            return;
        }
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
}

/// Truncates `s` to at most `max` bytes on a char boundary.
pub(crate) fn truncate_at_char(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_of_timestamps() {
        for (y, m, d) in [(1969, 12, 31), (2024, 2, 29), (2023, 12, 31), (1900, 1, 1), (2100, 6, 15)] {
            let ms = days_from_civil(y, m, d) * MS_PER_DAY + 3_600_000;
            assert_eq!(year_of_ms(ms), y, "{y}-{m}-{d}");
        }
        assert_eq!(year_of_ms(-1), 1969);
    }

    #[test]
    fn dates_and_sizes() {
        assert_eq!(parse_date_ms("1970-01-02"), Some(86_400_000));
        assert_eq!(parse_date_ms("2024/02/29"), Some(days_from_civil(2024, 2, 29) * 86_400_000));
        assert_eq!(parse_date_ms("2023-02-29"), None);
        assert_eq!(parse_date_ms("2023-13-01"), None);
        assert_eq!(parse_date_ms("2023"), Some(days_from_civil(2023, 1, 1) * 86_400_000));
        assert_eq!(parse_date_ms("20x3-01-01"), None);
        assert_eq!(parse_size("5M"), Some(5 * 1024 * 1024));
        assert_eq!(parse_size("1.5kb"), Some(1536));
        assert_eq!(parse_size("42"), Some(42));
        assert_eq!(parse_size("3 parsecs"), None);
    }

    #[test]
    fn subjects() {
        assert_eq!(normalize_subject("RE: Fwd: re[2]: Budget  "), "budget");
        assert_eq!(normalize_subject("Agenda: Monday"), "agenda: monday");
    }

    #[test]
    fn collapse() {
        let mut s = String::new();
        push_collapsed(&mut s, "  a \n\t b  ", 100);
        push_collapsed(&mut s, "c", 100);
        assert_eq!(s, "a b c");
        let mut t = String::new();
        push_collapsed(&mut t, "ééééé", 3);
        assert_eq!(t, "éé");
    }
}
