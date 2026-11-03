//! Apple Mail `.emlx`: `<byte count>\n<RFC 822 bytes><XML plist with flags>`.

use crate::hdr::{F_DELETED, F_DRAFT, F_FLAGGED, F_READ};

/// `(start, end, declared count)` of the RFC 822 payload within the file bytes. `end` is
/// clamped to the available data when the declared count overruns (truncated file).
pub(crate) fn split(data: &[u8]) -> Option<(usize, usize, u64)> {
    let nl = memchr::memchr(b'\n', &data[..data.len().min(64)])?;
    let count: u64 = std::str::from_utf8(&data[..nl]).ok()?.trim().parse().ok()?;
    let start = nl + 1;
    let end = start.saturating_add(usize::try_from(count).unwrap_or(usize::MAX)).min(data.len());
    Some((start, end, count))
}

fn after_key<'a>(plist: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let pat = format!("<key>{key}</key>");
    let p = memchr::memmem::find(plist, pat.as_bytes())?;
    Some(&plist[p + pat.len()..])
}

/// Integer value following `<key>key</key>` in a plist.
pub(crate) fn plist_int(plist: &[u8], key: &str) -> Option<i64> {
    let rest = after_key(plist, key)?;
    let open = memchr::memmem::find(rest, b"<integer>")?;
    // Only accept the immediately following value element.
    if rest[..open].iter().any(|c| !c.is_ascii_whitespace()) {
        return None;
    }
    let v = &rest[open + 9..];
    let close = memchr::memchr(b'<', v)?;
    std::str::from_utf8(&v[..close]).ok()?.trim().parse().ok()
}

/// Real/integer date (seconds since 1970) for `key`, as UTC ms; out-of-range values are `None`.
pub(crate) fn plist_date(plist: &[u8], key: &str) -> Option<i64> {
    if let Some(v) = plist_int(plist, key) {
        return v.checked_mul(1000);
    }
    let rest = after_key(plist, key)?;
    let open = memchr::memmem::find(rest, b"<real>")?;
    if rest[..open].iter().any(|c| !c.is_ascii_whitespace()) {
        return None;
    }
    let v = &rest[open + 6..];
    let close = memchr::memchr(b'<', v)?;
    let ms = std::str::from_utf8(&v[..close]).ok()?.trim().parse::<f64>().ok()? * 1000.0;
    // `as` saturates; reject NaN/inf/out-of-range instead of clamping to a bogus date.
    (ms.is_finite() && ms.abs() < i64::MAX as f64).then_some(ms as i64)
}

/// Apple Mail flag bits → row flags (bit 0 read, 1 deleted, 4 flagged, 6 draft).
pub(crate) fn flag_bits(f: i64) -> u16 {
    let mut out = 0;
    if f & 1 != 0 {
        out |= F_READ;
    }
    if f & 2 != 0 {
        out |= F_DELETED;
    }
    if f & (1 << 4) != 0 {
        out |= F_FLAGGED;
    }
    if f & (1 << 6) != 0 {
        out |= F_DRAFT;
    }
    out
}

pub(crate) const STATUS_MASK: u16 = F_READ | F_DELETED | F_FLAGGED | F_DRAFT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_plist() {
        let mut d = b"  12  \nSubject: x\n\n".to_vec();
        let n = d.len();
        d.extend_from_slice(
            b"<?xml version=\"1.0\"?><plist><dict><key>date-received</key><integer>100</integer>\
              <key>flags</key>\n\t<integer>17</integer></dict></plist>",
        );
        let (s, e, c) = split(&d).unwrap();
        assert_eq!((s, e, c), (7, n, 12));
        assert_eq!(&d[s..e], b"Subject: x\n\n");
        let plist = &d[e..];
        assert_eq!(plist_int(plist, "flags"), Some(17));
        assert_eq!(flag_bits(17), F_READ | F_FLAGGED);
        assert_eq!(plist_date(plist, "date-received"), Some(100_000));
        assert_eq!(plist_int(plist, "missing"), None);
        // Count larger than the data is clamped, garbage is rejected.
        assert_eq!(split(b"999\nabc"), Some((4, 7, 999)));
        assert_eq!(split(b"abc\nxyz"), None);
        assert_eq!(split(b""), None);
        // Dates that would overflow ms are rejected rather than wrapped/saturated.
        let huge = b"<key>d</key><integer>9223372036854775807</integer><key>r</key><real>nan</real>";
        assert_eq!(plist_date(huge, "d"), None);
        assert_eq!(plist_date(huge, "r"), None);
        assert_eq!(plist_date(b"<key>r</key> <real>1.5</real>", "r"), Some(1500));
    }
}
