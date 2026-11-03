//! `stv-library`: the single API the STViewer CLI and desktop app use.
//!
//! A [`Library`] owns a persisted registry of mail sources (`sources.json` in the data
//! directory), lazily opens them through the source crates (`stv-pst`, `stv-msg`,
//! `stv-mbox`), keeps a full-text index (`stv-index`, in `data_dir/index`) fresh on a
//! background worker thread, and adds cross-source features: JWZ threading,
//! duplicate detection, cross-format embedded messages, and export.

mod cards;
mod conversation;
pub mod dates;
mod dirlock;
mod discover;
mod embedded;
mod export;
mod fsname;
mod htmlsafe;
mod jsonfile;
mod mime;
mod multipart;
mod registry;
mod tags;
pub mod text;

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

pub use mail_model;
use mail_model::{
    AttachmentPage, AttachmentQuery, AttachmentRow, Error, Folder, FolderId, Header, IndexProgress, Locator,
    MailSource, Message, PropertyView, Result, SearchOptions, SearchResults, Sort, SourceId, SourceKind,
};
use serde::{Deserialize, Serialize};
use stv_index::MailIndex;

pub use conversation::ConversationItem;
pub use discover::{open_source, sniff};
pub use fsname::sanitize_filename;
pub use tags::{Tag, TagTarget, TaggedItem};

use registry::Registry;

/// One registered source as shown to the user.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Registry id; never reused after the source is removed.
    pub id: SourceId,
    pub kind: SourceKind,
    /// Absolute path of the file or directory (the first part of a split archive).
    pub path: PathBuf,
    pub display_name: String,
    /// Background indexing state of this source.
    pub index: IndexProgress,
}

/// Result of [`Library::add_paths`]: the sources registered (or already present), and
/// the paths that could not be added.
#[derive(Debug, Default)]
pub struct AddOutcome {
    pub added: Vec<SourceInfo>,
    pub errors: Vec<(PathBuf, Error)>,
}

/// Output format of [`Library::export`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// One `.eml` per message (native MIME bytes, or MIME synthesised from MAPI).
    Eml,
    /// One mboxrd file (`export.mbox`) with all messages.
    Mbox,
    /// One self-contained `.html` per message (inline images as `data:` URIs).
    Html,
    /// One `.txt` per message.
    Text,
    /// One `export.csv` with a row per message.
    Csv,
    /// Every attachment as its own file (embedded messages as `.eml`).
    Attachments,
    /// One `export.vcf` with a vCard 3.0 per contact; other items are skipped. No file when
    /// nothing is written.
    Vcf,
    /// One `export.ics` (iCalendar) with a VEVENT per appointment / meeting message and a
    /// VTODO per task; other items and appointments with no time to place them at are
    /// skipped, repeats of a UID already written (a meeting request, its replies and its
    /// calendar entry) are counted as duplicates. No file when nothing is written.
    Ics,
}

/// Result of [`Library::export`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExportReport {
    /// Files written (`Mbox`/`Csv`/`Vcf`/`Ics`: items written into the one file).
    pub written: u64,
    /// Messages (or attachments, as `attachment N: ...`) that could not be read.
    pub failed: Vec<(Locator, String)>,
    /// Items left out because the format does not apply to them (`Vcf`: not a contact;
    /// `Ics`: see [`ExportFormat::Ics`]).
    #[serde(default)]
    pub skipped: u64,
    /// `Ics`: items left out because a component with their UID was already written.
    #[serde(default)]
    pub duplicates: u64,
}

/// Default per-user data directory: `%LOCALAPPDATA%\STViewer` on Windows,
/// `~/.local/share/STViewer` on Linux, `~/Library/Application Support/STViewer` on macOS.
pub fn default_data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("STViewer")
}

/// macOS privacy protection (TCC) refuses Apple Mail data and other protected folders
/// with `EPERM` ("Operation not permitted") rather than the `EACCES` of file permissions,
/// until the user grants Full Disk Access.
#[cfg(target_os = "macos")]
fn explain_denied(e: Error) -> Error {
    match e {
        Error::Io(io) if io.raw_os_error() == Some(1) => Error::Other(format!(
            "{io}. macOS protects this location: allow STViewer (or, for `stv`, your terminal) in \
             System Settings → Privacy & Security → Full Disk Access, then restart it"
        )),
        e => e,
    }
}

#[cfg(not(target_os = "macos"))]
fn explain_denied(e: Error) -> Error {
    e
}

/// Source registry + index + background indexer. Cheap to clone; all clones share state.
/// The background worker stops (cancelling any running sync) when the last clone drops.
#[derive(Clone)]
pub struct Library {
    // Field order matters: the guard stops and joins the worker before `inner` drops,
    // so the index is closed by the time the last `Library` is gone.
    _guard: Arc<WorkerGuard>,
    inner: Arc<Inner>,
}

type Slot = Arc<Mutex<Option<Arc<dyn MailSource>>>>;

pub(crate) struct Inner {
    data_dir: PathBuf,
    index: MailIndex,
    registry: Mutex<Registry>,
    /// Per-source open slot; the slot mutex serialises the (possibly slow, e.g. first
    /// mbox scan) open of one source without blocking access to the others.
    cache: Mutex<HashMap<SourceId, Slot>>,
    progress: Mutex<HashMap<SourceId, IndexProgress>>,
    jobs: Mutex<Jobs>,
    jobs_cv: Condvar,
    /// Serialises `add_path` so concurrent adds of one path cannot register it twice.
    add_lock: Mutex<()>,
    tags: Mutex<tags::TagStore>,
    /// mbox/eml opener whose header caches live in `data_dir/cache/mbox`, so a library is
    /// self-contained (deleting the data directory removes everything it wrote).
    mbox: stv_mbox::MboxOpener,
    /// Last field: released only after the index and stores above are closed.
    _lock: dirlock::DirLock,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Job {
    Sync(SourceId),
    /// Drop the source's documents and cached reader, then sync from scratch.
    Reindex(SourceId),
}

impl Job {
    fn id(self) -> SourceId {
        match self {
            Job::Sync(id) | Job::Reindex(id) => id,
        }
    }
}

#[derive(Default)]
struct Jobs {
    queue: VecDeque<Job>,
    current: Option<(SourceId, Arc<AtomicBool>)>,
    shutdown: bool,
}

struct WorkerGuard {
    inner: Arc<Inner>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        {
            let mut jobs = lock(&self.inner.jobs);
            jobs.shutdown = true;
            if let Some((_, cancel)) = &jobs.current {
                cancel.store(true, Ordering::SeqCst);
            }
        }
        self.inner.jobs_cv.notify_all();
        if let Some(h) = lock(&self.handle).take() {
            let _ = h.join();
        }
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Library {
    /// Open (or create) a library in `data_dir`. Registered sources are not opened
    /// until first use; all of them are queued for an incremental index sync, which
    /// is a no-op for sources whose fingerprint has not changed.
    ///
    /// A library is used by one process at a time: while a `Library` (any clone) is
    /// alive, opening the same directory again fails with [`Error::InUse`].
    pub fn open(data_dir: &Path) -> Result<Library> {
        std::fs::create_dir_all(data_dir)?;
        let dir_lock = dirlock::DirLock::acquire(data_dir)?;
        let registry = Registry::load(&data_dir.join(registry::FILE_NAME))?;
        let tag_store = tags::TagStore::load(&data_dir.join(tags::FILE_NAME))?;
        let index_dir = data_dir.join("index");
        std::fs::create_dir_all(&index_dir)?;
        let index = MailIndex::open(&index_dir)?;

        let inner = Arc::new(Inner {
            data_dir: data_dir.to_path_buf(),
            index,
            registry: Mutex::new(registry),
            cache: Mutex::new(HashMap::new()),
            progress: Mutex::new(HashMap::new()),
            jobs: Mutex::new(Jobs::default()),
            jobs_cv: Condvar::new(),
            add_lock: Mutex::new(()),
            tags: Mutex::new(tag_store),
            mbox: stv_mbox::MboxOpener::with_options(stv_mbox::OpenOptions {
                cache_dir: Some(data_dir.join("cache").join("mbox")),
                no_cache: false,
            }),
            _lock: dir_lock,
        });
        let worker_inner = Arc::clone(&inner);
        let handle =
            std::thread::Builder::new().name("stv-indexer".into()).spawn(move || worker_inner.worker_loop())?;
        let lib = Library {
            _guard: Arc::new(WorkerGuard { inner: Arc::clone(&inner), handle: Mutex::new(Some(handle)) }),
            inner,
        };
        // Split-archive parts registered as separate sources (added one by one, or by an
        // older version) become one source. Failures leave the separate sources in place.
        let mut regrouped = AddOutcome::default();
        lib.register(Vec::new(), &mut regrouped);
        for (p, e) in &regrouped.errors {
            tracing::warn!("regrouping {}: {e}", p.display());
        }
        let regrouped: Vec<SourceId> = regrouped.added.iter().map(|s| s.id).collect();
        let ids: Vec<SourceId> =
            lock(&lib.inner.registry).sources.iter().map(|e| e.id).filter(|id| !regrouped.contains(id)).collect();
        for id in ids {
            // A previously synced source is reported as indexed; the queued sync
            // (usually a no-op) flips it back to running only if it finds new work.
            let indexed = lib.inner.index.doc_count(Some(id)).unwrap_or(0);
            lib.inner.enqueue_with(Job::Sync(id), |p| {
                p.indexed = indexed;
                p.total = (indexed > 0).then_some(indexed);
                p.done = indexed > 0;
            });
        }
        Ok(lib)
    }

    /// The directory holding `sources.json`, `tags.json` and the index.
    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    /// Register a file or directory; see [`Library::add_paths`]. Fails only when
    /// nothing could be registered.
    pub fn add_path(&self, path: &Path) -> Result<Vec<SourceInfo>> {
        let mut outcome = self.add_paths(&[path.to_path_buf()]);
        if outcome.added.is_empty() {
            return Err(outcome
                .errors
                .pop()
                .map_or_else(|| Error::Unsupported(path.display().to_string()), |(_, e)| e));
        }
        Ok(outcome.added)
    }

    /// Register files and/or directories. A directory may yield several sources (each
    /// PST/OST, plus one source per mbox/eml/emlx tree or .msg folder). PST/OST files
    /// numbered as parts of one split archive (`name.001.pst`, `name.002.pst`, ...)
    /// become a single source — across all `paths`, and together with parts registered
    /// earlier as separate sources, which the merged source replaces. Paths that are
    /// already registered return their existing entry. New sources start indexing in
    /// the background immediately.
    pub fn add_paths(&self, paths: &[PathBuf]) -> AddOutcome {
        let _adding = lock(&self.inner.add_lock);
        let mut outcome = AddOutcome::default();
        let mut found: Vec<(PathBuf, SourceKind)> = Vec::new();
        for p in paths {
            let p = match std::path::absolute(p) {
                Ok(p) => p,
                Err(e) => {
                    outcome.errors.push((p.clone(), e.into()));
                    continue;
                }
            };
            match discover::discover(&p) {
                Ok(v) if v.is_empty() => outcome
                    .errors
                    .push((p.clone(), Error::Unsupported(format!("no mail archives found in {}", p.display())))),
                Ok(v) => found.extend(v),
                Err(e) => outcome.errors.push((p, explain_denied(e))),
            }
        }
        self.register(found, &mut outcome);
        outcome
    }

    /// Register discovered sources, grouping split-archive parts with each other and
    /// with already-registered PST/OST files (single sources and parts of split archives).
    fn register(&self, found: Vec<(PathBuf, SourceKind)>, outcome: &mut AddOutcome) {
        let is_new = |p: &Path| found.iter().any(|(f, _)| registry::same_path(f, p));
        let mut candidates = found.clone();
        let registered = lock(&self.inner.registry);
        for e in registered.sources.iter().filter(|e| matches!(e.kind, SourceKind::Pst | SourceKind::Ost)) {
            candidates.extend(e.files().iter().filter(|p| !is_new(p)).map(|p| (p.clone(), e.kind)));
        }
        drop(registered);

        let mut new_ids = Vec::new();
        for item in multipart::group(candidates) {
            let (p, parts, kind, has_new) = match item {
                // A registered file that groups with nothing stays as it is.
                multipart::Found::One(p, _) if !is_new(&p) => continue,
                multipart::Found::One(p, kind) => (p, Vec::new(), kind, true),
                multipart::Found::Parts(parts, kind) => {
                    let has_new = parts.iter().any(|p| is_new(p));
                    (parts[0].clone(), parts, kind, has_new)
                }
            };
            // Bind first: a guard in an `if let` scrutinee would live through the block
            // and deadlock `info()` re-locking the registry.
            let existing = lock(&self.inner.registry).find_path(&p).map(|e| (e.id, e.parts.clone()));
            let replaced: Vec<SourceId> = match existing {
                Some((id, have))
                    if have.len() == parts.len() && have.iter().zip(&parts).all(|(a, b)| registry::same_path(a, b)) =>
                {
                    // Already registered as it is: report it only if the caller asked for it.
                    if has_new {
                        outcome.added.extend(self.inner.info(id));
                    }
                    continue;
                }
                // A split archive replaces its parts registered as single sources (and an
                // older registration of the same archive with fewer parts).
                _ => lock(&self.inner.registry)
                    .sources
                    .iter()
                    .filter(|e| e.files().iter().any(|f| parts.iter().any(|part| registry::same_path(f, part))))
                    .map(|e| e.id)
                    .collect(),
            };
            // Open now: validates the file(s) and gives a display name. The reader is
            // kept, so the indexer and the first listing do not open it again.
            match self.inner.open_entry(&p, &parts, kind) {
                Ok(src) => {
                    for id in replaced {
                        if let Err(e) = self.remove_source(id) {
                            tracing::warn!("replacing source {}: {e}", id.0);
                        }
                    }
                    let name = src.display_name();
                    let entry = lock(&self.inner.registry).insert(src.kind(), p, parts, name);
                    lock(&self.inner.cache).insert(entry.id, Arc::new(Mutex::new(Some(src))));
                    new_ids.push(entry.id);
                }
                Err(e) => {
                    tracing::warn!("cannot open {}: {e}", p.display());
                    outcome.errors.push((p, e));
                }
            }
        }
        if !new_ids.is_empty() {
            if let Err(e) = self.inner.save_registry() {
                outcome.errors.push((self.inner.data_dir.join(registry::FILE_NAME), e));
            }
        }
        for &id in &new_ids {
            self.inner.enqueue(Job::Sync(id));
        }
        outcome.added.extend(new_ids.iter().filter_map(|&id| self.inner.info(id)));
    }

    /// Unregister a source and delete its index documents. A running sync of that
    /// source is cancelled first.
    pub fn remove_source(&self, id: SourceId) -> Result<()> {
        if lock(&self.inner.registry).remove(id).is_none() {
            return Err(Error::NotFound(format!("source {}", id.0)));
        }
        self.inner.save_registry()?;
        {
            let mut jobs = lock(&self.inner.jobs);
            jobs.queue.retain(|j| j.id() != id);
            if let Some((cur, cancel)) = &jobs.current {
                if *cur == id {
                    cancel.store(true, Ordering::SeqCst);
                }
            }
            while matches!(&jobs.current, Some((cur, _)) if *cur == id) {
                jobs = self.inner.jobs_cv.wait(jobs).unwrap_or_else(PoisonError::into_inner);
            }
        }
        lock(&self.inner.cache).remove(&id);
        lock(&self.inner.progress).remove(&id);
        lock(&self.inner.tags).drop_source(id)?;
        self.inner.index.remove_source(id)
    }

    /// Every registered source, ordered by id.
    pub fn sources(&self) -> Vec<SourceInfo> {
        let ids: Vec<SourceId> = lock(&self.inner.registry).sources.iter().map(|e| e.id).collect();
        ids.into_iter().filter_map(|id| self.inner.info(id)).collect()
    }

    /// Folder tree of a source, parents before children.
    pub fn folders(&self, id: SourceId) -> Result<Vec<Folder>> {
        self.inner.source(id)?.folders()
    }

    /// Number of messages in `folder`.
    pub fn count(&self, id: SourceId, folder: &FolderId) -> Result<u64> {
        self.inner.source(id)?.count(folder)
    }

    /// Rows `range` of `folder` under `sort`; a range past the end yields fewer (or no) rows.
    pub fn list(&self, id: SourceId, folder: &FolderId, range: Range<u64>, sort: Sort) -> Result<Vec<Header>> {
        let range = range.start.min(range.end)..range.end;
        self.inner.source(id)?.list(folder, range, sort)
    }

    /// Full message. `Locator::Embedded` is resolved by the source when it can, and
    /// otherwise by decoding the parent attachment as TNEF / .msg / MIME.
    pub fn message(&self, id: SourceId, loc: &Locator) -> Result<Message> {
        let src = self.inner.source(id)?;
        let mut m = embedded::message(&*src, loc)?;
        if m.header.locator.is_none() {
            m.header.locator = Some(loc.clone());
        }
        Ok(m)
    }

    /// Raw bytes of attachment `index` (as numbered by `message().attachments[..].index`).
    pub fn attachment(&self, id: SourceId, loc: &Locator, index: u32) -> Result<Vec<u8>> {
        let src = self.inner.source(id)?;
        embedded::attachment(&*src, loc, index)
    }

    /// Raw properties of a message for the inspector (MAPI properties or MIME header
    /// fields), with embedded messages resolved like [`Library::message`].
    pub fn properties(&self, id: SourceId, loc: &Locator) -> Result<Vec<PropertyView>> {
        let src = self.inner.source(id)?;
        embedded::properties(&*src, loc)
    }

    /// RFC 5322 bytes of a message: the original MIME when the source has it, else
    /// MIME synthesised from the decoded message (MAPI sources).
    pub fn mime(&self, id: SourceId, loc: &Locator) -> Result<Vec<u8>> {
        let src = self.inner.source(id)?;
        let msg = embedded::message(&*src, loc)?;
        mime::message_mime(&*src, loc, &msg, 0)
    }

    /// The conversation containing `loc` as a dated timeline: one entry per email (copies
    /// in several folders merged), each with a preview of its own text.
    ///
    /// `scope` limits the conversation to those sources: messages elsewhere are neither
    /// returned nor followed to find further thread members, so nothing about them
    /// (folders, previews, copies) shows up. `None` searches every source.
    pub fn conversation(
        &self,
        id: SourceId,
        loc: &Locator,
        scope: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<ConversationItem>> {
        if scope.is_some_and(|s| !s.contains(&id)) {
            return Err(Error::NotFound(format!("source {}", id.0)));
        }
        let mut target = self.message(id, loc)?.header;
        target.locator = Some(loc.clone());
        let items = self.inner.collect_thread(id, target, scope)?;
        let keys: Vec<(SourceId, Locator)> =
            items.iter().filter_map(|(s, h)| h.locator.clone().map(|l| (*s, l))).collect();
        let found = self.inner.index.previews(&keys)?;
        // Align previews with items (items without a locator have none).
        let mut found = found.into_iter();
        let previews: Vec<Option<String>> =
            items.iter().map(|(_, h)| if h.locator.is_some() { found.next().flatten() } else { None }).collect();
        // Folder paths without the mailbox's own root name (already implied by the view).
        let mut names: HashMap<SourceId, HashMap<FolderId, String>> = HashMap::new();
        for s in items.iter().map(|(s, _)| *s) {
            names.entry(s).or_insert_with(|| {
                let folders = self.folders(s).unwrap_or_default();
                let mut paths = export::folder_paths(&folders);
                if let Some(root) = folders.iter().find(|f| f.parent.is_none()) {
                    let prefix = format!("{}/", root.name);
                    for p in paths.values_mut() {
                        if let Some(rest) = p.strip_prefix(&prefix) {
                            *p = rest.to_string();
                        }
                    }
                }
                paths
            });
        }
        let name =
            |s: SourceId, f: &FolderId| names.get(&s).and_then(|m| m.get(f)).cloned().unwrap_or_else(|| f.0.clone());
        Ok(conversation::build(items, previews, (id, loc), &name))
    }

    /// Full-text search over every indexed source (query syntax: see `stv-index`).
    pub fn search(&self, query: &str, opts: &SearchOptions) -> Result<SearchResults> {
        self.inner.index.search(query, opts)
    }

    /// One page of the attachment listing (rows are recorded while indexing, so a source
    /// lists its attachments as far as it has been indexed).
    pub fn attachments(&self, query: &AttachmentQuery) -> Result<AttachmentPage> {
        self.inner.index.attachments(query)
    }

    /// All tags with their item counts.
    pub fn tags(&self) -> Vec<Tag> {
        lock(&self.inner.tags).tags()
    }

    /// Creates an empty tag. `color` must be `#rgb` / `#rrggbb`; `None` picks the next
    /// palette color. Names are unique case-insensitively.
    pub fn create_tag(&self, name: &str, color: Option<&str>) -> Result<Tag> {
        lock(&self.inner.tags).create(name, color)
    }

    /// Renames a tag on every item; fails if `new` names another existing tag.
    pub fn rename_tag(&self, old: &str, new: &str) -> Result<()> {
        lock(&self.inner.tags).rename(old, new)
    }

    /// Deletes the tag and removes it from every item.
    pub fn delete_tag(&self, name: &str) -> Result<()> {
        lock(&self.inner.tags).delete(name)
    }

    /// Tag (`on`) or untag `targets`. Tagging creates the tag if needed and snapshots each
    /// newly tagged item's header / attachment row (read in parallel, without holding the
    /// tag lock). Targets that cannot be read are reported as an error after the rest
    /// are tagged.
    pub fn set_tag(&self, targets: &[TagTarget], tag: &str, on: bool) -> Result<()> {
        if !on {
            return lock(&self.inner.tags).remove(targets, tag);
        }
        let unknown: Vec<TagTarget> = lock(&self.inner.tags).unknown(targets).into_iter().cloned().collect();
        let results: Vec<Result<(TagTarget, Header, Option<AttachmentRow>)>> = std::thread::scope(|s| {
            let chunk = unknown.len().div_ceil(8).max(1);
            let handles: Vec<_> = unknown
                .chunks(chunk)
                .map(|part| s.spawn(move || part.iter().map(|t| self.tag_snapshot(t)).collect::<Vec<_>>()))
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_else(|_| vec![Err(Error::Other("tag snapshot panicked".into()))]))
                .collect()
        });
        let mut snapshots = Vec::with_capacity(results.len());
        let mut first_err = None;
        for r in results {
            match r {
                Ok(s) => snapshots.push(s),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        lock(&self.inner.tags).apply(targets, tag, snapshots, now)?;
        first_err.map_or(Ok(()), Err)
    }

    fn tag_snapshot(&self, t: &TagTarget) -> Result<(TagTarget, Header, Option<AttachmentRow>)> {
        let msg = self.message(t.source, &t.message)?;
        let mut header = msg.header;
        header.locator = Some(t.message.clone());
        let attachment = match t.attachment {
            None => None,
            Some(idx) => {
                let meta = msg
                    .attachments
                    .iter()
                    .find(|a| a.index == idx)
                    .ok_or_else(|| Error::NotFound(format!("attachment {idx}")))?;
                Some(
                    AttachmentRow::new(t.source, &header, meta, None)
                        .ok_or_else(|| Error::Unsupported("link attachments cannot be tagged".into()))?,
                )
            }
        };
        Ok((t.clone(), header, attachment))
    }

    /// Current tags of each target, in order.
    pub fn tags_of(&self, targets: &[TagTarget]) -> Vec<Vec<String>> {
        lock(&self.inner.tags).tags_of(targets)
    }

    /// Tagged items, newest-tagged first; filter by source and/or tag.
    pub fn tagged(&self, source: Option<SourceId>, tag: Option<&str>) -> Vec<TaggedItem> {
        lock(&self.inner.tags).tagged(source, tag)
    }

    /// Number of tagged items in `source`.
    pub fn tagged_count(&self, source: SourceId) -> u64 {
        lock(&self.inner.tags).count(source)
    }

    /// Progress of every registered source, ordered by id.
    pub fn index_progress(&self) -> Vec<IndexProgress> {
        self.sources().into_iter().map(|s| s.index).collect()
    }

    /// True while any sync is queued or running.
    pub fn is_indexing(&self) -> bool {
        let jobs = lock(&self.inner.jobs);
        !jobs.queue.is_empty() || jobs.current.is_some()
    }

    /// Re-open the source and rebuild its index documents from scratch, in the background.
    pub fn reindex(&self, id: SourceId) -> Result<()> {
        if lock(&self.inner.registry).get(id).is_none() {
            return Err(Error::NotFound(format!("source {}", id.0)));
        }
        self.inner.enqueue(Job::Reindex(id));
        Ok(())
    }

    /// Export `items` into the directory `dest` (created if missing). Per-message read
    /// failures are collected in the report; write failures abort the export.
    pub fn export(&self, items: &[(SourceId, Locator)], format: ExportFormat, dest: &Path) -> Result<ExportReport> {
        export::export(self, items, format, dest)
    }

    pub(crate) fn source(&self, id: SourceId) -> Result<Arc<dyn MailSource>> {
        self.inner.source(id)
    }
}

impl Inner {
    fn save_registry(&self) -> Result<()> {
        lock(&self.registry).save(&self.data_dir.join(registry::FILE_NAME))
    }

    fn info(&self, id: SourceId) -> Option<SourceInfo> {
        let e = lock(&self.registry).get(id)?.clone();
        let index =
            lock(&self.progress).get(&id).cloned().unwrap_or(IndexProgress { source: Some(id), ..Default::default() });
        Some(SourceInfo { id, kind: e.kind, path: e.path, display_name: e.display_name, index })
    }

    /// Open a registered source: a single file/directory, or a split archive's parts.
    fn open_entry(&self, path: &Path, parts: &[PathBuf], kind: SourceKind) -> Result<Arc<dyn MailSource>> {
        Ok(if parts.is_empty() {
            Arc::from(discover::open_kind(path, kind, &self.mbox)?)
        } else {
            Arc::new(multipart::MultiPartSource::open(parts, kind)?)
        })
    }

    fn source(&self, id: SourceId) -> Result<Arc<dyn MailSource>> {
        let (path, parts, kind) = {
            let reg = lock(&self.registry);
            let e = reg.get(id).ok_or_else(|| Error::NotFound(format!("source {}", id.0)))?;
            (e.path.clone(), e.parts.clone(), e.kind)
        };
        let slot = Arc::clone(lock(&self.cache).entry(id).or_default());
        let mut guard = lock(&slot);
        if let Some(src) = guard.as_ref() {
            return Ok(Arc::clone(src));
        }
        let src = self.open_entry(&path, &parts, kind)?;
        *guard = Some(Arc::clone(&src));
        Ok(src)
    }

    fn enqueue(&self, job: Job) {
        self.enqueue_with(job, |_| {});
    }

    /// Queues `job` (unless already queued) and marks its source as pending, then applies
    /// `init`. Progress is set while the queue is locked, so the worker cannot run the job
    /// (and report it done) before this initial state is in place.
    fn enqueue_with(&self, job: Job, init: impl FnOnce(&mut IndexProgress)) {
        {
            let mut jobs = lock(&self.jobs);
            if jobs.queue.contains(&job) {
                return;
            }
            jobs.queue.push_back(job);
            self.set_progress(job.id(), |e| {
                e.done = false;
                e.error = None;
                init(e);
            });
        }
        self.jobs_cv.notify_all();
    }

    fn set_progress(&self, id: SourceId, f: impl FnOnce(&mut IndexProgress)) {
        let mut p = lock(&self.progress);
        let e = p.entry(id).or_insert_with(|| IndexProgress { source: Some(id), ..Default::default() });
        f(e);
        e.source = Some(id);
    }

    fn worker_loop(&self) {
        loop {
            let (job, cancel) = {
                let mut jobs = lock(&self.jobs);
                loop {
                    if jobs.shutdown {
                        return;
                    }
                    if let Some(job) = jobs.queue.pop_front() {
                        let cancel = Arc::new(AtomicBool::new(false));
                        jobs.current = Some((job.id(), Arc::clone(&cancel)));
                        break (job, cancel);
                    }
                    jobs = self.jobs_cv.wait(jobs).unwrap_or_else(PoisonError::into_inner);
                }
            };
            // A reader panicking on a malformed archive must not take the worker down:
            // `current` would never clear, and `remove_source` / `is_indexing` waiters
            // would block forever. Shared state is behind poison-tolerant locks.
            if std::panic::catch_unwind(AssertUnwindSafe(|| self.run_job(job, &cancel))).is_err() {
                tracing::error!("indexing source {} panicked", job.id().0);
                self.set_progress(job.id(), |p| {
                    p.done = true;
                    p.error = Some("indexing failed: internal error (reader panicked)".into());
                });
            }
            lock(&self.jobs).current = None;
            self.jobs_cv.notify_all();
        }
    }

    fn run_job(&self, job: Job, cancel: &AtomicBool) {
        let id = job.id();
        if lock(&self.registry).get(id).is_none() {
            return;
        }
        let result = (|| -> Result<()> {
            if let Job::Reindex(_) = job {
                lock(&self.cache).remove(&id);
                self.index.remove_source(id)?;
            }
            let src = self.source(id)?;
            let on_progress = |p: IndexProgress| {
                self.set_progress(id, |e| {
                    e.indexed = p.indexed;
                    e.total = p.total;
                    e.read_errors = p.read_errors;
                    e.done = false;
                })
            };
            self.index.sync_source(id, &embedded::Resolving(&*src), &on_progress, cancel)
        })();
        match result {
            Ok(()) => {
                let n = self.index.doc_count(Some(id)).ok();
                // `error` is for fatal failures only; unreadable messages are counted in
                // `read_errors` (kept from the final progress report).
                self.set_progress(id, |e| {
                    if let Some(n) = n {
                        e.indexed = n;
                        e.total = Some(n);
                    }
                    e.error = None;
                    e.done = true;
                });
            }
            Err(Error::Cancelled) => self.set_progress(id, |e| e.error = None),
            Err(e) => {
                tracing::warn!("indexing source {} failed: {e}", id.0);
                self.set_progress(id, |p| {
                    p.done = true;
                    p.error = Some(e.to_string());
                });
            }
        }
    }

    /// Target + everything the index relates to it, transitively, capped. With a `scope`,
    /// documents of other sources are skipped and not followed.
    fn collect_thread(
        &self,
        id: SourceId,
        target: Header,
        scope: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<(SourceId, Header)>> {
        const CAP: usize = 500;
        const MAX_ROUNDS: usize = 32;

        let mut seen_docs: HashSet<(SourceId, Locator)> = HashSet::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut seen_convs: HashSet<String> = HashSet::new();
        let mut ids: Vec<String> = Vec::new();
        let mut convs: Vec<String> = Vec::new();

        let mut note = |h: &Header, ids: &mut Vec<String>, convs: &mut Vec<String>| {
            for m in h.message_id.iter().chain(h.in_reply_to.iter()).chain(h.references.iter()) {
                let m = m.trim().trim_start_matches('<').trim_end_matches('>');
                if !m.is_empty() && seen_ids.insert(m.to_owned()) {
                    ids.push(m.to_owned());
                }
            }
            if let Some(c) = h.conversation.as_deref().filter(|c| !c.is_empty()) {
                if seen_convs.insert(c.to_owned()) {
                    convs.push(c.to_owned());
                }
            }
        };

        if let Some(l) = &target.locator {
            seen_docs.insert((id, l.clone()));
        }
        note(&target, &mut ids, &mut convs);
        let mut items = vec![(id, target)];

        for _ in 0..MAX_ROUNDS {
            if (ids.is_empty() && convs.is_empty()) || items.len() >= CAP {
                break;
            }
            let batch_ids = std::mem::take(&mut ids);
            let batch_convs = std::mem::take(&mut convs);
            let mut found = Vec::new();
            if batch_convs.is_empty() {
                found.extend(self.index.related(&batch_ids, None)?);
            } else {
                for (i, c) in batch_convs.iter().enumerate() {
                    let q: &[String] = if i == 0 { &batch_ids } else { &[] };
                    found.extend(self.index.related(q, Some(c))?);
                }
            }
            for (sid, h) in found {
                if scope.is_some_and(|s| !s.contains(&sid)) {
                    continue;
                }
                let Some(l) = h.locator.clone() else { continue };
                if items.len() >= CAP {
                    break;
                }
                if seen_docs.insert((sid, l)) {
                    note(&h, &mut ids, &mut convs);
                    items.push((sid, h));
                }
            }
        }
        Ok(items)
    }
}
