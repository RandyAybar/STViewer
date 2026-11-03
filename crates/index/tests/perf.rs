//! `cargo test --profile release-fast -p stv-index --test perf -- --ignored --nocapture`
//!
//! Indexes 200k synthetic messages (generated on demand, never held in memory at once;
//! every third one carries attachments) and reports indexing throughput, index size, and
//! p50/p99 latency of message searches and attachment-listing queries.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::time::Instant;

use mail_model::*;
use stv_index::MailIndex;

const N: u32 = 200_000;
const VOCAB: usize = 20_000;
const SENDERS: u32 = 500;

fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// Deterministic pseudo-word for a vocabulary index.
fn word(i: usize) -> String {
    const SYL: [&str; 16] =
        ["ka", "lo", "mi", "ne", "ru", "sa", "te", "vo", "pri", "dex", "lan", "tor", "qu", "bel", "zo", "fin"];
    let mut s = String::new();
    let mut n = i + 16;
    while n > 0 {
        s.push_str(SYL[n % 16]);
        n /= 16;
    }
    s
}

/// Zipf-ish pick: low indices are common.
fn pick(r: &mut impl FnMut() -> u64) -> usize {
    let u = (r() % 1_000_000) as f64 / 1_000_000.0;
    ((VOCAB as f64).powf(u) as usize).saturating_sub(1).min(VOCAB - 1)
}

struct Synthetic {
    words: Vec<String>,
    mtime: AtomicI64,
}

impl Synthetic {
    fn header(&self, nid: u32) -> Header {
        let mut r = rng(u64::from(nid));
        let subject: Vec<&str> = (0..5).map(|_| self.words[pick(&mut r)].as_str()).collect();
        let s = (r() % u64::from(SENDERS)) as u32;
        Header {
            locator: Some(Locator::Pst { nid }),
            folder: Some(FolderId(format!("f{}", nid % 20))),
            subject: subject.join(" "),
            from: Some(Address {
                name: Some(format!("Sender {s}")),
                email: Some(format!("user{s}@corp{}.example", s % 17)),
            }),
            to: vec![Address { name: None, email: Some(format!("user{}@corp.example", r() % 500)) }],
            date: Some(1_262_304_000_000 + (r() % (10 * 365 * 86_400_000)) as i64),
            size: 2_000 + r() % 200_000,
            has_attachments: nid.is_multiple_of(3),
            flags: MessageFlags { read: !r().is_multiple_of(4), flagged: r().is_multiple_of(50), ..Default::default() },
            message_id: Some(format!("{nid}@synthetic")),
            ..Default::default()
        }
    }
}

impl MailSource for Synthetic {
    fn kind(&self) -> SourceKind {
        SourceKind::Pst
    }
    fn display_name(&self) -> String {
        "synthetic".into()
    }
    fn fingerprint(&self) -> Result<Fingerprint> {
        Ok(Fingerprint {
            path: PathBuf::from("synthetic"),
            size: u64::from(N),
            mtime: self.mtime.load(Relaxed),
            head_hash: 0,
        })
    }
    fn folders(&self) -> Result<Vec<Folder>> {
        Ok((0..20)
            .map(|i| Folder {
                id: FolderId(format!("f{i}")),
                parent: None,
                name: format!("Folder {i}"),
                kind: FolderKind::Mail,
                message_count: Some(u64::from(N / 20)),
                unread_count: None,
            })
            .collect())
    }
    fn count(&self, _folder: &FolderId) -> Result<u64> {
        Ok(u64::from(N / 20))
    }
    fn list(&self, _folder: &FolderId, _range: Range<u64>, _sort: Sort) -> Result<Vec<Header>> {
        Ok(Vec::new())
    }
    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_> {
        Box::new((1..=N).map(|n| Ok(self.header(n))))
    }
    fn message(&self, loc: &Locator) -> Result<Message> {
        let Locator::Pst { nid } = loc else { return Err(Error::NotFound("loc".into())) };
        let header = self.header(*nid);
        let mut r = rng(u64::from(*nid) ^ 0xABCD);
        let len = 80 + (r() % 300) as usize;
        let mut body = String::with_capacity(len * 8);
        for i in 0..len {
            body.push_str(&self.words[pick(&mut r)]);
            body.push(if i % 15 == 14 { '\n' } else { ' ' });
        }
        let attachments = if nid.is_multiple_of(3) {
            const EXT: [&str; 7] = ["pdf", "jpg", "docx", "xlsx", "png", "zip", "ics"];
            (0..1 + (r() % 3) as u32)
                .map(|index| AttachmentMeta {
                    index,
                    filename: Some(format!("{}.{}", self.words[pick(&mut r)], EXT[(r() % 7) as usize])),
                    size: 1_000 + r() % 5_000_000,
                    inline: r().is_multiple_of(4),
                    ..Default::default()
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Message { header, body: Body { text: Some(body), ..Default::default() }, attachments, ..Default::default() })
    }
    fn attachment(&self, _loc: &Locator, _index: u32) -> Result<Vec<u8>> {
        Err(Error::NotFound("none".into()))
    }
    fn raw_mime(&self, _loc: &Locator) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn properties(&self, _loc: &Locator) -> Result<Vec<PropertyView>> {
        Ok(Vec::new())
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

/// Total bytes of the files under `dir`.
fn dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let meta = e.metadata().unwrap();
            if meta.is_dir() {
                dir_size(&e.path())
            } else {
                meta.len()
            }
        })
        .sum()
}

/// Runs every query of `queries` ten times (after one warm-up round) and prints p50/p99/max.
fn report<Q>(label: &str, queries: &[Q], mut run: impl FnMut(&Q)) {
    queries.iter().for_each(&mut run);
    let mut lat = Vec::new();
    for _ in 0..10 {
        for q in queries {
            let t = Instant::now();
            run(q);
            lat.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{label:>17}: {} queries, p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        lat.len(),
        pct(&lat, 0.5),
        pct(&lat, 0.99),
        lat.last().unwrap()
    );
}

#[test]
#[ignore]
fn perf_200k() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let src = Synthetic { words: (0..VOCAB).map(word).collect(), mtime: AtomicI64::new(0) };

    let t = Instant::now();
    ix.sync_source(SourceId(1), &src, &|_| {}, &AtomicBool::new(false)).unwrap();
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(ix.doc_count(None).unwrap(), u64::from(N));
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    println!("indexed {N} docs in {secs:.1}s = {:.0} docs/s ({threads} cores)", f64::from(N) / secs);
    let att = dir_size(&dir.path().join("att"));
    println!(
        "index size: {:.1} MB messages + {:.1} MB attachments",
        (dir_size(dir.path()) - att) as f64 / 1e6,
        att as f64 / 1e6
    );

    // Fingerprint changed, content identical: key diff only, no message reads.
    src.mtime.store(1, Relaxed);
    let t = Instant::now();
    ix.sync_source(SourceId(1), &src, &|_| {}, &AtomicBool::new(false)).unwrap();
    println!("no-op resync (fingerprint changed, nothing new): {:.2}s", t.elapsed().as_secs_f64());
    let t = Instant::now();
    ix.sync_source(SourceId(1), &src, &|_| {}, &AtomicBool::new(false)).unwrap();
    println!("sync with unchanged fingerprint: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);

    let w = |i: usize| src.words[i].clone();
    let queries: Vec<String> = vec![
        w(3),
        w(500),
        w(15_000),
        format!("{} {}", w(10), w(50)),
        format!("\"{} {}\"", w(1), w(2)),
        format!("{} OR {}", w(100), w(200)),
        format!("{}~", w(4000)),
        "from:user42".into(),
        "from:user42@corp8.example".into(),
        format!("subject:{}", w(20)),
        format!("{} after:2015-01-01 before:2016-01-01", w(5)),
        format!("{} -{}", w(7), w(8)),
        "is:unread is:flagged".into(),
        "larger:150K".into(),
        String::new(),
    ];
    let sorted_opts =
        SearchOptions { limit: 50, sort: Some(Sort { key: SortKey::Date, descending: true }), ..Default::default() };
    let facet_opts = SearchOptions { limit: 50, facets: true, ..Default::default() };
    let plain_opts = SearchOptions { limit: 50, ..Default::default() };

    for (label, opts) in [("relevance", &plain_opts), ("date-sorted", &sorted_opts), ("relevance+facets", &facet_opts)]
    {
        report(label, &queries, |q| {
            std::hint::black_box(ix.search(q, opts).unwrap());
        });
    }
    for q in &queries {
        let t = Instant::now();
        let r = ix.search(q, &facet_opts).unwrap();
        println!("  {:>8.2} ms  {:>7} hits  {q}", t.elapsed().as_secs_f64() * 1000.0, r.total);
    }

    let att_queries: Vec<AttachmentQuery> = vec![
        AttachmentQuery::default(),
        AttachmentQuery { include_inline: true, ..Default::default() },
        AttachmentQuery { text: w(3)[..3].to_string(), ..Default::default() },
        AttachmentQuery { text: format!("{} -{}", w(10), w(11)), ..Default::default() },
        AttachmentQuery { categories: vec![AttachmentCategory::Pdf, AttachmentCategory::Image], ..Default::default() },
        AttachmentQuery {
            min_size: Some(1_000_000),
            exclude_extensions: vec!["png".into()],
            sort: AttachmentSort { key: AttachmentSortKey::Name, descending: false },
            ..Default::default()
        },
        AttachmentQuery {
            sort: AttachmentSort { key: AttachmentSortKey::Size, descending: true },
            ..Default::default()
        },
    ];
    report("attachments", &att_queries, |q| {
        std::hint::black_box(ix.attachments(q).unwrap());
    });
    for q in &att_queries {
        let t = Instant::now();
        let r = ix.attachments(q).unwrap();
        println!("  {:>8.2} ms  {:>7} rows  {:?}", t.elapsed().as_secs_f64() * 1000.0, r.total, q.text);
    }
}
