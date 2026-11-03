//! Small shared helpers: varint codec, hashing, timestamps, path checks.

use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use mail_model::{Error, Result};

/// Bytes hashed for `Fingerprint::head_hash` and the cache prefix check.
pub(crate) const HEAD_LEN: u64 = 64 * 1024;

pub(crate) fn hash(bytes: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(bytes)
}

pub(crate) fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// Positioned read of exactly `len` bytes (short only at EOF).
pub(crate) fn read_at(file: &File, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut done = 0usize;
    while done < len {
        let n = read_at_once(file, offset + done as u64, &mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    buf.truncate(done);
    Ok(buf)
}

#[cfg(windows)]
fn read_at_once(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(unix)]
fn read_at_once(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

/// Hash of the first `min(HEAD_LEN, len)` bytes of a file.
pub(crate) fn head_hash(file: &File, len: u64) -> std::io::Result<u64> {
    let buf = read_at(file, 0, len.min(HEAD_LEN) as usize)?;
    Ok(hash(&buf))
}

/// Joins a locator-relative path onto `root`, rejecting absolute paths and `..`.
pub(crate) fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            _ => return Err(Error::NotFound(format!("invalid relative path {rel:?}"))),
        }
    }
    Ok(out)
}

/// `a/b` join with '/' separators, treating "" as the root.
pub(crate) fn rel_join(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}/{name}")
    }
}

pub(crate) fn ext_lower(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default()
}

// ---- varint / blob codec ----

pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

pub(crate) fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

pub(crate) fn put_opt(out: &mut Vec<u8>, s: Option<&str>) {
    match s {
        None => out.push(0),
        Some(s) => {
            put_varint(out, s.len() as u64 + 1);
            out.extend_from_slice(s.as_bytes());
        }
    }
}

/// Cursor over a blob; every getter returns `None` on truncated/corrupt data.
pub(crate) struct Reader<'a> {
    pub(crate) buf: &'a [u8],
    pub(crate) pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    pub(crate) fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            let b = *self.buf.get(self.pos)?;
            self.pos += 1;
            if shift > 63 {
                return None;
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
        }
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub(crate) fn str(&mut self) -> Option<&'a str> {
        let n = self.varint()? as usize;
        std::str::from_utf8(self.bytes(n)?).ok()
    }

    pub(crate) fn opt(&mut self) -> Option<Option<&'a str>> {
        let n = self.varint()? as usize;
        if n == 0 {
            return Some(None);
        }
        Some(Some(std::str::from_utf8(self.bytes(n - 1)?).ok()?))
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.bytes(8)?.try_into().ok()?))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.bytes(4)?.try_into().ok()?))
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.bytes(2)?.try_into().ok()?))
    }
}

pub(crate) fn month_index(name: &str) -> Option<u32> {
    const M: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let lower = name.get(..3)?.to_ascii_lowercase();
    M.iter().position(|m| *m == lower).map(|i| i as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip_and_truncation() {
        let mut b = Vec::new();
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, 5 << 32, u64::MAX] {
            put_varint(&mut b, v);
        }
        put_opt(&mut b, None);
        put_opt(&mut b, Some("héllo"));
        let mut r = Reader::new(&b);
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, 5 << 32, u64::MAX] {
            assert_eq!(r.varint(), Some(v));
        }
        assert_eq!(r.opt(), Some(None));
        assert_eq!(r.opt(), Some(Some("héllo")));
        assert_eq!(r.varint(), None);
        let mut r = Reader::new(&[0x80, 0x80]);
        assert_eq!(r.varint(), None);
    }

    #[test]
    fn safe_join_rejects_escape() {
        let root = Path::new("/r");
        assert!(safe_join(root, "../x").is_err());
        assert!(safe_join(root, "a/../../x").is_err());
        assert_eq!(safe_join(root, "a/b.eml").unwrap(), Path::new("/r/a/b.eml"));
    }
}
