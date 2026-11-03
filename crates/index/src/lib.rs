//! Full-text index over every `MailSource`, on tantivy.
//!
//! One tantivy index (one directory) holds documents from all sources; each document is
//! one message keyed by `(SourceId, Locator)`. Hits carry a stored copy of the `Header` and a
//! short body preview, so search results render without reopening the source.
//!
//! Snippets: bodies are indexed but not stored. A stored `preview` (first 2 KB of body text,
//! whitespace-collapsed) is the snippet source: it keeps the doc store small (bodies would
//! roughly double the index) and avoids reopening a PST/mbox per hit, at the cost of not
//! highlighting matches that occur only deep inside a long body (the subject, then the
//! preview start, is shown instead).

mod attachments;
mod collectors;
mod directory;
mod doc;
pub mod extract;
mod query;
mod schema;
mod util;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Display;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use mail_model::{
    AttachmentPage, AttachmentQuery, Error, FacetCount, Fingerprint, Header, IndexProgress, MailSource, Result,
    SearchHit, SearchOptions, SearchResults, SortKey, SourceId,
};
use serde::{Deserialize, Serialize};
use tantivy::collector::{Collector, Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, ConstScoreQuery, Occur, Query, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, Searcher, TantivyDocument, Term};

use crate::collectors::{FacetCollector, Facets, StrValues};
use crate::schema::Fields;

const META_FILE: &str = "stv-index.json";
/// Total indexing heap shared by tantivy's indexing threads.
const WRITER_HEAP: usize = 256 * 1024 * 1024;
/// Heap for writers that only delete.
const DELETE_HEAP: usize = 32 * 1024 * 1024;
/// Indexing heap of the attachment listing (small rows, no large text).
const ATT_WRITER_HEAP: usize = 64 * 1024 * 1024;

/// One extracted message, travelling from a worker to the writer thread.
struct Built {
    key: String,
    doc: TantivyDocument,
    att_docs: Vec<TantivyDocument>,
    /// `false` when the message could not be read (indexed from its listing header).
    ok: bool,
    /// Replaces an earlier partial document with the same key.
    retry: bool,
}

const COMMIT_EVERY_DOCS: u64 = 20_000;
const COMMIT_EVERY: Duration = Duration::from_secs(5);
const PROGRESS_EVERY: Duration = Duration::from_millis(200);
/// `SearchOptions::limit == 0` means this many hits.
pub const DEFAULT_LIMIT: usize = 100;
const SNIPPET_CHARS: usize = 200;
const TOP_SENDERS: usize = 20;
/// Upper bound on documents returned by `related`.
const RELATED_LIMIT: usize = 10_000;

fn ie(e: impl Display) -> Error {
    Error::Other(format!("index: {e}"))
}

/// Message-loading workers per sync: twice the cores, because reading from network
/// shares and large archives is latency-bound rather than CPU-bound.
/// `STVIEWER_INDEX_THREADS` overrides (1–256).
fn index_threads() -> usize {
    std::env::var("STVIEWER_INDEX_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get() * 2))
        .min(256)
}

#[derive(Default, Serialize, Deserialize)]
struct Meta {
    schema_version: u32,
    /// Fingerprint of each source as of its last *completed* sync.
    sources: BTreeMap<u32, Fingerprint>,
}

/// Full-text index over all registered sources. `Send + Sync`; searches run concurrently
/// with a sync (they see the last periodic commit).
pub struct MailIndex {
    dir: PathBuf,
    index: Index,
    reader: IndexReader,
    fields: Fields,
    /// One row per attachment (`dir/att`), written and deleted with its message.
    att: attachments::AttIndex,
    /// tantivy allows one writer per index: syncs and removals are serialised.
    write_lock: Mutex<()>,
    meta: Mutex<Meta>,
}

impl MailIndex {
    /// Opens (or creates) the index in `dir`. An index written with another schema version,
    /// or one tantivy cannot open, is discarded and rebuilt empty (it is a cache of the sources).
    /// The attachment listing lives in `dir/att` and is discarded together with it.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let att_dir = dir.join(attachments::DIR);
        let (schema, fields) = schema::build();
        let meta_path = dir.join(META_FILE);
        let fresh = || Meta { schema_version: schema::SCHEMA_VERSION, sources: BTreeMap::new() };
        let meta = std::fs::read(&meta_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Meta>(&b).ok())
            .filter(|m| m.schema_version == schema::SCHEMA_VERSION);
        let mut meta = match meta {
            Some(m) => m,
            None => {
                clear_dir(dir)?;
                clear_att_dir(&att_dir)?;
                fresh()
            }
        };
        let open = |schema| -> tantivy::Result<Index> {
            let mmap = MmapDirectory::open(dir).map_err(tantivy::TantivyError::from)?;
            Index::open_or_create(directory::RetryingDirectory(mmap), schema)
        };
        let index = match open(schema.clone()) {
            Ok(i) => i,
            Err(_) => {
                clear_dir(dir)?;
                clear_att_dir(&att_dir)?;
                meta = fresh();
                open(schema).map_err(ie)?
            }
        };
        let att = match attachments::AttIndex::open(&att_dir) {
            Ok(a) => a,
            Err(_) => {
                // An unreadable listing is rebuilt from scratch; the message documents must
                // go too, or incremental sync would skip their messages and never re-list them.
                clear_att_dir(&att_dir)?;
                let mut w: IndexWriter<TantivyDocument> = index.writer(DELETE_HEAP).map_err(ie)?;
                w.delete_all_documents().map_err(ie)?;
                w.commit().map_err(ie)?;
                meta = fresh();
                attachments::AttIndex::open(&att_dir).map_err(ie)?
            }
        };
        schema::register_analyzers(&index);
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into().map_err(ie)?;
        let this = MailIndex {
            dir: dir.to_path_buf(),
            index,
            reader,
            fields,
            att,
            write_lock: Mutex::new(()),
            meta: Mutex::new(meta),
        };
        this.save_meta()?;
        Ok(this)
    }

    fn save_meta(&self) -> Result<()> {
        let bytes = {
            let meta = self.meta.lock().unwrap_or_else(|p| p.into_inner());
            serde_json::to_vec_pretty(&*meta).map_err(ie)?
        };
        let tmp = self.dir.join(format!("{META_FILE}.tmp"));
        std::fs::write(&tmp, bytes)?;
        let dest = self.dir.join(META_FILE);
        directory::retry_transient(|| std::fs::rename(&tmp, &dest))?;
        Ok(())
    }

    fn stored_fingerprint(&self, id: SourceId) -> Option<Fingerprint> {
        self.meta.lock().unwrap_or_else(|p| p.into_inner()).sources.get(&id.0).cloned()
    }

    fn source_term(&self, id: SourceId) -> Term {
        Term::from_field_u64(self.fields.source, u64::from(id.0))
    }

    fn source_query(&self, id: SourceId) -> TermQuery {
        TermQuery::new(self.source_term(id), IndexRecordOption::Basic)
    }

    /// Incremental: if the stored fingerprint equals `src.fingerprint()` this returns
    /// immediately. Otherwise documents are added for locators not yet indexed and deleted
    /// for locators that vanished, then the new fingerprint is stored. Commits every ~5 s /
    /// 20k docs so search works mid-sync. Messages that fail to load are indexed from their
    /// listing header and counted in progress `read_errors`. On cancel, work so far is
    /// committed (the next sync resumes) and `Error::Cancelled` is returned.
    ///
    /// Pipeline: one thread lists headers, [`index_threads`] workers load and extract
    /// messages, and the calling thread feeds the tantivy writer (which has its own
    /// indexing threads). Bounded channels between the stages keep memory flat; no stage
    /// waits for a batch, so one slow message never idles the other workers.
    pub fn sync_source(
        &self,
        id: SourceId,
        src: &dyn MailSource,
        progress: &(dyn Fn(IndexProgress) + Sync),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        let fingerprint = src.fingerprint()?;
        let has_partial = self.reader.searcher().search(&self.partial_query(id), &Count).map_err(ie)? > 0;
        if !has_partial && self.stored_fingerprint(id).as_ref() == Some(&fingerprint) {
            let n = self.doc_count(Some(id))?;
            progress(IndexProgress { source: Some(id), indexed: n, total: Some(n), done: true, ..Default::default() });
            return Ok(());
        }

        let total = estimate_total(src);
        let mut prog = IndexProgress { source: Some(id), total, ..Default::default() };
        progress(prog.clone());

        let folder_names = doc::folder_names(src);
        let existing = self.source_keys(id)?;
        let mut writer: IndexWriter<TantivyDocument> = self.index.writer(WRITER_HEAP).map_err(ie)?;
        let mut att_writer = self.att.writer(ATT_WRITER_HEAP)?;

        let workers = index_threads();
        // Set when the writer fails, so producer and workers wind down.
        let stop = AtomicBool::new(false);
        let halted = || cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed);
        let skipped = AtomicU64::new(0);
        let header_failures = AtomicU64::new(0);
        let fields = &self.fields;
        let folder_names = &folder_names;

        let mut docs = 0u64;
        let mut load_failures = 0u64;
        let mut write_result: Result<()> = Ok(());
        let (job_tx, job_rx) = mpsc::sync_channel::<(String, Header, bool)>(workers * 4);
        let (doc_tx, doc_rx) = mpsc::sync_channel::<Built>(workers * 4);
        let job_rx = Mutex::new(job_rx);
        let vanished = std::thread::scope(|s| {
            let producer = {
                let (halted, skipped, header_failures) = (&halted, &skipped, &header_failures);
                s.spawn(move || {
                    let (mut existing, mut partial) = existing;
                    // Keys already handled in this listing: a message listed twice must not
                    // get a second document (nor be re-read once it is indexed).
                    let mut seen: HashSet<String> = HashSet::new();
                    for next in src.iter_headers() {
                        if halted() {
                            break;
                        }
                        let h = match next {
                            Ok(h) => h,
                            Err(_) => {
                                header_failures.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                        };
                        let Some(loc) = &h.locator else {
                            header_failures.fetch_add(1, Ordering::Relaxed);
                            continue;
                        };
                        let key = doc::make_key(id, loc);
                        if seen.contains(&key) {
                            continue;
                        }
                        if let Some(key) = existing.take(&key) {
                            skipped.fetch_add(1, Ordering::Relaxed);
                            seen.insert(key);
                            continue;
                        }
                        // A partial document was indexed from its header only last time: read it again.
                        let retry = partial.remove(&key);
                        seen.insert(key.clone());
                        if job_tx.send((key, h, retry)).is_err() {
                            break;
                        }
                    }
                    existing.extend(partial);
                    existing
                })
            };

            for _ in 0..workers {
                let (doc_tx, job_rx, halted, stop) = (doc_tx.clone(), &job_rx, &halted, &stop);
                s.spawn(move || loop {
                    // The guard is a temporary of this statement: released before the work.
                    let job = job_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok((key, h, retry)) = job else { break };
                    // When halted, keep draining so a producer blocked on a full queue can exit.
                    if halted() {
                        continue;
                    }
                    // A reader that panics on one malformed message must not abort the whole
                    // sync: like an unreadable message, it is indexed from its listing header.
                    let gathered = std::panic::catch_unwind(AssertUnwindSafe(|| doc::gather(src, h.clone())));
                    let (input, ok) = gathered.unwrap_or_else(|_| (doc::DocInput::from_header(h), false));
                    let mut d = doc::to_document(fields, id, &key, &input, folder_names);
                    if !ok {
                        d.add_u64(fields.partial, 1);
                    }
                    let att_docs = attachments::docs(
                        &self.att.f,
                        id,
                        &key,
                        &input.header,
                        &input.attachments,
                        &input.embedded_subjects,
                    );
                    if doc_tx.send(Built { key, doc: d, att_docs, ok, retry }).is_err() {
                        stop.store(true, Ordering::Relaxed);
                    }
                });
            }
            drop(doc_tx);

            let mut since_commit = 0u64;
            let mut last_commit = Instant::now();
            let mut last_progress = Instant::now();
            for b in &doc_rx {
                if !b.ok {
                    load_failures += 1;
                }
                // A crash between the two commits can leave rows of a message that is not
                // in the message index yet; replacing by message key keeps them unique.
                att_writer.delete_term(self.att.msg_term(&b.key));
                if b.retry {
                    writer.delete_term(Term::from_field_text(self.fields.key, &b.key));
                }
                let added = b
                    .att_docs
                    .into_iter()
                    .try_for_each(|d| att_writer.add_document(d).map(drop))
                    .and_then(|()| writer.add_document(b.doc).map(drop));
                if let Err(e) = added {
                    write_result = Err(ie(e));
                    break;
                }
                docs += 1;
                since_commit += 1;
                if since_commit >= COMMIT_EVERY_DOCS || last_commit.elapsed() >= COMMIT_EVERY {
                    if let Err(e) = self.commit_both(&mut writer, &mut att_writer) {
                        write_result = Err(e);
                        break;
                    }
                    since_commit = 0;
                    last_commit = Instant::now();
                }
                if last_progress.elapsed() >= PROGRESS_EVERY {
                    prog.indexed = skipped.load(Ordering::Relaxed) + docs;
                    prog.read_errors = header_failures.load(Ordering::Relaxed) + load_failures;
                    progress(prog.clone());
                    last_progress = Instant::now();
                }
            }
            if write_result.is_err() {
                stop.store(true, Ordering::Relaxed);
            }
            // Workers whose send now fails set `stop` and keep draining jobs.
            drop(doc_rx);
            // A listing that panicked must not pass for a complete one: its fingerprint
            // would be stored and the messages it never reached would never be indexed.
            producer.join().map_err(|_| ie("listing the source's messages panicked"))
        });
        write_result?;
        let vanished = vanished?;
        prog.indexed = skipped.load(Ordering::Relaxed) + docs;
        prog.read_errors = header_failures.load(Ordering::Relaxed) + load_failures;

        if cancel.load(Ordering::Relaxed) {
            // Keep what was indexed; the stale fingerprint makes the next sync resume.
            self.commit_both(&mut writer, &mut att_writer)?;
            writer.wait_merging_threads().map_err(ie)?;
            att_writer.wait_merging_threads().map_err(ie)?;
            prog.error = Some("cancelled".into());
            progress(prog);
            return Err(Error::Cancelled);
        }

        for key in &vanished {
            writer.delete_term(Term::from_field_text(self.fields.key, key));
            att_writer.delete_term(self.att.msg_term(key));
        }
        self.commit_both(&mut writer, &mut att_writer)?;
        writer.wait_merging_threads().map_err(ie)?;
        att_writer.wait_merging_threads().map_err(ie)?;
        self.meta.lock().unwrap_or_else(|p| p.into_inner()).sources.insert(id.0, fingerprint);
        self.save_meta()?;

        prog.total = Some(prog.indexed);
        prog.done = true;
        progress(prog);
        Ok(())
    }

    fn commit(&self, writer: &mut IndexWriter<TantivyDocument>) -> Result<()> {
        writer.commit().map_err(ie)?;
        self.reader.reload().map_err(ie)
    }

    /// Attachment rows first: a message document is never committed without its rows.
    fn commit_both(
        &self,
        writer: &mut IndexWriter<TantivyDocument>,
        att_writer: &mut IndexWriter<TantivyDocument>,
    ) -> Result<()> {
        att_writer.commit().map_err(ie)?;
        self.att.reload()?;
        self.commit(writer)
    }

    /// One page of the attachment listing (see [`AttachmentQuery`]).
    pub fn attachments(&self, q: &AttachmentQuery) -> Result<AttachmentPage> {
        self.att.query(q)
    }

    /// Indexed locator keys of `id`: complete documents, and partial ones (message
    /// unreadable last time, indexed from its header) that the next sync retries.
    fn source_keys(&self, id: SourceId) -> Result<(HashSet<String>, HashSet<String>)> {
        let searcher = self.reader.searcher();
        let all: HashSet<String> =
            searcher.search(&self.source_query(id), &StrValues("key")).map_err(ie)?.into_iter().collect();
        let partial: HashSet<String> =
            searcher.search(&self.partial_query(id), &StrValues("key")).map_err(ie)?.into_iter().collect();
        let complete = all.difference(&partial).cloned().collect();
        Ok((complete, partial))
    }

    fn partial_query(&self, id: SourceId) -> BooleanQuery {
        let partial = TermQuery::new(Term::from_field_u64(self.fields.partial, 1), IndexRecordOption::Basic);
        BooleanQuery::new(vec![
            (Occur::Must, Box::new(self.source_query(id)) as Box<dyn Query>),
            (Occur::Must, Box::new(partial)),
        ])
    }

    /// Deletes every document of `id` and forgets its fingerprint.
    pub fn remove_source(&self, id: SourceId) -> Result<()> {
        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut writer: IndexWriter<TantivyDocument> = self.index.writer(DELETE_HEAP).map_err(ie)?;
        let mut att_writer = self.att.writer(DELETE_HEAP)?;
        writer.delete_term(self.source_term(id));
        att_writer.delete_term(self.att.source_term(id));
        self.commit_both(&mut writer, &mut att_writer)?;
        writer.wait_merging_threads().map_err(ie)?;
        att_writer.wait_merging_threads().map_err(ie)?;
        self.meta.lock().unwrap_or_else(|p| p.into_inner()).sources.remove(&id.0);
        self.save_meta()
    }

    /// Runs a Gmail-style query: whitespace-separated clauses are ANDed, `a OR b`, `-x`,
    /// `( … )`, `"phrase"`, `term~` (fuzzy), and the field prefixes `from:` `to:` `cc:` `bcc:`
    /// `subject:` `body:` `attachment:` `has:attachment` `is:` `label:` `in:` `before:` `after:`
    /// `larger:` `smaller:`. Bare terms search subject, sender, recipients, body, item
    /// details (location, company, phones, …) and attachment names/text. An empty query
    /// matches everything; an invalid one (unknown `is:` value, bad date or size, parentheses
    /// nested deeper than 32) is an error. See the `query` module for how calendar, contact
    /// and task items map onto these fields.
    ///
    /// `opts.limit == 0` returns [`DEFAULT_LIMIT`] hits. Hits are relevance-ranked unless
    /// `opts.sort` is set. `Date` sorts by the message date, `Start` by
    /// [`Header::start_ms`] (appointment start, task due date, else the message date), the
    /// time `before:`/`after:` and the year facet use; `Native` sorts by date (the index has
    /// no container order).
    pub fn search(&self, query_str: &str, opts: &SearchOptions) -> Result<SearchResults> {
        let started = Instant::now();
        let f = &self.fields;
        let searcher = self.reader.searcher();
        let mut tr = query::Translator { f, analyzer: schema::analyzer() };
        let user_q: Box<dyn Query> = tr.translate(&query::parse(query_str)?)?.unwrap_or_else(|| Box::new(AllQuery));

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, user_q.box_clone())];
        if !opts.sources.is_empty() {
            let any_source = BooleanQuery::new(
                opts.sources
                    .iter()
                    .map(|s| (Occur::Should, Box::new(self.source_query(*s)) as Box<dyn Query>))
                    .collect(),
            );
            clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(Box::new(any_source), 0.0))));
        }
        if let Some(folder) = &opts.folder {
            let t = TermQuery::new(Term::from_field_text(f.folder, &folder.0), IndexRecordOption::Basic);
            clauses.push((Occur::Must, Box::new(ConstScoreQuery::new(Box::new(t), 0.0))));
        }
        let q = BooleanQuery::new(clauses);

        let limit = if opts.limit == 0 { DEFAULT_LIMIT } else { opts.limit };
        let top = page(&searcher, limit, opts.offset);
        let (hits, total, facets) = match opts.sort {
            None => run(&searcher, &q, top.order_by_score(), opts.facets, |s: &f32| *s)?,
            Some(sort) => {
                let order = if sort.descending { Order::Desc } else { Order::Asc };
                match sort.key {
                    SortKey::Date | SortKey::Native => {
                        run(&searcher, &q, top.order_by_fast_field::<i64>("date", order), opts.facets, |_| 0.0)?
                    }
                    SortKey::Start => {
                        run(&searcher, &q, top.order_by_fast_field::<i64>("start", order), opts.facets, |_| 0.0)?
                    }
                    SortKey::Size => {
                        run(&searcher, &q, top.order_by_fast_field::<u64>("size", order), opts.facets, |_| 0.0)?
                    }
                    SortKey::Subject => {
                        run(&searcher, &q, top.order_by_string_fast_field("subject_sort", order), opts.facets, |_| 0.0)?
                    }
                    SortKey::From => {
                        run(&searcher, &q, top.order_by_string_fast_field("from_sort", order), opts.facets, |_| 0.0)?
                    }
                }
            }
        };

        let mut out_hits = Vec::with_capacity(hits.len());
        if !hits.is_empty() {
            let mut body_snip = SnippetGenerator::create(&searcher, &*user_q, f.body).map_err(ie)?;
            body_snip.set_max_num_chars(SNIPPET_CHARS);
            let mut subject_snip = SnippetGenerator::create(&searcher, &*user_q, f.subject).map_err(ie)?;
            subject_snip.set_max_num_chars(SNIPPET_CHARS);
            for (score, addr) in hits {
                let (source, header, preview) = self.load_hit(&searcher, addr)?;
                let snippet = make_snippet(&body_snip, &subject_snip, &preview, &header.subject);
                out_hits.push(SearchHit { source, header, score, snippet });
            }
        }

        let mut res = SearchResults { total, hits: out_hits, ..Default::default() };
        if let Some(fc) = facets {
            let mut from: Vec<FacetCount> =
                fc.from.into_iter().map(|(value, count)| FacetCount { value, count }).collect();
            from.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
            from.truncate(TOP_SENDERS);
            let mut years: Vec<(u64, u64)> = fc.year.into_iter().collect();
            years.sort_unstable_by_key(|&(year, _)| std::cmp::Reverse(year));
            let mut sources: Vec<(u64, u64)> = fc.source.into_iter().collect();
            sources.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            res.facets_from = from;
            res.facets_year = years.into_iter().map(|(y, count)| FacetCount { value: y.to_string(), count }).collect();
            res.facets_source =
                sources.into_iter().map(|(s, count)| FacetCount { value: s.to_string(), count }).collect();
        }
        res.took_ms = started.elapsed().as_millis() as u64;
        Ok(res)
    }

    fn load_hit(&self, searcher: &Searcher, addr: DocAddress) -> Result<(SourceId, Header, String)> {
        let d: TantivyDocument = searcher.doc(addr).map_err(ie)?;
        let f = &self.fields;
        let source = d.get_first(f.source).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let header = d
            .get_first(f.header)
            .and_then(|v| v.as_bytes())
            .and_then(|b| serde_json::from_slice::<Header>(b).ok())
            .ok_or_else(|| Error::Format("index document without a readable header".into()))?;
        let preview = d.get_first(f.preview).and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok((SourceId(source), header, preview))
    }

    /// Documents whose message_id / in_reply_to / references / conversation intersect the
    /// given keys (for threading + dedupe), oldest first.
    pub fn related(&self, message_ids: &[String], conversation: Option<&str>) -> Result<Vec<(SourceId, Header)>> {
        let f = &self.fields;
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let mut seen = HashSet::new();
        for id in message_ids.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if !seen.insert(id) {
                continue;
            }
            for field in [f.message_id, f.in_reply_to, f.references] {
                clauses.push((
                    Occur::Should,
                    Box::new(TermQuery::new(Term::from_field_text(field, id), IndexRecordOption::Basic)),
                ));
            }
        }
        if let Some(c) = conversation.map(str::trim).filter(|c| !c.is_empty()) {
            clauses.push((
                Occur::Should,
                Box::new(TermQuery::new(Term::from_field_text(f.conversation, c), IndexRecordOption::Basic)),
            ));
        }
        if clauses.is_empty() {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();
        let top = TopDocs::with_limit(RELATED_LIMIT).order_by_fast_field::<i64>("date", Order::Asc);
        let docs = searcher.search(&BooleanQuery::new(clauses), &top).map_err(ie)?;
        docs.into_iter().map(|(_, addr)| self.load_hit(&searcher, addr).map(|(s, h, _)| (s, h))).collect()
    }

    /// Stored body preview (first ~2 KB of text) of each message, `None` if not indexed.
    pub fn previews(&self, items: &[(SourceId, mail_model::Locator)]) -> Result<Vec<Option<String>>> {
        let searcher = self.reader.searcher();
        items
            .iter()
            .map(|(s, loc)| {
                let key = doc::make_key(*s, loc);
                let q = TermQuery::new(Term::from_field_text(self.fields.key, &key), IndexRecordOption::Basic);
                let top = searcher.search(&q, &TopDocs::with_limit(1).order_by_score()).map_err(ie)?;
                match top.first() {
                    Some((_, addr)) => self.load_hit(&searcher, *addr).map(|(_, _, p)| Some(p)),
                    None => Ok(None),
                }
            })
            .collect()
    }

    /// Number of indexed documents, for one source or all.
    pub fn doc_count(&self, source: Option<SourceId>) -> Result<u64> {
        let searcher = self.reader.searcher();
        match source {
            None => Ok(searcher.num_docs()),
            Some(id) => Ok(searcher.search(&self.source_query(id), &Count).map_err(ie)? as u64),
        }
    }
}

/// `TopDocs` for one page. tantivy reserves `2 × (limit + offset)` slots per segment, so
/// both are clamped to the live document count: an absurd page from a caller (an MCP agent
/// asking for offset 2^60) cannot exhaust memory.
pub(crate) fn page(searcher: &Searcher, limit: usize, offset: usize) -> TopDocs {
    let docs = usize::try_from(searcher.num_docs()).unwrap_or(usize::MAX);
    let offset = offset.min(docs);
    TopDocs::with_limit(limit.min(docs - offset).max(1)).and_offset(offset)
}

/// Scored hit addresses, total match count, and facets (when requested) of one search.
type Collected = (Vec<(f32, DocAddress)>, u64, Option<Facets>);

/// Runs a top-docs collector plus either a plain count or the facet collector.
fn run<K, C>(searcher: &Searcher, q: &dyn Query, top: C, facets: bool, score: impl Fn(&K) -> f32) -> Result<Collected>
where
    C: Collector<Fruit = Vec<(K, DocAddress)>>,
{
    let (docs, total, facets) = if facets {
        let (docs, fc) = searcher.search(q, &(top, FacetCollector)).map_err(ie)?;
        let total = fc.total;
        (docs, total, Some(fc))
    } else {
        let (docs, n) = searcher.search(q, &(top, Count)).map_err(ie)?;
        (docs, n as u64, None)
    };
    Ok((docs.into_iter().map(|(k, a)| (score(&k), a)).collect(), total, facets))
}

fn make_snippet(body: &SnippetGenerator, subject: &SnippetGenerator, preview: &str, subj: &str) -> String {
    let s = body.snippet(preview);
    if !s.highlighted().is_empty() {
        return s.to_html();
    }
    let s = subject.snippet(subj);
    if !s.highlighted().is_empty() {
        return s.to_html();
    }
    let mut end = preview.len().min(SNIPPET_CHARS);
    while !preview.is_char_boundary(end) {
        end -= 1;
    }
    html_escape::encode_text(&preview[..end]).into_owned()
}

/// Messages in real (non-label) folders, if the source can count them cheaply.
fn estimate_total(src: &dyn MailSource) -> Option<u64> {
    let folders = src.folders().ok()?;
    let mut n = 0;
    for f in folders.iter().filter(|f| f.kind != mail_model::FolderKind::Label) {
        n += match f.message_count {
            Some(c) => c,
            None => src.count(&f.id).ok()?,
        };
    }
    Some(n)
}

/// Removes the index files (regular files only) from `dir`.
fn clear_dir(dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Removes the attachment listing index (`dir/att`) entirely.
fn clear_att_dir(att_dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(att_dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}
