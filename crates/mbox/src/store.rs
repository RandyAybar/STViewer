//! Compact in-memory rows for one container unit (an mbox file or a directory of
//! eml/emlx files), lazily computed sort permutations, and the versioned sidecar cache.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mail_model::{Header, Result, SortKey};
use rayon::prelude::*;

use crate::hdr::{self, HeaderInfo, F_DELETED, F_PARTIAL, F_READ};
use crate::scan::Encoded;
use crate::util::{self, hash, mtime_ms, put_str, read_at, Reader, HEAD_LEN};
use crate::{emlx, scan};

/// One message. File-backed units store `mtime` in `off` and the file size in `len`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    /// mbox: first byte after the `From_` line. Files: mtime (ms).
    pub off: u64,
    /// mbox: message byte length in the file (escaped). Files: file size.
    pub len: u64,
    /// UTC ms, `i64::MIN` when unknown.
    pub date: i64,
    /// Start of this row's blob in the arena (end = next row's start).
    pub blob: u64,
    /// mbox: length of the `From_` line incl. its line break.
    pub delta: u32,
    pub flags: u16,
}

const ROW_BYTES: usize = 8 + 8 + 8 + 8 + 4 + 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Filter {
    /// Not deleted.
    Visible,
    Deleted,
    Label(u32),
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Count {
    pub total: u64,
    pub unread: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitKind {
    Mbox,
    Eml,
    Emlx,
}

impl UnitKind {
    fn tag(self) -> u8 {
        match self {
            UnitKind::Mbox => 0,
            UnitKind::Eml => 1,
            UnitKind::Emlx => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadOutcome {
    /// Parsed from scratch.
    Fresh,
    /// Loaded entirely from the sidecar cache.
    Cached,
    /// Cache reused; only new/changed content parsed.
    Incremental,
}

/// Sorted row-index permutations keyed by (filter, sort key tag).
type ViewCache = HashMap<(Filter, u8), Arc<Vec<u32>>>;

pub(crate) struct UnitData {
    pub rows: Vec<Row>,
    pub arena: Vec<u8>,
    pub labels: Vec<String>,
    /// (size, mtime) of the mbox file this data describes; (0,0) for file units.
    pub stamp: (u64, i64),
    pub visible: Count,
    pub deleted: Count,
    pub per_label: Vec<Count>,
    views: Mutex<ViewCache>,
}

impl UnitData {
    fn new(rows: Vec<Row>, arena: Vec<u8>, labels: Vec<String>, stamp: (u64, i64)) -> Self {
        let mut d = UnitData {
            rows,
            arena,
            labels,
            stamp,
            visible: Count::default(),
            deleted: Count::default(),
            per_label: Vec::new(),
            views: Mutex::new(HashMap::new()),
        };
        let mut per_label = vec![Count::default(); d.labels.len()];
        for i in 0..d.rows.len() {
            let r = d.rows[i];
            let unread = u64::from(r.flags & F_READ == 0);
            if r.flags & F_DELETED != 0 {
                d.deleted.total += 1;
                d.deleted.unread += unread;
                continue;
            }
            d.visible.total += 1;
            d.visible.unread += unread;
            for l in hdr::blob_labels(d.blob(i)) {
                if let Some(c) = per_label.get_mut(l as usize) {
                    c.total += 1;
                    c.unread += unread;
                }
            }
        }
        d.per_label = per_label;
        d
    }

    pub(crate) fn empty(stamp: (u64, i64)) -> Self {
        UnitData::new(Vec::new(), Vec::new(), Vec::new(), stamp)
    }

    pub(crate) fn blob(&self, i: usize) -> &[u8] {
        let start = self.rows[i].blob as usize;
        let end = self.rows.get(i + 1).map_or(self.arena.len(), |r| r.blob as usize);
        self.arena.get(start..end.max(start)).unwrap_or_default()
    }

    pub(crate) fn count(&self, f: Filter) -> Count {
        match f {
            Filter::Visible => self.visible,
            Filter::Deleted => self.deleted,
            Filter::Label(l) => self.per_label.get(l as usize).copied().unwrap_or_default(),
        }
    }

    fn matches(&self, i: usize, f: Filter) -> bool {
        let deleted = self.rows[i].flags & F_DELETED != 0;
        match f {
            Filter::Visible => !deleted,
            Filter::Deleted => deleted,
            Filter::Label(l) => !deleted && hdr::blob_labels(self.blob(i)).any(|x| x == l),
        }
    }

    /// Row indices of `filter` ordered ascending by `key` (computed once per pair).
    pub(crate) fn view(&self, filter: Filter, key: SortKey) -> Arc<Vec<u32>> {
        let tag = sort_tag(key);
        if let Some(v) = self.views.lock().expect("views lock").get(&(filter, tag)) {
            return v.clone();
        }
        let base: Vec<u32> = if key == SortKey::Native {
            (0..self.rows.len()).filter(|&i| self.matches(i, filter)).map(|i| i as u32).collect()
        } else {
            self.view(filter, SortKey::Native).as_ref().clone()
        };
        let mut v = base;
        match key {
            SortKey::Native => {}
            // mbox rows carry no calendar data: Start falls back to the date, as documented.
            SortKey::Date | SortKey::Start => v.sort_unstable_by_key(|&i| (self.rows[i as usize].date, i)),
            SortKey::Size => v.sort_unstable_by_key(|&i| (self.rows[i as usize].len, i)),
            SortKey::Subject | SortKey::From => {
                let mut keyed: Vec<(String, u32)> = v
                    .par_iter()
                    .map(|&i| {
                        let (subj, from) = hdr::blob_sort_fields(self.blob(i as usize)).unwrap_or(("", ""));
                        let k = if key == SortKey::Subject {
                            hdr::sort_subject(subj)
                        } else {
                            from.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace()).to_lowercase()
                        };
                        (k, i)
                    })
                    .collect();
                keyed.par_sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                v = keyed.into_iter().map(|(_, i)| i).collect();
            }
        }
        let v = Arc::new(v);
        self.views.lock().expect("views lock").insert((filter, tag), v.clone());
        v
    }

    /// Model header for row `i` (locator/folder filled by the caller).
    pub(crate) fn header(&self, i: usize) -> Header {
        let r = self.rows[i];
        let mut h = Header {
            date: (r.date != i64::MIN).then_some(r.date),
            size: r.len,
            has_attachments: r.flags & hdr::F_HAS_ATT != 0,
            flags: hdr::flags_to_model(r.flags),
            ..Header::default()
        };
        if hdr::decode_into(self.blob(i), &self.labels, &mut h).is_none() {
            h.subject = String::from("(corrupt cache entry)");
        }
        h
    }

    /// Index of the mbox row starting at `off`.
    pub(crate) fn find_offset(&self, off: u64) -> Option<usize> {
        self.rows.binary_search_by_key(&off, |r| r.off).ok()
    }
}

fn sort_tag(k: SortKey) -> u8 {
    match k {
        SortKey::Date | SortKey::Start => 0,
        SortKey::From => 1,
        SortKey::Subject => 2,
        SortKey::Size => 3,
        SortKey::Native => 4,
    }
}

/// Accumulates rows + blobs with label interning.
struct Builder {
    rows: Vec<Row>,
    arena: Vec<u8>,
    labels: Vec<String>,
    label_ids: HashMap<String, u32>,
}

impl Builder {
    fn new(labels: Vec<String>) -> Self {
        let label_ids = labels.iter().enumerate().map(|(i, l)| (l.clone(), i as u32)).collect();
        Builder { rows: Vec::new(), arena: Vec::new(), labels, label_ids }
    }

    fn reserve(&mut self, rows: usize, bytes: usize) {
        self.rows.reserve(rows);
        self.arena.reserve(bytes);
    }

    fn push(&mut self, mut row: Row, enc: &Encoded) {
        row.blob = self.arena.len() as u64;
        crate::util::put_varint(&mut self.arena, enc.labels.len() as u64);
        for l in &enc.labels {
            let id = match self.label_ids.get(l) {
                Some(&id) => id,
                None => {
                    let id = self.labels.len() as u32;
                    self.labels.push(l.clone());
                    self.label_ids.insert(l.clone(), id);
                    id
                }
            };
            crate::util::put_varint(&mut self.arena, u64::from(id));
        }
        self.arena.extend_from_slice(&enc.rest);
        self.rows.push(row);
    }

    /// Copies an existing row + blob verbatim (labels are shared ids).
    fn push_raw(&mut self, mut row: Row, blob: &[u8]) {
        row.blob = self.arena.len() as u64;
        self.arena.extend_from_slice(blob);
        self.rows.push(row);
    }

    fn finish(self, stamp: (u64, i64)) -> UnitData {
        UnitData::new(self.rows, self.arena, self.labels, stamp)
    }
}

// ---- sidecar cache ----

const MAGIC: &[u8; 8] = b"STVMBOX\0";
/// 2: `F_HAS_ATT` of multipart/mixed messages comes from their part headers.
const VERSION: u32 = 2;

struct CacheFile {
    size: u64,
    mtime: i64,
    prefix_len: u64,
    prefix_hash: u64,
    rescan_from: u64,
    rows: Vec<Row>,
    arena: Vec<u8>,
    labels: Vec<String>,
}

/// Cache file location for a unit path inside `dir`.
pub(crate) fn cache_path(dir: &Path, unit: &Path, kind: UnitKind) -> PathBuf {
    let key = unit.canonicalize().unwrap_or_else(|_| unit.to_path_buf());
    let h = hash(format!("{}|{}", kind.tag(), key.to_string_lossy()).as_bytes());
    dir.join(format!("{h:016x}.stvc"))
}

fn read_cache(path: &Path, kind: UnitKind) -> Option<CacheFile> {
    let buf = std::fs::read(path).ok()?;
    if buf.len() < 16 || &buf[..8] != MAGIC {
        return None;
    }
    let (body, sum) = buf.split_at(buf.len() - 8);
    if hash(body) != u64::from_le_bytes(sum.try_into().ok()?) {
        return None;
    }
    let mut r = Reader::new(body);
    r.bytes(8)?;
    if r.u32()? != VERSION {
        return None;
    }
    if *r.bytes(1)?.first()? != kind.tag() {
        return None;
    }
    let size = r.u64()?;
    let mtime = r.u64()? as i64;
    let prefix_len = r.u64()?;
    let prefix_hash = r.u64()?;
    let rescan_from = r.u64()?;
    let nrows = r.u64()? as usize;
    let narena = r.u64()? as usize;
    let nlabels = r.u64()? as usize;
    let raw_rows = r.bytes(nrows.checked_mul(ROW_BYTES)?)?;
    let mut rows = Vec::with_capacity(nrows);
    for c in raw_rows.as_chunks::<ROW_BYTES>().0 {
        let mut rr = Reader::new(c);
        rows.push(Row {
            off: rr.u64()?,
            len: rr.u64()?,
            date: rr.u64()? as i64,
            blob: rr.u64()?,
            delta: rr.u32()?,
            flags: rr.u16()?,
        });
    }
    let arena_start = r.pos;
    r.bytes(narena)?;
    let arena_end = r.pos;
    if rows.iter().any(|row| row.blob > narena as u64) || rows.windows(2).any(|w| w[0].blob > w[1].blob) {
        return None;
    }
    let mut labels = Vec::with_capacity(nlabels.min(1 << 16));
    for _ in 0..nlabels {
        labels.push(r.str()?.to_string());
    }
    // Reuse the read buffer as the arena (no second 10s-of-MB allocation).
    let mut arena = buf;
    arena.truncate(arena_end);
    arena.drain(..arena_start);
    Some(CacheFile { size, mtime, prefix_len, prefix_hash, rescan_from, rows, arena, labels })
}

/// Writer that hashes everything it writes (cache trailer checksum).
struct HashWriter<W: Write> {
    inner: W,
    hasher: xxhash_rust::xxh3::Xxh3,
}

impl<W: Write> HashWriter<W> {
    fn put(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.hasher.update(b);
        self.inner.write_all(b)
    }
}

struct CacheMeta {
    kind: UnitKind,
    size: u64,
    mtime: i64,
    prefix: (u64, u64),
    rescan_from: u64,
}

fn write_cache(path: &Path, m: &CacheMeta, d: &UnitData) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}-{seq}", std::process::id()));
    let res = (|| {
        let file = File::create(&tmp)?;
        let mut w = HashWriter {
            inner: std::io::BufWriter::with_capacity(1 << 20, file),
            hasher: xxhash_rust::xxh3::Xxh3::new(),
        };
        w.put(MAGIC)?;
        w.put(&VERSION.to_le_bytes())?;
        w.put(&[m.kind.tag()])?;
        for v in [m.size, m.mtime as u64, m.prefix.0, m.prefix.1, m.rescan_from] {
            w.put(&v.to_le_bytes())?;
        }
        for v in [d.rows.len() as u64, d.arena.len() as u64, d.labels.len() as u64] {
            w.put(&v.to_le_bytes())?;
        }
        let mut rec = [0u8; ROW_BYTES];
        for r in &d.rows {
            rec[0..8].copy_from_slice(&r.off.to_le_bytes());
            rec[8..16].copy_from_slice(&r.len.to_le_bytes());
            rec[16..24].copy_from_slice(&r.date.to_le_bytes());
            rec[24..32].copy_from_slice(&r.blob.to_le_bytes());
            rec[32..36].copy_from_slice(&r.delta.to_le_bytes());
            rec[36..38].copy_from_slice(&r.flags.to_le_bytes());
            w.put(&rec)?;
        }
        w.put(&d.arena)?;
        let mut lb = Vec::new();
        for l in &d.labels {
            put_str(&mut lb, l);
        }
        w.put(&lb)?;
        let sum = w.hasher.digest();
        w.inner.write_all(&sum.to_le_bytes())?;
        w.inner.flush()
    })();
    match res.and_then(|()| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn save(
    cache: Option<&Path>,
    kind: UnitKind,
    d: &UnitData,
    size: u64,
    mtime: i64,
    prefix: (u64, u64),
    rescan_from: u64,
) {
    let Some(path) = cache else { return };
    // A read-only or full cache dir only costs reopen speed.
    let _ = write_cache(path, &CacheMeta { kind, size, mtime, prefix, rescan_from }, d);
}

// ---- mbox loading ----

/// Checks that sampled cached rows still sit right after a `From_` line.
fn rows_still_valid(file: &File, rows: &[Row]) -> bool {
    if rows.is_empty() {
        return true;
    }
    let step = (rows.len() / 32).max(1);
    let samples = (0..rows.len()).step_by(step).chain(std::iter::once(rows.len() - 1));
    for i in samples {
        let r = rows[i];
        let Some(start) = r.off.checked_sub(u64::from(r.delta)) else { return false };
        match read_at(file, start, 5) {
            Ok(b) if b == b"From " => {}
            _ => return false,
        }
    }
    true
}

pub(crate) fn load_mbox(path: &Path, cache: Option<&Path>) -> Result<(UnitData, LoadOutcome)> {
    let file = File::open(path)?;
    let meta = file.metadata()?;
    let size = meta.len();
    let mtime = mtime_ms(&meta);
    let prefix_len = size.min(HEAD_LEN);
    let prefix_hash = hash(&read_at(&file, 0, prefix_len as usize)?);

    let mut base: Option<(Builder, u64)> = None;
    if let Some(c) = cache.and_then(|p| read_cache(p, UnitKind::Mbox)) {
        let same_prefix = c.prefix_len <= size
            && (c.prefix_len == prefix_len && c.prefix_hash == prefix_hash
                || hash(&read_at(&file, 0, c.prefix_len as usize)?) == c.prefix_hash);
        if same_prefix && c.size == size && c.mtime == mtime {
            let d = UnitData::new(c.rows, c.arena, c.labels, (size, mtime));
            return Ok((d, LoadOutcome::Cached));
        }
        if same_prefix && size > c.size && rows_still_valid(&file, &c.rows) {
            // Appended: keep all rows that start before the rescan point.
            let mut b = Builder::new(c.labels);
            let keep = c.rows.iter().take_while(|r| r.off.saturating_sub(u64::from(r.delta)) < c.rescan_from).count();
            b.rows = c.rows[..keep].to_vec();
            let arena_end = c.rows.get(keep).map_or(c.arena.len(), |r| r.blob as usize);
            b.arena = c.arena;
            b.arena.truncate(arena_end);
            base = Some((b, c.rescan_from));
        }
    }

    if size == 0 {
        let d = UnitData::empty((0, mtime));
        save(cache, UnitKind::Mbox, &d, size, mtime, (prefix_len, prefix_hash), 0);
        return Ok((d, LoadOutcome::Fresh));
    }
    // SAFETY: the map is read-only and dropped before returning; a concurrent writer
    // truncating the file is the documented mmap hazard, mitigated by short lifetime.
    let map = unsafe { memmap2::Mmap::map(&file)? };
    let data: &[u8] = &map;
    let (mut b, start, outcome) = match base {
        Some((b, from)) if (from as usize) < data.len() && scan::is_from_line(data, from as usize) => {
            (b, from as usize, LoadOutcome::Incremental)
        }
        _ => (Builder::new(Vec::new()), scan::skip_preamble(data), LoadOutcome::Fresh),
    };
    let cands = scan::find_from_lines(data, start);
    let msgs = scan::build_messages(data, &cands);
    drop(map);
    b.reserve(msgs.len(), msgs.iter().map(|m| m.enc.rest.len() + 4).sum());
    for m in &msgs {
        let row = Row {
            off: m.content_start as u64,
            len: (m.end - m.content_start) as u64,
            date: m.enc.date.unwrap_or(i64::MIN),
            blob: 0,
            delta: (m.content_start - m.from_start) as u32,
            flags: m.enc.flags | m.flags_extra,
        };
        b.push(row, &m.enc);
    }
    let rescan_from = b.rows.last().map_or(start as u64, |r| r.off - u64::from(r.delta));
    let d = b.finish((size, mtime));
    save(cache, UnitKind::Mbox, &d, size, mtime, (prefix_len, prefix_hash), rescan_from);
    Ok((d, outcome))
}

// ---- eml / emlx directory loading ----

/// One message file of a unit, relative to the unit directory.
#[derive(Clone, Debug)]
pub(crate) struct FileEntry {
    pub rel: String,
    pub size: u64,
    pub mtime: i64,
}

const HEADER_READ: usize = 64 * 1024;
const HEADER_READ_MAX: usize = 1 << 20;

/// Reads the start of a message: at least its header block (when it has one within 1 MiB)
/// and whatever of the body the same read brought in. Returns the bytes and the header length.
fn read_message_head(file: &File, start: u64, avail: u64) -> std::io::Result<(Vec<u8>, usize)> {
    let mut want = HEADER_READ.min(avail as usize);
    loop {
        let buf = read_at(file, start, want)?;
        if let Some(end) = hdr::header_end(&buf) {
            return Ok((buf, end));
        }
        if want as u64 >= avail || want >= HEADER_READ_MAX {
            let end = buf.len();
            return Ok((buf, end));
        }
        want = (want * 4).min(HEADER_READ_MAX).min(avail as usize);
    }
}

/// Header facts from [`read_message_head`]'s result; the body prefix settles a
/// multipart/mixed message's attachment flag.
fn head_info(head: std::io::Result<(Vec<u8>, usize)>) -> HeaderInfo {
    let (buf, end) = head.unwrap_or_default();
    let mut info = hdr::info_from_bytes(&buf[..end]).unwrap_or_default();
    hdr::settle_attachments(&mut info, &buf[end..]);
    info
}

/// Header facts of one eml/emlx file (missing/unreadable files give an empty header).
pub(crate) fn parse_file_header(path: &Path, kind: UnitKind) -> (HeaderInfo, u16) {
    let Ok(file) = File::open(path) else {
        return (HeaderInfo::default(), 0);
    };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut extra = 0;
    let info = if kind == UnitKind::Emlx {
        let Ok(head) = read_at(&file, 0, 64) else {
            return (HeaderInfo::default(), 0);
        };
        let Some((start, _, count)) = emlx::split(&head) else {
            return (HeaderInfo::default(), 0);
        };
        let start = start as u64;
        let payload = count.min(size.saturating_sub(start));
        let mut info = head_info(read_message_head(&file, start, payload));
        let plist_at = start + payload;
        let plist = read_at(&file, plist_at, (size - plist_at).min(64 * 1024) as usize).unwrap_or_default();
        if let Some(f) = emlx::plist_int(&plist, "flags") {
            info.flags = (info.flags & !emlx::STATUS_MASK) | emlx::flag_bits(f);
        }
        if info.date.is_none() {
            info.date = emlx::plist_date(&plist, "date-received").or_else(|| emlx::plist_date(&plist, "date-sent"));
        }
        if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_ascii_lowercase().ends_with(".partial.emlx"))
        {
            extra |= F_PARTIAL;
        }
        info
    } else {
        head_info(read_message_head(&file, 0, size))
    };
    (info, extra)
}

pub(crate) fn load_files(
    dir: &Path,
    files: &[FileEntry],
    kind: UnitKind,
    cache: Option<&Path>,
) -> Result<(UnitData, LoadOutcome)> {
    let cached = cache.and_then(|p| read_cache(p, kind));
    let mut known: HashMap<&str, usize> = HashMap::new();
    let cached_data = cached.map(|c| UnitData::new(c.rows, c.arena, c.labels, (0, 0)));
    if let Some(d) = &cached_data {
        for i in 0..d.rows.len() {
            if let Some(p) = hdr::blob_path(d.blob(i)) {
                known.insert(p, i);
            }
        }
    }
    let reuse: Vec<Option<usize>> = files
        .iter()
        .map(|f| {
            let i = *known.get(f.rel.as_str())?;
            let r = cached_data.as_ref()?.rows[i];
            (r.len == f.size && r.off as i64 == f.mtime).then_some(i)
        })
        .collect();
    if let Some(d) = cached_data.as_ref() {
        if reuse.iter().all(Option::is_some) && d.rows.len() == files.len() {
            let identical = reuse.iter().enumerate().all(|(i, r)| *r == Some(i));
            if identical {
                let d = cached_data.expect("checked above");
                return Ok((d, LoadOutcome::Cached));
            }
        }
    }
    let parsed: Vec<Option<Encoded>> = files
        .par_iter()
        .zip(reuse.par_iter())
        .map(|(f, r)| match r {
            Some(_) => None,
            None => {
                let (mut info, extra) =
                    util::safe_join(dir, &f.rel).map(|p| parse_file_header(&p, kind)).unwrap_or_default();
                info.flags |= extra;
                Some(Encoded::new(info, &f.rel))
            }
        })
        .collect();
    let mut b = Builder::new(cached_data.as_ref().map(|d| d.labels.clone()).unwrap_or_default());
    let mut any_reused = false;
    for ((f, r), p) in files.iter().zip(&reuse).zip(parsed) {
        if let (Some(i), Some(d)) = (r, cached_data.as_ref()) {
            b.push_raw(d.rows[*i], d.blob(*i));
            any_reused = true;
            continue;
        }
        let Some(enc) = p else { continue };
        let row = Row {
            off: f.mtime as u64,
            len: f.size,
            date: enc.date.unwrap_or(i64::MIN),
            blob: 0,
            delta: 0,
            flags: enc.flags,
        };
        b.push(row, &enc);
    }
    let d = b.finish((0, 0));
    save(cache, kind, &d, 0, 0, (0, 0), 0);
    Ok((d, if any_reused { LoadOutcome::Incremental } else { LoadOutcome::Fresh }))
}

/// Reads message bytes of an mbox row, verifying the `From_` line is still where it was.
pub(crate) fn read_mbox_message(path: &Path, row: &Row) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    let start =
        row.off.checked_sub(u64::from(row.delta)).ok_or_else(|| mail_model::Error::Format("bad mbox row".into()))?;
    let total = usize::try_from(row.len + u64::from(row.delta))
        .map_err(|_| mail_model::Error::Unsupported("message larger than address space".into()))?;
    let mut buf = read_at(&file, start, total)?;
    if buf.len() < total || !buf.starts_with(b"From ") {
        return Err(mail_model::Error::NotFound("mbox file changed since it was scanned; reopen the source".into()));
    }
    buf.drain(..row.delta as usize);
    if row.flags & hdr::F_NO_UNESCAPE == 0 {
        if let std::borrow::Cow::Owned(v) = scan::unescape(&buf) {
            buf = v;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mbox(n: usize, start: usize) -> String {
        (start..start + n)
            .map(|i| {
                format!(
                    "From a@b Mon Feb  1 10:00:{:02} 2021\nSubject: m{i}\nX-Gmail-Labels: L{}\nMessage-ID: <{i}@x>\n\nbody {i}\n\n",
                    i % 60,
                    i % 3
                )
            })
            .collect()
    }

    #[test]
    fn cache_roundtrip_append_and_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("box");
        let cache = dir.path().join("c.stvc");
        std::fs::write(&file, mbox(10, 0)).unwrap();
        let (d, o) = load_mbox(&file, Some(&cache)).unwrap();
        assert_eq!((d.rows.len(), o), (10, LoadOutcome::Fresh));
        let (d2, o) = load_mbox(&file, Some(&cache)).unwrap();
        assert_eq!(o, LoadOutcome::Cached);
        assert_eq!(d2.rows, d.rows);
        assert_eq!(d2.arena, d.arena);
        assert_eq!(d2.labels, d.labels);

        // Append: only the tail is scanned, result equals a fresh scan.
        let mut f = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
        f.write_all(mbox(5, 10).as_bytes()).unwrap();
        drop(f);
        let (d3, o) = load_mbox(&file, Some(&cache)).unwrap();
        assert_eq!(o, LoadOutcome::Incremental);
        let (fresh, _) = load_mbox(&file, None).unwrap();
        assert_eq!(d3.rows, fresh.rows);
        assert_eq!(d3.labels, fresh.labels);
        assert_eq!(d3.rows.len(), 15);
        assert_eq!(d3.header(14).subject, "m14");

        // Rewrite with different content: full rescan.
        std::fs::write(&file, mbox(3, 100)).unwrap();
        let (d4, o) = load_mbox(&file, Some(&cache)).unwrap();
        assert_eq!(o, LoadOutcome::Fresh);
        assert_eq!(d4.header(0).subject, "m100");

        // Corrupt cache is ignored.
        let mut bytes = std::fs::read(&cache).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        std::fs::write(&cache, &bytes).unwrap();
        let (d5, o) = load_mbox(&file, Some(&cache)).unwrap();
        assert_eq!((d5.rows.len(), o), (3, LoadOutcome::Fresh));
    }

    #[test]
    fn rows_beyond_4gib_roundtrip_through_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("c.stvc");
        let mut b = Builder::new(Vec::new());
        let info = HeaderInfo { subject: "big".into(), ..Default::default() };
        let row = Row { off: 5 << 32, len: 6 << 32, date: -5, blob: 0, delta: 40, flags: 3 };
        b.push(row, &Encoded::new(info, ""));
        let d = b.finish((7 << 32, 1));
        save(Some(&cache), UnitKind::Mbox, &d, 7 << 32, 1, (1, 2), 3);
        let c = read_cache(&cache, UnitKind::Mbox).unwrap();
        assert_eq!(c.rows[0].off, 5 << 32);
        assert_eq!(c.rows[0].len, 6 << 32);
        assert_eq!(c.size, 7 << 32);
        assert!(read_cache(&cache, UnitKind::Eml).is_none());
    }

    #[test]
    fn views_sort_and_filter() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("box");
        std::fs::write(&file, mbox(9, 0)).unwrap();
        let (d, _) = load_mbox(&file, None).unwrap();
        assert_eq!(d.labels.len(), 3);
        let l0 = d.labels.iter().position(|l| l == "L0").unwrap() as u32;
        let v = d.view(Filter::Label(l0), SortKey::Native);
        assert_eq!(v.as_slice(), &[0, 3, 6]);
        assert_eq!(d.count(Filter::Label(l0)).total, 3);
        let s = d.view(Filter::Visible, SortKey::Subject);
        assert_eq!(s.len(), 9);
    }
}
