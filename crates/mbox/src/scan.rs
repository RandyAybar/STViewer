//! mbox boundary detection: parallel `\nFrom ` search, `From_` line validation,
//! Content-Length (mboxcl/cl2) handling, and `>From ` unescaping.

use std::borrow::Cow;

use mail_model::time::{days_from_civil, MS_PER_DAY};
use memchr::memmem;
use rayon::prelude::*;

use crate::hdr::{self, HeaderInfo, F_NO_UNESCAPE};
use crate::util::month_index;

const CHUNK: usize = 8 << 20;
const MAX_FROM_LINE: usize = 1024;
/// Header blocks longer than this are cut (pathological input).
const MAX_HEADER: usize = 1 << 20;

/// Skips a UTF-8 BOM and leading blank lines.
pub(crate) fn skip_preamble(data: &[u8]) -> usize {
    let mut p = if data.starts_with(b"\xEF\xBB\xBF") { 3 } else { 0 };
    while let Some(&c) = data.get(p) {
        if matches!(c, b'\n' | b'\r' | b' ' | b'\t') {
            p += 1;
        } else {
            break;
        }
    }
    p
}

fn line_at(data: &[u8], p: usize) -> &[u8] {
    let rest = &data[p..data.len().min(p + MAX_FROM_LINE)];
    match memchr::memchr(b'\n', rest) {
        Some(n) => &rest[..n],
        None => rest,
    }
}

/// A plausible mbox separator: `From <sender> <date with hh:mm>`.
pub(crate) fn is_from_line(data: &[u8], p: usize) -> bool {
    let line = line_at(data, p);
    if !line.starts_with(b"From ") || line.len() >= MAX_FROM_LINE {
        return false;
    }
    let rest = &line[5..];
    // Sender token must exist and must not look like a header ("From: ...") or prose start.
    let rest = trim_start(rest);
    if rest.is_empty() {
        return false;
    }
    line.windows(5).any(|w| {
        w[0].is_ascii_digit() && w[1].is_ascii_digit() && w[2] == b':' && w[3].is_ascii_digit() && w[4].is_ascii_digit()
    })
}

fn trim_start(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|c| **c == b' ' || **c == b'\t').count();
    &b[n..]
}

/// True if `data` looks like an mbox file (used by `sniff`).
pub(crate) fn looks_like_mbox(data: &[u8]) -> bool {
    let p = skip_preamble(data);
    p < data.len() && is_from_line(data, p)
}

/// Parses the asctime date of a `From_` line (`From x Sat Jan  3 01:05:34 1996 [+0000]`) to UTC ms.
pub(crate) fn from_line_date(line: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(line).ok()?;
    let toks: Vec<&str> = s.split_ascii_whitespace().skip(2).collect();
    let mi = toks.iter().position(|t| t.len() >= 3 && month_index(t).is_some())?;
    let month = month_index(toks[mi])?;
    let day: u32 = toks.get(mi + 1)?.parse().ok()?;
    let mut time = None;
    let mut year = None;
    let mut tz = 0i64;
    for t in &toks[mi + 2..] {
        if t.contains(':') && time.is_none() {
            let mut it = t.split(':').map(|x| x.parse::<u32>().ok());
            let (h, m) = (it.next()??, it.next()??);
            let sec = it.next().flatten().unwrap_or(0);
            if h > 23 || m > 59 || sec > 60 {
                return None;
            }
            time = Some(i64::from(h * 3600 + m * 60 + sec));
        } else if let Some(off) = zone_offset(t) {
            tz = off;
        } else if t.len() == 4 && year.is_none() {
            year = t.parse::<i64>().ok();
        }
    }
    let (year, time) = (year?, time?);
    if !(1..=31).contains(&day) || !(1900..=3000).contains(&year) {
        return None;
    }
    Some(days_from_civil(year, month, day) * MS_PER_DAY + (time - tz) * 1000)
}

/// `+hhmm` / `-hhmm` → offset east of UTC in seconds.
fn zone_offset(t: &str) -> Option<i64> {
    let v: i64 =
        t.strip_prefix(['+', '-']).filter(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit()))?.parse().ok()?;
    let off = (v / 100) * 3600 + (v % 100) * 60;
    Some(if t.starts_with('-') { -off } else { off })
}

/// All validated `From_` line starts at or after `start`, in order.
pub(crate) fn find_from_lines(data: &[u8], start: usize) -> Vec<usize> {
    let len = data.len();
    if start >= len {
        return Vec::new();
    }
    let finder = memmem::Finder::new(b"\nFrom ");
    let n_chunks = (len - start).div_ceil(CHUNK);
    let mut parts: Vec<Vec<usize>> = (0..n_chunks)
        .into_par_iter()
        .map(|i| {
            let s = start + i * CHUNK;
            let e = (s + CHUNK).min(len);
            // A match starting before `e` may end up to 5 bytes after it (seam fix-up).
            let search_end = (e + 5).min(len);
            finder
                .find_iter(&data[s..search_end])
                .map(|p| s + p)
                .filter(|&p| p < e)
                .map(|p| p + 1)
                .filter(|&p| is_from_line(data, p))
                .collect()
        })
        .collect();
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum::<usize>() + 1);
    if is_from_line(data, start) {
        out.push(start);
    }
    for p in parts.iter_mut() {
        out.append(p);
    }
    out
}

/// Header facts of one message, already encoded for the row arena (built in parallel).
pub(crate) struct Encoded {
    pub date: Option<i64>,
    pub flags: u16,
    pub labels: Vec<String>,
    /// Blob bytes after the label ids (see `hdr::encode_rest`).
    pub rest: Vec<u8>,
}

impl Encoded {
    pub(crate) fn new(info: HeaderInfo, path: &str) -> Self {
        let mut rest = Vec::with_capacity(256);
        hdr::encode_rest(&mut rest, path, &info);
        Encoded { date: info.date, flags: info.flags, labels: info.labels, rest }
    }
}

/// One parsed candidate message.
struct Seg {
    from_start: usize,
    content_start: usize,
    /// Body start (just after the blank line), or segment end if there is none.
    body_start: usize,
    content_length: Option<u64>,
    enc: Encoded,
}

fn parse_seg(data: &[u8], from_start: usize, seg_end: usize) -> Seg {
    let line = line_at(data, from_start);
    let mut content_start = from_start + line.len();
    if content_start < data.len() && data[content_start] == b'\n' {
        content_start += 1;
    }
    let content_start = content_start.min(seg_end.max(from_start));
    let region = &data[content_start..seg_end.max(content_start)];
    let region = &region[..region.len().min(MAX_HEADER)];
    let hend = hdr::header_end(region);
    let block = &region[..hend.unwrap_or(region.len())];
    let mut info = hdr::info_from_bytes(block).unwrap_or_default();
    if info.date.is_none() {
        info.date = from_line_date(line);
    }
    let content_length = find_content_length(block);
    let body_start = content_start + hend.unwrap_or(region.len());
    hdr::settle_attachments(&mut info, data.get(body_start..seg_end).unwrap_or_default());
    Seg { from_start, content_start, body_start, content_length, enc: Encoded::new(info, "") }
}

fn find_content_length(block: &[u8]) -> Option<u64> {
    let mut pos = 0;
    while pos < block.len() {
        let end = memchr::memchr(b'\n', &block[pos..]).map_or(block.len(), |n| pos + n);
        let line = &block[pos..end];
        if line.len() > 15 && line[..15].eq_ignore_ascii_case(b"content-length:") {
            let v = std::str::from_utf8(&line[15..]).ok()?.trim();
            return v.parse().ok();
        }
        pos = end + 1;
    }
    None
}

/// Final message boundaries for the candidates.
pub(crate) struct Msg {
    pub from_start: usize,
    pub content_start: usize,
    pub end: usize,
    pub flags_extra: u16,
    pub enc: Encoded,
}

/// Parses headers of all candidates in parallel, then resolves Content-Length and trims separators.
pub(crate) fn build_messages(data: &[u8], cands: &[usize]) -> Vec<Msg> {
    let len = data.len();
    let segs: Vec<Seg> = cands
        .par_iter()
        .enumerate()
        .map(|(i, &c)| parse_seg(data, c, cands.get(i + 1).copied().unwrap_or(len)))
        .collect();
    let mut out = Vec::with_capacity(segs.len());
    let mut it = segs.into_iter().enumerate();
    while let Some((i, seg)) = it.next() {
        let mut next = i + 1;
        let mut end = cands.get(next).copied().unwrap_or(len);
        let mut extra = 0;
        if let Some(cl) = seg.content_length {
            let body_end = (seg.body_start as u64).saturating_add(cl);
            if body_end > end as u64 && body_end <= len as u64 {
                // Honor only if it lands exactly on a separator (after optional blank lines) or EOF.
                let mut j = body_end as usize;
                for _ in 0..2 {
                    if data[j..].starts_with(b"\r\n") {
                        j += 2;
                    } else if data[j..].starts_with(b"\n") {
                        j += 1;
                    }
                }
                let target = if j >= len { Some(cands.len()) } else { cands.binary_search(&j).ok() };
                if let Some(k) = target.filter(|&k| k > i + 1) {
                    next = k;
                    end = body_end as usize;
                    extra = F_NO_UNESCAPE;
                }
            }
        }
        if extra == 0 {
            // Strip the one line break that separates this message from the next `From_` line.
            if end > seg.content_start && data[end - 1] == b'\n' {
                end -= 1;
                if end > seg.content_start && data[end - 1] == b'\r' {
                    end -= 1;
                }
            }
        }
        out.push(Msg {
            from_start: seg.from_start,
            content_start: seg.content_start,
            end: end.max(seg.content_start),
            flags_extra: extra,
            enc: seg.enc,
        });
        // Candidates swallowed by an honored Content-Length.
        if next > i + 1 {
            it.nth(next - i - 2);
        }
    }
    out
}

/// mboxrd/mboxo read-side unescape: one `>` is removed from lines matching `^>+From `.
pub(crate) fn unescape(bytes: &[u8]) -> Cow<'_, [u8]> {
    if memmem::find(bytes, b">From ").is_none() {
        return Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    while pos < bytes.len() {
        let end = memchr::memchr(b'\n', &bytes[pos..]).map_or(bytes.len(), |n| pos + n + 1);
        let line = &bytes[pos..end];
        let gt = line.iter().take_while(|c| **c == b'>').count();
        if gt > 0 && line[gt..].starts_with(b"From ") {
            out.extend_from_slice(&line[1..]);
        } else {
            out.extend_from_slice(line);
        }
        pos = end;
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_line_validation() {
        let d = b"From - Sat Jan  3 01:05:34 1996\n";
        assert!(is_from_line(d, 0));
        assert!(!is_from_line(b"From the desk of Bob\n", 0));
        assert!(!is_from_line(b"From: a@b 10:30\n", 0));
        assert!(!is_from_line(b"From \n", 0));
        assert!(looks_like_mbox(b"\xEF\xBB\xBF\r\n\nFrom a@b Mon Feb  1 10:00:00 2021\n"));
        assert!(!looks_like_mbox(b"Subject: x\n\nFrom a@b Mon Feb  1 10:00:00 2021\n"));
    }

    #[test]
    fn asctime_dates() {
        assert_eq!(from_line_date(b"From - Thu Jan  1 00:00:10 1970"), Some(10_000));
        assert_eq!(
            from_line_date(b"From 123@xxx Sat Jan 01 01:00:00 +0100 2022"),
            Some(days_from_civil(2022, 1, 1) * MS_PER_DAY)
        );
        assert_eq!(from_line_date(b"From x nothing"), None);
        // Out-of-range / overflowing times are rejected; an unparsable zone is ignored.
        assert_eq!(from_line_date(b"From x Thu Jan  1 99999999999999999:00:00 1970"), None);
        assert_eq!(from_line_date(b"From x Thu Jan  1 24:00:00 1970"), None);
        assert_eq!(from_line_date(b"From x Thu Jan  1 00:00:10 +ab12 1970"), Some(10_000));
    }

    #[test]
    fn seam_handling_finds_every_boundary() {
        // Place a separator straddling each possible position relative to a chunk seam.
        let msg = b"From a@b Mon Feb  1 10:00:00 2021\nSubject: s\n\nbody\n\n";
        let mut data = Vec::new();
        data.extend_from_slice(msg);
        while data.len() < CHUNK - 3 {
            data.extend_from_slice(b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n");
        }
        data.truncate(CHUNK - 3);
        data.push(b'\n');
        data.extend_from_slice(msg);
        data.extend_from_slice(msg);
        let c = find_from_lines(&data, 0);
        assert_eq!(c.len(), 3);
        assert_eq!(c[1], CHUNK - 2);
    }

    #[test]
    fn unescape_rd() {
        let b = b"a\n>From x\n>>From y\n> From z\nFrom w\n";
        assert_eq!(&*unescape(b), b"a\nFrom x\n>From y\n> From z\nFrom w\n");
    }
}
