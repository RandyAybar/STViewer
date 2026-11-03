//! Attachment listing: one document per attachment, in a second tantivy index next to
//! the message index (`<index dir>/att`). Rows are written by the same sync that indexes
//! their message and deleted with it (by message key), so both stay consistent.

use std::collections::HashMap;
use std::ops::Bound;
use std::path::Path;
use std::time::Instant;

use mail_model::{
    AttachmentCategory, AttachmentMeta, AttachmentPage, AttachmentQuery, AttachmentRow, AttachmentSortKey, Header,
    Result, SourceId,
};
use tantivy::collector::{Collector, Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{AllQuery, BooleanQuery, ConstScoreQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, NumericOptions, Schema, TextOptions, Value, FAST, STORED, STRING};
use tantivy::{DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, Searcher, TantivyDocument, Term};

use crate::collectors::StrCounts;
use crate::{directory, ie, page, schema};

pub(crate) const DIR: &str = "att";
const DEFAULT_LIMIT: usize = 100;

#[derive(Clone, Copy)]
pub(crate) struct AttFields {
    source: Field,
    /// Message key (`"{source}:{locator json}"`) of the carrying message.
    msg_key: Field,
    /// File name + sender + subject, full-text.
    text: Field,
    name_sort: Field,
    /// Lowercase extension: `exclude_extensions`.
    ext: Field,
    /// `"{ext}\0{lowercase name}"`, FAST: the `Type` sort (extension, then name).
    type_sort: Field,
    /// `AttachmentCategory::as_str`, FAST for the per-category chip counts.
    category: Field,
    size: Field,
    date: Field,
    from_sort: Field,
    inline: Field,
    /// Serialized `AttachmentRow`.
    row: Field,
}

fn build_schema() -> (Schema, AttFields) {
    let mut b = Schema::builder();
    let num = NumericOptions::default().set_indexed().set_fast();
    let fast_str = TextOptions::default().set_fast("raw");
    let f = AttFields {
        source: b.add_u64_field("source", num.clone()),
        msg_key: b.add_text_field("msg_key", STRING),
        text: b.add_text_field("text", schema::text(false)),
        name_sort: b.add_text_field("name_sort", fast_str.clone()),
        ext: b.add_text_field("ext", STRING),
        type_sort: b.add_text_field("type_sort", fast_str.clone()),
        category: b.add_text_field("category", STRING | FAST),
        size: b.add_u64_field("size", num.clone()),
        date: b.add_i64_field("date", num.clone()),
        from_sort: b.add_text_field("from_sort", fast_str),
        inline: b.add_u64_field("inline", num),
        row: b.add_bytes_field("row", STORED),
    };
    (b.build(), f)
}

pub(crate) struct AttIndex {
    index: Index,
    reader: IndexReader,
    pub(crate) f: AttFields,
}

impl AttIndex {
    /// Opens (or creates) the attachment index in `dir`.
    pub(crate) fn open(dir: &Path) -> tantivy::Result<AttIndex> {
        std::fs::create_dir_all(dir)?;
        let (schema, f) = build_schema();
        let mmap = MmapDirectory::open(dir).map_err(tantivy::TantivyError::from)?;
        let index = Index::open_or_create(directory::RetryingDirectory(mmap), schema)?;
        schema::register_analyzers(&index);
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        Ok(AttIndex { index, reader, f })
    }

    pub(crate) fn reload(&self) -> Result<()> {
        self.reader.reload().map_err(ie)
    }

    pub(crate) fn source_term(&self, id: SourceId) -> Term {
        Term::from_field_u64(self.f.source, u64::from(id.0))
    }

    pub(crate) fn msg_term(&self, key: &str) -> Term {
        Term::from_field_text(self.f.msg_key, key)
    }

    pub(crate) fn writer(&self, heap: usize) -> Result<IndexWriter<TantivyDocument>> {
        self.index.writer(heap).map_err(ie)
    }

    pub(crate) fn query(&self, q: &AttachmentQuery) -> Result<AttachmentPage> {
        let started = Instant::now();
        let searcher = self.reader.searcher();
        let base = self.filters(q)?;

        let mut main = clone_clauses(&base);
        if !q.categories.is_empty() {
            main.push(any_of(q.categories.iter().map(|c| self.category_query(*c))));
        }
        let main_q = finish(main);
        let limit = if q.limit == 0 { DEFAULT_LIMIT } else { q.limit };
        let top = page(&searcher, limit, q.offset);
        let order = if q.sort.descending { Order::Desc } else { Order::Asc };
        let key = q.sort.key;
        let (addrs, total, counts) = if q.categories.is_empty() {
            // The chip counts cover exactly the main query: one pass yields page, counts and
            // total (every row has one category).
            let (addrs, counts) = sorted_page(&searcher, &main_q, top, key, order, StrCounts("category"))?;
            let total = counts.values().sum();
            (addrs, total, counts)
        } else {
            let (addrs, total) = sorted_page(&searcher, &main_q, top, key, order, Count)?;
            // Chip counts ignore the category restriction (exclusions still apply).
            let counts = searcher.search(&finish(base), &StrCounts("category")).map_err(ie)?;
            (addrs, total as u64, counts)
        };

        let mut rows = Vec::with_capacity(addrs.len());
        for addr in addrs {
            let doc: TantivyDocument = searcher.doc(addr).map_err(ie)?;
            let bytes =
                doc.get_first(self.f.row).and_then(|v| v.as_bytes()).ok_or_else(|| ie("attachment row missing"))?;
            rows.push(serde_json::from_slice::<AttachmentRow>(bytes).map_err(ie)?);
        }

        let category_counts =
            AttachmentCategory::ALL.into_iter().filter_map(|c| counts.get(c.as_str()).map(|&n| (c, n))).collect();
        Ok(AttachmentPage { total, rows, category_counts, took_ms: started.elapsed().as_millis() as u64 })
    }

    /// Every restriction of `q` except its `categories` include-list.
    fn filters(&self, q: &AttachmentQuery) -> Result<Vec<(Occur, Box<dyn Query>)>> {
        let f = &self.f;
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        if !q.sources.is_empty() {
            clauses.push(any_of(q.sources.iter().map(|s| (Occur::Should, term_query(self.source_term(*s))))));
        }
        if !q.include_inline {
            clauses.push((Occur::MustNot, term_query(Term::from_field_u64(f.inline, 1))));
        }
        for c in &q.exclude_categories {
            clauses.push((Occur::MustNot, term_query(Term::from_field_text(f.category, c.as_str()))));
        }
        for e in &q.exclude_extensions {
            let e = e.trim().trim_start_matches('.').to_lowercase();
            if !e.is_empty() {
                clauses.push((Occur::MustNot, term_query(Term::from_field_text(f.ext, &e))));
            }
        }
        if q.min_size.is_some() || q.max_size.is_some() {
            let lo = q.min_size.map_or(Bound::Unbounded, |n| Bound::Included(Term::from_field_u64(f.size, n)));
            let hi = q.max_size.map_or(Bound::Unbounded, |n| Bound::Included(Term::from_field_u64(f.size, n)));
            clauses.push((Occur::Must, Box::new(RangeQuery::new(lo, hi))));
        }
        let mut analyzer = schema::analyzer();
        for word in q.text.split_whitespace() {
            let (negate, word) = match word.strip_prefix('-') {
                Some(w) if !w.is_empty() => (true, w),
                _ => (false, word),
            };
            let mut tokens = Vec::new();
            let mut stream = analyzer.token_stream(word);
            while let Some(t) = stream.next() {
                tokens.push(t.text.clone());
            }
            // Every token of the word must prefix-match (so `image0` matches `image001`).
            for t in tokens {
                let pattern = format!("{}.*", regex_escape(&t));
                let prefix = RegexQuery::from_pattern(&pattern, f.text).map_err(ie)?;
                clauses.push((if negate { Occur::MustNot } else { Occur::Must }, Box::new(prefix)));
            }
        }
        Ok(clauses)
    }

    fn category_query(&self, c: AttachmentCategory) -> (Occur, Box<dyn Query>) {
        (Occur::Should, term_query(Term::from_field_text(self.f.category, c.as_str())))
    }
}

fn term_query(t: Term) -> Box<dyn Query> {
    Box::new(TermQuery::new(t, IndexRecordOption::Basic))
}

fn clone_clauses(c: &[(Occur, Box<dyn Query>)]) -> Vec<(Occur, Box<dyn Query>)> {
    c.iter().map(|(o, q)| (*o, q.box_clone())).collect()
}

/// A Must clause matching any of the Should clauses.
fn any_of(items: impl Iterator<Item = (Occur, Box<dyn Query>)>) -> (Occur, Box<dyn Query>) {
    (Occur::Must, Box::new(ConstScoreQuery::new(Box::new(BooleanQuery::new(items.collect())), 0.0)))
}

/// A query from clauses; pure exclusions need a positive clause to subtract from.
fn finish(mut clauses: Vec<(Occur, Box<dyn Query>)>) -> BooleanQuery {
    if !clauses.iter().any(|(o, _)| *o == Occur::Must) {
        clauses.push((Occur::Must, Box::new(AllQuery)));
    }
    BooleanQuery::new(clauses)
}

/// One page of `q` in `key` order, plus the fruit of `extra`, in a single pass.
fn sorted_page<X: Collector>(
    searcher: &Searcher,
    q: &dyn Query,
    top: TopDocs,
    key: AttachmentSortKey,
    order: Order,
    extra: X,
) -> Result<(Vec<DocAddress>, X::Fruit)> {
    fn run<K, C: Collector<Fruit = Vec<(K, DocAddress)>>, X: Collector>(
        searcher: &Searcher,
        q: &dyn Query,
        top: C,
        extra: X,
    ) -> Result<(Vec<DocAddress>, X::Fruit)> {
        let (docs, fruit) = searcher.search(q, &(top, extra)).map_err(ie)?;
        Ok((docs.into_iter().map(|(_, addr)| addr).collect(), fruit))
    }
    match key {
        AttachmentSortKey::Date => run(searcher, q, top.order_by_fast_field::<i64>("date", order), extra),
        AttachmentSortKey::Size => run(searcher, q, top.order_by_fast_field::<u64>("size", order), extra),
        AttachmentSortKey::Name => run(searcher, q, top.order_by_string_fast_field("name_sort", order), extra),
        AttachmentSortKey::Type => run(searcher, q, top.order_by_string_fast_field("type_sort", order), extra),
        AttachmentSortKey::From => run(searcher, q, top.order_by_string_fast_field("from_sort", order), extra),
    }
}

/// Analyzer tokens are lowercase alphanumerics, but escape defensively.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

/// One listing document per attachment of a message.
pub(crate) fn docs(
    f: &AttFields,
    source: SourceId,
    msg_key: &str,
    header: &Header,
    metas: &[AttachmentMeta],
    embedded_subjects: &HashMap<u32, String>,
) -> Vec<TantivyDocument> {
    if header.locator.is_none() {
        return Vec::new();
    }
    let from_sort = header
        .from
        .as_ref()
        .and_then(|a| a.name.as_deref().filter(|n| !n.trim().is_empty()).or(a.email.as_deref()))
        .unwrap_or("")
        .trim()
        .to_lowercase();
    metas
        .iter()
        .filter_map(|m| AttachmentRow::new(source, header, m, embedded_subjects.get(&m.index).map(String::as_str)))
        .map(|row| {
            let category = row.category;
            let mut d = TantivyDocument::default();
            d.add_u64(f.source, u64::from(source.0));
            d.add_text(f.msg_key, msg_key);
            d.add_text(f.text, &row.filename);
            if let Some(a) = &row.from {
                for part in [a.name.as_deref(), a.email.as_deref()].into_iter().flatten() {
                    d.add_text(f.text, part);
                }
            }
            d.add_text(f.text, &row.subject);
            let name = row.filename.to_lowercase();
            d.add_text(f.type_sort, format!("{}\0{name}", row.extension));
            d.add_text(f.name_sort, name);
            d.add_text(f.ext, &row.extension);
            d.add_text(f.category, category.as_str());
            d.add_u64(f.size, row.size);
            d.add_i64(f.date, row.date.unwrap_or(i64::MIN));
            d.add_text(f.from_sort, &from_sort);
            d.add_u64(f.inline, u64::from(row.inline));
            d.add_bytes(f.row, &serde_json::to_vec(&row).expect("row serialises"));
            d
        })
        .collect()
}
