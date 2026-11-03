//! `MailSource` implementations: a single `.msg` / TNEF file, or a directory tree of them.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use mail_model::{
    Error, Fingerprint, Folder, FolderId, FolderKind, Header, Locator, MailSource, Message, PropertyView, Result, Sort,
    SortKey, SourceKind, SourceOpener,
};
use rayon::prelude::*;

use crate::msg::Load;
use crate::node::{guard, split_locator, Node};
use crate::tnef;
use crate::value::{Fnv, CFB_MAGIC};

const HEAD_HASH_BYTES: usize = 64 * 1024;
/// Bounds for the cheap directory sniff.
const SNIFF_MAX_ENTRIES: usize = 20_000;
const SNIFF_MAX_DEPTH: usize = 6;

/// `SourceOpener` for `.msg` files, directory trees of them, and standalone TNEF files.
pub struct MsgOpener;

fn is_message_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".msg") || lower.ends_with(".tnef") || lower == "winmail.dat"
}

fn read_head(path: &Path, n: usize) -> Option<Vec<u8>> {
    let mut f = fs::File::open(path).ok()?;
    let mut buf = vec![0; n];
    let mut got = 0;
    while got < n {
        match f.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(k) => got += k,
            Err(_) => return None,
        }
    }
    buf.truncate(got);
    Some(buf)
}

fn dir_has_messages(root: &Path) -> bool {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > SNIFF_MAX_ENTRIES {
                return false;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_file() && e.file_name().to_str().is_some_and(|n| n.to_ascii_lowercase().ends_with(".msg")) {
                return true;
            }
            if ft.is_dir() && depth < SNIFF_MAX_DEPTH {
                stack.push((e.path(), depth + 1));
            }
        }
    }
    false
}

impl SourceOpener for MsgOpener {
    fn sniff(&self, path: &Path) -> Option<SourceKind> {
        if path.is_dir() {
            return dir_has_messages(path).then_some(SourceKind::Msg);
        }
        let head = read_head(path, 8)?;
        if tnef::has_signature(&head) {
            return Some(SourceKind::Tnef);
        }
        if head.starts_with(&CFB_MAGIC) {
            let ext_msg = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("msg"));
            if ext_msg {
                return Some(SourceKind::Msg);
            }
            // Other compound files (.doc, .xls, .oft templates...) only qualify with a MSG property stream.
            let is_msg = guard(|| {
                let c = cfb::open(path).map_err(Error::Io)?;
                Ok(c.is_stream("/__properties_version1.0"))
            });
            return matches!(is_msg, Ok(true)).then_some(SourceKind::Msg);
        }
        None
    }

    fn open(&self, path: &Path) -> Result<Box<dyn MailSource>> {
        Ok(Box::new(FileSource::open(path)?))
    }
}

struct FileEntry {
    /// Path relative to the source root with `/` separators; empty for single-file sources.
    rel: String,
    folder: String,
    size: u64,
}

/// Ascending permutation of a folder's cached headers, keyed by (folder id, `SortKey as u8`).
type OrderCache = Mutex<HashMap<(String, u8), Arc<Vec<u32>>>>;

pub(crate) struct FileSource {
    root: PathBuf,
    kind: SourceKind,
    is_dir: bool,
    /// Sorted by (folder, rel) so each folder is a contiguous range.
    files: Vec<FileEntry>,
    by_rel: HashMap<String, usize>,
    folders: Vec<Folder>,
    folder_ranges: HashMap<String, Range<usize>>,
    /// Per-folder headers (in `files` order), built on the first non-native sort.
    headers: Mutex<HashMap<String, Arc<Vec<Header>>>>,
    orders: OrderCache,
}

fn mtime_ms(md: &fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// Recursively collect message files under `root` as (rel path, size, mtime).
fn scan_dir(root: &Path) -> Result<Vec<(String, u64, i64)>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if dir == root => return Err(Error::Io(e)),
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "skipping unreadable directory");
                continue;
            }
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let Some(name) = e.file_name().to_str().map(str::to_owned) else { continue };
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if ft.is_dir() {
                stack.push((e.path(), child_rel));
            } else if ft.is_file() && is_message_file(&name) {
                let Ok(md) = e.metadata() else { continue };
                out.push((child_rel, md.len(), mtime_ms(&md)));
            }
        }
    }
    out.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn parent_of(rel: &str) -> &str {
    rel.rfind('/').map_or("", |i| &rel[..i])
}

fn file_name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

impl FileSource {
    pub fn open(path: &Path) -> Result<Self> {
        let md = fs::metadata(path)?;
        let root = path.to_path_buf();
        let display = file_name_of(path);
        if md.is_dir() {
            let scanned = scan_dir(path)?;
            let mut files: Vec<FileEntry> = scanned
                .into_iter()
                .map(|(rel, size, _)| FileEntry { folder: parent_of(&rel).to_owned(), rel, size })
                .collect();
            files.sort_unstable_by(|a, b| a.folder.cmp(&b.folder).then_with(|| a.rel.cmp(&b.rel)));

            let mut folder_ranges: HashMap<String, Range<usize>> = HashMap::new();
            for (i, f) in files.iter().enumerate() {
                folder_ranges.entry(f.folder.clone()).and_modify(|r| r.end = i + 1).or_insert(i..i + 1);
            }
            // Every ancestor of a folder with messages is a folder too.
            let mut ids: Vec<String> = Vec::new();
            for f in folder_ranges.keys() {
                let mut cur = f.as_str();
                loop {
                    ids.push(cur.to_owned());
                    if cur.is_empty() {
                        break;
                    }
                    cur = parent_of(cur);
                }
            }
            ids.push(String::new());
            ids.sort_unstable();
            ids.dedup();
            let folders = ids
                .iter()
                .map(|id| {
                    let count = folder_ranges.get(id).map_or(0, |r| r.len() as u64);
                    Folder {
                        id: FolderId(id.clone()),
                        parent: (!id.is_empty()).then(|| FolderId(parent_of(id).to_owned())),
                        name: if id.is_empty() {
                            display.clone()
                        } else {
                            id.rsplit('/').next().unwrap_or(id).to_owned()
                        },
                        kind: FolderKind::Mail,
                        message_count: Some(count),
                        unread_count: None,
                    }
                })
                .collect();
            let by_rel = files.iter().enumerate().map(|(i, f)| (f.rel.clone(), i)).collect();
            Ok(FileSource {
                root,
                kind: SourceKind::Msg,
                is_dir: true,
                files,
                by_rel,
                folders,
                folder_ranges,
                headers: Mutex::default(),
                orders: Mutex::default(),
            })
        } else {
            let head = read_head(path, 8).unwrap_or_default();
            let kind = if tnef::has_signature(&head) {
                SourceKind::Tnef
            } else if head.starts_with(&CFB_MAGIC) {
                SourceKind::Msg
            } else {
                return Err(Error::Format(format!("{}: not an Outlook .msg or TNEF file", path.display())));
            };
            let files = vec![FileEntry { rel: String::new(), folder: String::new(), size: md.len() }];
            let folders = vec![Folder {
                id: FolderId(String::new()),
                parent: None,
                name: display,
                kind: FolderKind::Mail,
                message_count: Some(1),
                unread_count: None,
            }];
            Ok(FileSource {
                root,
                kind,
                is_dir: false,
                files,
                by_rel: HashMap::from([(String::new(), 0)]),
                folders,
                folder_ranges: HashMap::from([(String::new(), 0..1)]),
                headers: Mutex::default(),
                orders: Mutex::default(),
            })
        }
    }

    fn abs(&self, e: &FileEntry) -> PathBuf {
        if self.is_dir {
            self.root.join(&e.rel)
        } else {
            self.root.clone()
        }
    }

    /// Map the file and run `f` on its bytes.
    fn with_bytes<T>(&self, e: &FileEntry, f: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
        let path = self.abs(e);
        let file = fs::File::open(&path)?;
        if file.metadata()?.len() == 0 {
            return Err(Error::Format(format!("{}: empty file", path.display())));
        }
        // SAFETY: read-only mapping of a file we only read; concurrent truncation by another
        // process is the documented hazard of memory maps and is not expected for mail archives.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        guard(|| f(&map))
    }

    fn entry_for(&self, loc: &Locator) -> Result<(&FileEntry, Vec<u32>)> {
        let (rel, chain) = split_locator(loc)?;
        let i =
            *self.by_rel.get(rel).ok_or_else(|| Error::NotFound(format!("no message file {rel:?} in this source")))?;
        Ok((&self.files[i], chain))
    }

    fn header_of(&self, e: &FileEntry) -> Result<Header> {
        let m = self.with_bytes(e, |b| Node::open(b)?.message(Load::Header))?;
        Ok(m.to_header(Some(Locator::File { path: e.rel.clone() }), Some(FolderId(e.folder.clone()))))
    }

    /// Header for listing: unreadable files still get a row (named after the file) so the
    /// list matches `count()`; opening it reports the decode error.
    fn list_header(&self, e: &FileEntry) -> Header {
        self.header_of(e).unwrap_or_else(|err| {
            tracing::warn!(file = %e.rel, error = %err, "unreadable message file");
            Header {
                locator: Some(Locator::File { path: e.rel.clone() }),
                folder: Some(FolderId(e.folder.clone())),
                subject: Path::new(&e.rel)
                    .file_name()
                    .map_or_else(|| file_name_of(&self.root), |n| n.to_string_lossy().into_owned()),
                size: e.size,
                ..Header::default()
            }
        })
    }

    fn range_of(&self, folder: &FolderId) -> Result<Range<usize>> {
        if let Some(r) = self.folder_ranges.get(&folder.0) {
            return Ok(r.clone());
        }
        if self.folders.iter().any(|f| f.id == *folder) {
            return Ok(0..0);
        }
        Err(Error::NotFound(format!("folder {:?}", folder.0)))
    }

    fn folder_headers(&self, folder: &str, range: Range<usize>) -> Arc<Vec<Header>> {
        if let Some(h) = self.headers.lock().expect("headers lock").get(folder) {
            return h.clone();
        }
        let built: Arc<Vec<Header>> = Arc::new(self.files[range].par_iter().map(|e| self.list_header(e)).collect());
        self.headers.lock().expect("headers lock").entry(folder.to_owned()).or_insert(built).clone()
    }

    fn folder_order(&self, folder: &str, key: SortKey, headers: &[Header]) -> Arc<Vec<u32>> {
        let k = (folder.to_owned(), key as u8);
        if let Some(o) = self.orders.lock().expect("orders lock").get(&k) {
            return o.clone();
        }
        let mut idx: Vec<u32> = (0..headers.len() as u32).collect();
        match key {
            SortKey::Date => idx.sort_by_key(|&i| headers[i as usize].date),
            SortKey::Start => idx.sort_by_key(|&i| headers[i as usize].start_ms()),
            SortKey::Size => idx.sort_by_key(|&i| headers[i as usize].size),
            SortKey::Subject => {
                let keys: Vec<String> = headers.iter().map(|h| h.subject.to_lowercase()).collect();
                idx.sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]));
            }
            SortKey::From => {
                let keys: Vec<String> = headers
                    .iter()
                    .map(|h| {
                        h.from
                            .as_ref()
                            .and_then(|a| a.name.as_deref().filter(|n| !n.is_empty()).or(a.email.as_deref()))
                            .unwrap_or("")
                            .to_lowercase()
                    })
                    .collect();
                idx.sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]));
            }
            SortKey::Native => {}
        }
        let idx = Arc::new(idx);
        self.orders.lock().expect("orders lock").entry(k).or_insert(idx).clone()
    }
}

/// Clamp a `u64` page range to `len` items.
fn clamp(range: &Range<u64>, len: usize) -> Range<usize> {
    let s = (range.start.min(len as u64)) as usize;
    let e = (range.end.min(len as u64)) as usize;
    s..e.max(s)
}

impl MailSource for FileSource {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn display_name(&self) -> String {
        file_name_of(&self.root)
    }

    fn fingerprint(&self) -> Result<Fingerprint> {
        if self.is_dir {
            let scanned = scan_dir(&self.root)?;
            let mut h = Fnv::new();
            let (mut size, mut mtime) = (0u64, 0i64);
            for (rel, s, m) in &scanned {
                size += s;
                mtime = mtime.max(*m);
                h.write(rel.as_bytes());
                h.write(&s.to_le_bytes());
                h.write(&m.to_le_bytes());
            }
            Ok(Fingerprint { path: self.root.clone(), size, mtime, head_hash: h.0 })
        } else {
            let md = fs::metadata(&self.root)?;
            let mut h = Fnv::new();
            h.write(&read_head(&self.root, HEAD_HASH_BYTES).unwrap_or_default());
            Ok(Fingerprint { path: self.root.clone(), size: md.len(), mtime: mtime_ms(&md), head_hash: h.0 })
        }
    }

    fn folders(&self) -> Result<Vec<Folder>> {
        Ok(self.folders.clone())
    }

    fn count(&self, folder: &FolderId) -> Result<u64> {
        Ok(self.range_of(folder)?.len() as u64)
    }

    fn list(&self, folder: &FolderId, range: Range<u64>, sort: Sort) -> Result<Vec<Header>> {
        let fr = self.range_of(folder)?;
        let n = fr.len();
        let page = clamp(&range, n);
        if page.is_empty() {
            return Ok(Vec::new());
        }
        // Position `p` of the requested order → index within the folder.
        let pos = |p: usize| if sort.descending { n - 1 - p } else { p };
        if sort.key == SortKey::Native {
            let files = &self.files[fr.clone()];
            return Ok(page.into_par_iter().map(|p| self.list_header(&files[pos(p)])).collect());
        }
        let headers = self.folder_headers(&folder.0, fr);
        let order = self.folder_order(&folder.0, sort.key, &headers);
        Ok(page.map(|p| headers[order[pos(p)] as usize].clone()).collect())
    }

    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_> {
        Box::new(self.files.iter().map(|e| self.header_of(e)))
    }

    fn message(&self, loc: &Locator) -> Result<Message> {
        let (e, chain) = self.entry_for(loc)?;
        let m = self.with_bytes(e, |b| Node::open(b)?.walk(&chain)?.message(Load::Full))?;
        Ok(m.to_message(Some(loc.clone()), Some(FolderId(e.folder.clone()))))
    }

    fn attachment(&self, loc: &Locator, index: u32) -> Result<Vec<u8>> {
        let (e, chain) = self.entry_for(loc)?;
        self.with_bytes(e, |b| Node::open(b)?.walk(&chain)?.attachment(index))
    }

    fn raw_mime(&self, _loc: &Locator) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn properties(&self, loc: &Locator) -> Result<Vec<PropertyView>> {
        let (e, chain) = self.entry_for(loc)?;
        let m = self.with_bytes(e, |b| Node::open(b)?.walk(&chain)?.message(Load::Full))?;
        Ok(mapi_props::convert::property_views(&m.mapi.props))
    }
}
