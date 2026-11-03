//! `MailSource` over a layout of units (single mbox / eml / emlx file, or a directory tree).

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use mail_model::{
    Error, Fingerprint, Folder, FolderId, FolderKind, Header, Locator, MailSource, Message, PropertyView, Result, Sort,
    SourceKind,
};
use rayon::prelude::*;

use crate::discover::{self, Layout, UnitSpec};
use crate::hdr::{self, F_DELETED, F_PARTIAL};
use crate::store::{self, Filter, LoadOutcome, UnitData, UnitKind};
use crate::util::{self, hash, mtime_ms, rel_join, safe_join};
use crate::{emlx, mime};

/// Construction options.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    /// Directory for sidecar header caches. `None` = `dirs::cache_dir()/stviewer/mbox`.
    pub cache_dir: Option<PathBuf>,
    /// Disable the sidecar cache entirely.
    pub no_cache: bool,
}

impl OpenOptions {
    fn resolved_cache_dir(&self) -> Option<PathBuf> {
        if self.no_cache {
            return None;
        }
        self.cache_dir.clone().or_else(|| dirs::cache_dir().map(|d| d.join("stviewer").join("mbox")))
    }
}

#[derive(Clone, Copy, Debug)]
enum Target {
    Container,
    View(usize, Filter),
}

struct State {
    layout: Arc<Layout>,
    units: Vec<Arc<UnitData>>,
    folders: Vec<Folder>,
    targets: HashMap<String, Target>,
    /// mbox unit rel path → unit index.
    mbox_units: HashMap<String, usize>,
}

/// One mbox / eml / emlx source (single file or directory tree).
pub struct MboxSource {
    root: PathBuf,
    kind: SourceKind,
    single_file: bool,
    cache_dir: Option<PathBuf>,
    state: RwLock<Option<Arc<State>>>,
    /// Layout discovered by `open`, consumed by the first load.
    initial: Mutex<Option<Layout>>,
    load_lock: Mutex<()>,
    outcomes: Mutex<Vec<LoadOutcome>>,
}

fn deleted_id(unit_folder: &str) -> String {
    format!("{unit_folder}#deleted")
}

fn label_id(unit_folder: &str, label: &str) -> String {
    if unit_folder.is_empty() {
        format!("label:{label}")
    } else {
        format!("{unit_folder}#label:{label}")
    }
}

impl MboxSource {
    /// Opens `path` (file or directory). Cheap: the folder layout of directories is
    /// discovered here, messages are scanned on first access.
    pub fn open(path: &Path, opts: &OpenOptions) -> Result<MboxSource> {
        let meta = std::fs::metadata(path)?;
        let (kind, single_file, initial) = if meta.is_dir() {
            let layout = discover::dir_layout(path)?;
            (layout.kind, false, Some(layout))
        } else {
            let kind = discover::sniff_file(path)
                .ok_or_else(|| Error::Format(format!("{} is not an mbox/eml/emlx file", path.display())))?;
            (kind, true, None)
        };
        Ok(MboxSource {
            root: path.to_path_buf(),
            kind,
            single_file,
            cache_dir: opts.resolved_cache_dir(),
            state: RwLock::new(None),
            initial: Mutex::new(initial),
            load_lock: Mutex::new(()),
            outcomes: Mutex::new(Vec::new()),
        })
    }

    /// How each unit was loaded on the most recent (re)load (diagnostics / tests).
    #[doc(hidden)]
    pub fn load_outcomes(&self) -> Vec<&'static str> {
        self.outcomes
            .lock()
            .expect("outcomes lock")
            .iter()
            .map(|o| match o {
                LoadOutcome::Fresh => "fresh",
                LoadOutcome::Cached => "cached",
                LoadOutcome::Incremental => "incremental",
            })
            .collect()
    }

    fn layout(&self) -> Result<Layout> {
        if self.single_file {
            discover::single_file_layout(&self.root, self.kind)
        } else {
            discover::dir_layout(&self.root)
        }
    }

    fn state(&self) -> Result<Arc<State>> {
        if let Some(s) = self.state.read().expect("state lock").as_ref() {
            return Ok(s.clone());
        }
        self.reload(false)
    }

    /// (Re)builds state; with `refresh`, rediscovers the layout and reloads changed units.
    fn reload(&self, refresh: bool) -> Result<Arc<State>> {
        let _guard = self.load_lock.lock().expect("load lock");
        let prev = self.state.read().expect("state lock").clone();
        if let (Some(p), false) = (&prev, refresh) {
            return Ok(p.clone());
        }
        let initial = if refresh { None } else { self.initial.lock().expect("initial lock").take() };
        let layout = Arc::new(match initial {
            Some(l) => l,
            None => self.layout()?,
        });
        let loaded: Vec<Result<(Arc<UnitData>, LoadOutcome)>> = layout
            .units
            .par_iter()
            .map(|spec| {
                if let Some(p) = prev.as_ref() {
                    if let Some(d) = reusable(p, spec) {
                        return Ok((d, LoadOutcome::Cached));
                    }
                }
                self.load_unit(spec).map(|(d, o)| (Arc::new(d), o))
            })
            .collect();
        let mut units = Vec::with_capacity(loaded.len());
        let mut outcomes = Vec::with_capacity(loaded.len());
        for (r, spec) in loaded.into_iter().zip(&layout.units) {
            match r {
                Ok((d, o)) => {
                    units.push(d);
                    outcomes.push(o);
                }
                // One unreadable mailbox in a tree must not hide the rest.
                Err(e) if !self.single_file => {
                    tracing::warn!(path = %spec.abs.display(), error = %e, "skipping unreadable mailbox");
                    units.push(Arc::new(UnitData::empty((0, 0))));
                    outcomes.push(LoadOutcome::Fresh);
                }
                Err(e) => return Err(e),
            }
        }
        *self.outcomes.lock().expect("outcomes lock") = outcomes;
        let state = Arc::new(build_state(layout, units, self.single_file));
        *self.state.write().expect("state lock") = Some(state.clone());
        Ok(state)
    }

    fn load_unit(&self, spec: &UnitSpec) -> Result<(UnitData, LoadOutcome)> {
        let cache = self.cache_dir.as_ref().map(|d| store::cache_path(d, &spec.abs, spec.kind));
        match spec.kind {
            UnitKind::Mbox => store::load_mbox(&spec.abs, cache.as_deref()),
            // A single eml/emlx file is cheaper to parse than to cache.
            k if self.single_file => store::load_files(&spec.abs, &spec.files, k, None),
            k => store::load_files(&spec.abs, &spec.files, k, cache.as_deref()),
        }
    }

    fn unit_folder_of(&self, st: &State, unit: usize, row: usize) -> FolderId {
        let spec = &st.layout.units[unit];
        let deleted = st.units[unit].rows[row].flags & F_DELETED != 0;
        FolderId(if deleted { deleted_id(&spec.folder) } else { spec.folder.clone() })
    }

    fn locator_of(&self, st: &State, unit: usize, row: usize) -> Locator {
        let spec = &st.layout.units[unit];
        let d = &st.units[unit];
        let r = d.rows[row];
        match spec.kind {
            UnitKind::Mbox => Locator::Mbox { file: spec.rel.clone(), offset: r.off, len: r.len },
            _ => Locator::File {
                path: if self.single_file {
                    String::new()
                } else {
                    rel_join(&spec.rel, hdr::blob_path(d.blob(row)).unwrap_or(""))
                },
            },
        }
    }

    fn full_header(&self, st: &State, unit: usize, row: usize) -> Header {
        let mut h = st.units[unit].header(row);
        h.locator = Some(self.locator_of(st, unit, row));
        h.folder = Some(self.unit_folder_of(st, unit, row));
        h
    }

    /// Finds `(unit, row)` for a top-level locator.
    fn find_row(&self, st: &State, loc: &Locator) -> Option<(usize, usize)> {
        match loc {
            Locator::Mbox { file, offset, len } => {
                let u = *st.mbox_units.get(file)?;
                let r = st.units[u].find_offset(*offset)?;
                (st.units[u].rows[r].len == *len).then_some((u, r))
            }
            Locator::File { path } => {
                if self.single_file {
                    return (!st.units[0].rows.is_empty()).then_some((0, 0));
                }
                let mut best: Option<(usize, &str)> = None;
                for (u, spec) in st.layout.units.iter().enumerate() {
                    if spec.kind == UnitKind::Mbox {
                        continue;
                    }
                    let rest = if spec.rel.is_empty() {
                        Some(path.as_str())
                    } else {
                        path.strip_prefix(spec.rel.as_str()).and_then(|r| r.strip_prefix('/'))
                    };
                    if let Some(rest) = rest {
                        if best.is_none_or(|(b, _)| st.layout.units[b].rel.len() < spec.rel.len()) {
                            best = Some((u, rest));
                        }
                    }
                }
                let (u, rest) = best?;
                let d = &st.units[u];
                // Rows of file units are sorted by relative path.
                let (mut lo, mut hi) = (0usize, d.rows.len());
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    match hdr::blob_path(d.blob(mid)).unwrap_or("").cmp(rest) {
                        std::cmp::Ordering::Less => lo = mid + 1,
                        std::cmp::Ordering::Greater => hi = mid,
                        std::cmp::Ordering::Equal => return Some((u, mid)),
                    }
                }
                None
            }
            Locator::Embedded { .. } | Locator::Pst { .. } | Locator::Part { .. } => None,
        }
    }

    fn file_path(&self, path: &str) -> Result<PathBuf> {
        if self.single_file {
            if !path.is_empty() {
                return Err(Error::NotFound(format!("no file {path:?} in single-file source")));
            }
            Ok(self.root.clone())
        } else {
            safe_join(&self.root, path)
        }
    }

    /// Native RFC 5322 bytes of any locator this source understands.
    fn resolve_bytes(&self, loc: &Locator) -> Result<Vec<u8>> {
        match loc {
            Locator::Mbox { file, offset, len } => {
                let st = self.state()?;
                let (u, r) = self
                    .find_row(&st, loc)
                    .ok_or_else(|| Error::NotFound(format!("mbox message {file:?}@{offset}+{len}")))?;
                store::read_mbox_message(&st.layout.units[u].abs, &st.units[u].rows[r])
            }
            Locator::File { path } => {
                let p = self.file_path(path)?;
                let mut data = std::fs::read(&p)?;
                if util::ext_lower(&p) == "emlx" {
                    let (s, e, _) =
                        emlx::split(&data).ok_or_else(|| Error::Format("emlx: missing byte-count line".into()))?;
                    data.truncate(e);
                    data.drain(..s);
                }
                Ok(data)
            }
            Locator::Embedded { parent, attachment } => {
                let pb = self.resolve_bytes(parent)?;
                let bytes = mime::embedded_message_bytes(&pb, *attachment)?;
                if bytes.is_empty() {
                    if let Some(ext) = self.partial_attachment(parent, &pb, *attachment)? {
                        return Ok(ext);
                    }
                }
                Ok(bytes)
            }
            Locator::Pst { .. } | Locator::Part { .. } => {
                Err(Error::Unsupported("PST locator given to an mbox source".into()))
            }
        }
    }

    /// For Apple Mail `.partial.emlx`: attachment bytes stored in `../Attachments/<id>/<section>/`.
    fn partial_attachment(&self, loc: &Locator, msg: &[u8], index: u32) -> Result<Option<Vec<u8>>> {
        let Some(dir) = self.partial_dir(loc)? else { return Ok(None) };
        let sections = mime::attachment_sections(msg)?;
        let Some(sec) = sections.get(index as usize) else { return Ok(None) };
        Ok(first_file(&dir.join(sec)).map(std::fs::read).transpose()?)
    }

    fn partial_dir(&self, loc: &Locator) -> Result<Option<PathBuf>> {
        let Locator::File { path } = loc else { return Ok(None) };
        let p = self.file_path(path)?;
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { return Ok(None) };
        let lower = name.to_ascii_lowercase();
        let Some(id) = lower.strip_suffix(".partial.emlx") else { return Ok(None) };
        let id = &name[..id.len()];
        Ok(p.parent().and_then(Path::parent).map(|data| data.join("Attachments").join(id)))
    }
}

fn first_file(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> =
        std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
    files.sort();
    files.into_iter().next()
}

/// Previous unit data that is still valid for `spec` (unchanged file / file list).
fn reusable(prev: &State, spec: &UnitSpec) -> Option<Arc<UnitData>> {
    let (i, old) = prev
        .layout
        .units
        .iter()
        .enumerate()
        .find(|(_, u)| u.kind == spec.kind && u.rel == spec.rel && u.abs == spec.abs)?;
    let data = &prev.units[i];
    let same = match spec.kind {
        UnitKind::Mbox => data.stamp == spec.stamp && data.stamp != (0, 0),
        _ => {
            old.files.len() == spec.files.len()
                && old
                    .files
                    .iter()
                    .zip(&spec.files)
                    .all(|(a, b)| a.rel == b.rel && a.size == b.size && a.mtime == b.mtime)
        }
    };
    same.then(|| data.clone())
}

fn build_state(layout: Arc<Layout>, units: Vec<Arc<UnitData>>, single_file: bool) -> State {
    let mut folders = Vec::new();
    let mut targets = HashMap::new();
    let mut mbox_units = HashMap::new();
    for (i, u) in layout.units.iter().enumerate() {
        if u.kind == UnitKind::Mbox {
            mbox_units.insert(u.rel.clone(), i);
        }
    }
    for f in &layout.folders {
        let unit = f.unit.map(|u| (u, &units[u]));
        let gmail = unit.is_some_and(|(_, d)| !d.labels.is_empty());
        let name = if single_file && gmail { "All Mail".to_string() } else { f.name.clone() };
        let count = unit.map_or_else(Default::default, |(_, d)| d.count(Filter::Visible));
        folders.push(Folder {
            id: FolderId(f.id.clone()),
            parent: f.parent.clone().map(FolderId),
            name,
            kind: FolderKind::Mail,
            message_count: Some(count.total),
            unread_count: Some(count.unread),
        });
        let Some((ui, d)) = unit else {
            targets.insert(f.id.clone(), Target::Container);
            continue;
        };
        targets.insert(f.id.clone(), Target::View(ui, Filter::Visible));
        // Gmail label folders (nested on '/').
        let mut order: Vec<(usize, &String)> = d.labels.iter().enumerate().collect();
        order.sort_by(|a, b| a.1.cmp(b.1));
        for (li, label) in order {
            let mut parent = f.id.clone();
            let mut prefix = String::new();
            let parts: Vec<&str> = label.split('/').collect();
            for (k, part) in parts.iter().enumerate() {
                prefix = if prefix.is_empty() { part.to_string() } else { format!("{prefix}/{part}") };
                let id = label_id(&f.id, &prefix);
                let last = k + 1 == parts.len();
                if last {
                    let c = d.count(Filter::Label(li as u32));
                    if let Some(existing) = folders.iter_mut().find(|x| x.id.0 == id) {
                        existing.message_count = Some(c.total);
                        existing.unread_count = Some(c.unread);
                    } else {
                        folders.push(Folder {
                            id: FolderId(id.clone()),
                            parent: Some(FolderId(parent.clone())),
                            name: part.to_string(),
                            kind: FolderKind::Label,
                            message_count: Some(c.total),
                            unread_count: Some(c.unread),
                        });
                    }
                    targets.insert(id.clone(), Target::View(ui, Filter::Label(li as u32)));
                } else if !targets.contains_key(&id) {
                    folders.push(Folder {
                        id: FolderId(id.clone()),
                        parent: Some(FolderId(parent.clone())),
                        name: part.to_string(),
                        kind: FolderKind::Label,
                        message_count: Some(0),
                        unread_count: Some(0),
                    });
                    targets.insert(id.clone(), Target::Container);
                }
                parent = id;
            }
        }
        let del = d.count(Filter::Deleted);
        if del.total > 0 {
            let id = deleted_id(&f.id);
            folders.push(Folder {
                id: FolderId(id.clone()),
                parent: Some(FolderId(f.id.clone())),
                name: "Deleted".into(),
                kind: FolderKind::Deleted,
                message_count: Some(del.total),
                unread_count: Some(del.unread),
            });
            targets.insert(id, Target::View(ui, Filter::Deleted));
        }
    }
    State { layout, units, folders, targets, mbox_units }
}

impl MailSource for MboxSource {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn display_name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.to_string_lossy().into_owned())
    }

    fn fingerprint(&self) -> Result<Fingerprint> {
        if self.single_file {
            let f = std::fs::File::open(&self.root)?;
            let meta = f.metadata()?;
            return Ok(Fingerprint {
                path: self.root.clone(),
                size: meta.len(),
                mtime: mtime_ms(&meta),
                head_hash: util::head_hash(&f, meta.len())?,
            });
        }
        let layout = self.layout()?;
        let mut size = 0u64;
        let mut mtime = 0i64;
        let mut listing = Vec::new();
        for u in &layout.units {
            if u.kind == UnitKind::Mbox {
                size += u.stamp.0;
                mtime = mtime.max(u.stamp.1);
                listing.extend_from_slice(format!("{}|{}|{}\n", u.rel, u.stamp.0, u.stamp.1).as_bytes());
            } else {
                for f in &u.files {
                    size += f.size;
                    mtime = mtime.max(f.mtime);
                    listing.extend_from_slice(format!("{}/{}|{}|{}\n", u.rel, f.rel, f.size, f.mtime).as_bytes());
                }
            }
        }
        Ok(Fingerprint { path: self.root.clone(), size, mtime, head_hash: hash(&listing) })
    }

    fn folders(&self) -> Result<Vec<Folder>> {
        Ok(self.state()?.folders.clone())
    }

    fn count(&self, folder: &FolderId) -> Result<u64> {
        let st = self.state()?;
        match st.targets.get(&folder.0) {
            Some(Target::Container) => Ok(0),
            Some(Target::View(u, f)) => Ok(st.units[*u].count(*f).total),
            None => Err(Error::NotFound(format!("folder {:?}", folder.0))),
        }
    }

    fn list(&self, folder: &FolderId, range: Range<u64>, sort: Sort) -> Result<Vec<Header>> {
        let st = self.state()?;
        let (u, f) = match st.targets.get(&folder.0) {
            Some(Target::Container) => return Ok(Vec::new()),
            Some(Target::View(u, f)) => (*u, *f),
            None => return Err(Error::NotFound(format!("folder {:?}", folder.0))),
        };
        let view = st.units[u].view(f, sort.key);
        let n = view.len() as u64;
        let (start, end) = (range.start.min(n), range.end.min(n));
        Ok((start..end)
            .map(|i| {
                let pos = if sort.descending { n - 1 - i } else { i } as usize;
                let row = view[pos] as usize;
                let mut h = self.full_header(&st, u, row);
                h.folder = Some(folder.clone());
                h
            })
            .collect())
    }

    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_> {
        let st = match self.reload(true) {
            Ok(s) => s,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        let n = st.units.len();
        Box::new((0..n).flat_map(move |u| {
            let st = st.clone();
            let rows = st.units[u].rows.len();
            (0..rows).map(move |r| Ok(self.full_header(&st, u, r)))
        }))
    }

    fn message(&self, loc: &Locator) -> Result<Message> {
        let bytes = self.resolve_bytes(loc)?;
        let mut msg = mime::message_from_mime(&bytes, Some(loc.clone()), None)?;
        if matches!(loc, Locator::Embedded { .. }) {
            return Ok(msg);
        }
        let st = self.state()?;
        if let Some((u, r)) = self.find_row(&st, loc) {
            let row = st.units[u].rows[r];
            // The list row carries the mailbox's status flags (X-Mozilla-Status, emlx plist, ...),
            // which the message bytes alone may not.
            let flags = hdr::flags_to_model(row.flags);
            msg.header.flags.read = flags.read;
            msg.header.flags.flagged = flags.flagged;
            msg.header.flags.deleted = flags.deleted;
            msg.header.flags.draft = flags.draft;
            msg.header.date = msg.header.date.or((row.date != i64::MIN).then_some(row.date));
            msg.header.size = row.len;
            msg.header.folder = Some(self.unit_folder_of(&st, u, r));
            if row.flags & F_PARTIAL != 0 {
                self.fill_partial_sizes(loc, &bytes, &mut msg)?;
            }
        }
        Ok(msg)
    }

    fn attachment(&self, loc: &Locator, index: u32) -> Result<Vec<u8>> {
        let bytes = self.resolve_bytes(loc)?;
        let data = mime::attachment_from_mime(&bytes, index)?;
        if data.is_empty() {
            if let Some(ext) = self.partial_attachment(loc, &bytes, index)? {
                return Ok(ext);
            }
        }
        Ok(data)
    }

    fn raw_mime(&self, loc: &Locator) -> Result<Option<Vec<u8>>> {
        self.resolve_bytes(loc).map(Some)
    }

    fn properties(&self, loc: &Locator) -> Result<Vec<PropertyView>> {
        mime::properties_from_mime(&self.resolve_bytes(loc)?)
    }
}

impl MboxSource {
    fn fill_partial_sizes(&self, loc: &Locator, bytes: &[u8], msg: &mut Message) -> Result<()> {
        let Some(dir) = self.partial_dir(loc)? else { return Ok(()) };
        let sections = mime::attachment_sections(bytes)?;
        for a in msg.attachments.iter_mut().filter(|a| a.size == 0) {
            if let Some(f) = sections.get(a.index as usize).and_then(|s| first_file(&dir.join(s))) {
                if let Ok(m) = std::fs::metadata(&f) {
                    a.size = m.len();
                }
            }
        }
        Ok(())
    }
}
