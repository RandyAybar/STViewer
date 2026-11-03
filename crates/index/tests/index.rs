mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use mail_model::*;
use stv_index::MailIndex;

const SRC: SourceId = SourceId(1);
const SRC2: SourceId = SourceId(2);

fn alice() -> Address {
    addr("Alice Smith", "alice@example.com")
}
fn bob() -> Address {
    addr("Bob Jones", "bob@corp.example")
}
fn carol() -> Address {
    addr("Carol Díaz", "carol@example.org")
}
fn dave() -> Address {
    addr("Dave", "dave@startup.example")
}
fn erin() -> Address {
    addr("Erin", "erin@example.com")
}

/// 50 crafted messages; see the per-message comments for what each exercises.
fn corpus() -> Vec<FakeMsg> {
    let docx = zip_bytes(&[(
        "word/document.xml",
        "<w:document><w:body><w:p><w:r><w:t>confidential revenue projections</w:t></w:r></w:p></w:body></w:document>",
    )]);
    let mut v = vec![
        // Thread root with a docx attachment and a label.
        msg(
            1,
            "inbox",
            "Quarterly budget review",
            alice(),
            "2023-01-15",
            "Please review the attached budget spreadsheet before Friday.",
        )
        .attach("report.docx", None, docx)
        .with(|h| {
            h.message_id = Some("m1@example.com".into());
            h.labels = vec!["Important".into()];
        }),
        msg(2, "inbox", "Re: Quarterly budget review", bob(), "2023-01-16", "Looks good to me.").with(|h| {
            h.to = vec![alice()];
            h.message_id = Some("m2@example.com".into());
            h.in_reply_to = Some("m1@example.com".into());
            h.references = vec!["m1@example.com".into()];
        }),
        msg(3, "inbox", "Re: Re: Quarterly budget review", alice(), "2023-01-17", "Thanks Bob.").with(|h| {
            h.message_id = Some("m3@example.com".into());
            h.in_reply_to = Some("m2@example.com".into());
            h.references = vec!["m1@example.com".into(), "m2@example.com".into()];
        }),
    ];
    // HTML-only body, flagged.
    let mut m4 = msg(4, "inbox", "Team offsite", carol(), "2022-06-01", "").with(|h| h.flags.flagged = true);
    m4.body = Body {
        text: None,
        html: Some("<p>Team <b>offsite</b> in Barcelona</p><style>.x{color:red}</style>".into()),
        rtf: None,
    };
    v.push(m4);
    // RTF-only body.
    let mut m5 = msg(5, "projects", "Kickoff", dave(), "2021-03-03", "");
    m5.body =
        Body { text: None, html: None, rtf: Some(r"{\rtf1\ansi Kickoff meeting notes\par Agenda attached}".into()) };
    v.push(m5);
    // PDF attachment.
    v.push(
        msg(
            6,
            "inbox",
            "Your invoice",
            addr("Vendor Billing", "billing@vendor.example"),
            "2024-02-10",
            "See attachment.",
        )
        .attach("invoice.pdf", Some("application/pdf"), tiny_pdf("Invoice number 98765")),
    );
    // Text attachment.
    v.push(msg(7, "projects", "Notes", erin(), "2020-05-05", "fyi").attach(
        "notes.txt",
        None,
        b"secret project nebula".to_vec(),
    ));
    // Natively embedded message.
    let mut m8 = msg(8, "inbox", "Fwd: trip", bob(), "2023-07-07", "see forwarded");
    m8.attachments.push((
        AttachmentMeta {
            index: 0,
            filename: Some("itinerary.msg".into()),
            kind: AttachmentKind::EmbeddedMessage,
            size: 500,
            ..Default::default()
        },
        Vec::new(),
    ));
    m8.embedded.push((
        0,
        Message {
            header: Header { subject: "Forwarded itinerary".into(), ..Default::default() },
            body: Body { text: Some("flight to Reykjavik".into()), ..Default::default() },
            ..Default::default()
        },
    ));
    m8.header.has_attachments = true;
    v.push(m8);
    v.push(msg(9, "inbox", "Unread ping", dave(), "2024-03-01", "ping").with(|h| h.flags.read = false));
    v.push(msg(10, "sent", "Sent report", alice(), "2023-03-03", "sending the report").with(|h| h.to = vec![carol()]));
    // Large message with a non-extractable image.
    v.push(
        msg(11, "inbox", "Big scan", erin(), "2019-09-09", "scanned documents")
            .attach("scan.jpg", Some("image/jpeg"), vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10])
            .with(|h| h.size = 10 * 1024 * 1024),
    );
    v.push(msg(
        12,
        "inbox",
        "Kubernetes rollout",
        dave(),
        "2022-11-11",
        "We migrated the cluster to kubernetes yesterday.",
    ));
    v.push(msg(13, "inbox", "Fox story", erin(), "2021-01-01", "the quick brown fox jumps over the lazy dog"));
    v.push(msg(14, "inbox", "Fox story reversed", erin(), "2021-01-02", "brown quick fox and a dog"));
    v.push(msg(15, "inbox", "Café réunion", carol(), "2022-02-02", "Rendez-vous au café"));
    // message() fails for this one (see `broken`).
    v.push(msg(16, "inbox", "Unreadable thing", bob(), "2020-12-12", "never seen"));
    v.push(msg(17, "inbox", "CC test", erin(), "2023-05-05", "cc").with(|h| {
        h.to = vec![carol()];
        h.cc = vec![bob()];
    }));
    v.push(
        msg(18, "inbox", "Conversation A", erin(), "2022-08-08", "first in conversation")
            .with(|h| h.conversation = Some("conv-abc".into())),
    );
    v.push(
        msg(19, "inbox", "Conversation B", bob(), "2022-08-09", "second in conversation")
            .with(|h| h.conversation = Some("conv-abc".into())),
    );
    v.push(
        msg(20, "inbox", "Duplicate by id", carol(), "2021-06-06", "copy one")
            .with(|h| h.message_id = Some("dup@x".into())),
    );
    v.push(
        msg(21, "projects", "Duplicate by id", carol(), "2021-06-06", "copy one")
            .with(|h| h.message_id = Some("dup@x".into())),
    );
    v.push(msg(22, "inbox", "Content dup", dave(), "2020-02-02", "identical body text"));
    v.push(msg(23, "sent", "Content dup", dave(), "2020-02-02", "identical  body\ntext"));
    for n in 24..=50u32 {
        let from = match n {
            24..=37 => alice(),
            38..=44 => bob(),
            _ => erin(),
        };
        v.push(msg(
            n,
            "inbox",
            &format!("Status update {n}"),
            from,
            &format!("2018-01-{:02}", n - 23),
            &format!("routine status number {n}"),
        ));
    }
    assert_eq!(v.len(), 50);
    v
}

fn corpus_source() -> FakeSource {
    let s = FakeSource::new(corpus());
    s.broken.write().unwrap().insert(16);
    s
}

fn second_source() -> FakeSource {
    FakeSource::new(vec![
        msg(1, "inbox", "Gamma budget plan", dave(), "2022-04-04", "second source budget"),
        msg(2, "inbox", "Delta", dave(), "2022-04-05", "nothing"),
        // Copy of corpus message 20 (same Message-ID) in another source.
        msg(3, "inbox", "Duplicate by id", carol(), "2021-06-06", "copy one")
            .with(|h| h.message_id = Some("dup@x".into())),
    ])
}

struct Fixture {
    _dir: tempfile::TempDir,
    index: MailIndex,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let index = MailIndex::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    index.sync_source(SRC, &corpus_source(), &noop, &cancel).unwrap();
    index.sync_source(SRC2, &second_source(), &noop, &cancel).unwrap();
    Fixture { _dir: dir, index }
}

fn opts_src1() -> SearchOptions {
    SearchOptions { limit: 1000, sources: vec![SRC], ..Default::default() }
}

fn nids(index: &MailIndex, q: &str) -> BTreeSet<u32> {
    let res = index.search(q, &opts_src1()).unwrap_or_else(|e| panic!("{q}: {e}"));
    assert_eq!(res.total as usize, res.hits.len(), "{q}");
    res.hits
        .iter()
        .map(|h| match h.header.locator {
            Some(Locator::Pst { nid }) => nid,
            ref l => panic!("unexpected locator {l:?}"),
        })
        .collect()
}

fn set(v: impl IntoIterator<Item = u32>) -> BTreeSet<u32> {
    v.into_iter().collect()
}

#[test]
fn query_operators() {
    let fx = fixture();
    let ix = &fx.index;
    let alice_ids = set([1, 3, 10].into_iter().chain(24..=37));
    let erin_ids = set([7, 11, 13, 14, 17, 18].into_iter().chain(45..=50));

    let cases: Vec<(&str, BTreeSet<u32>)> = vec![
        ("budget", set([1, 2, 3])),
        ("BUDGET", set([1, 2, 3])),
        ("from:alice", alice_ids.clone()),
        ("from:alice@example.com", alice_ids.clone()),
        ("from:ALICE@Example.com", alice_ids.clone()),
        ("from:example.com", alice_ids.union(&erin_ids).copied().collect()),
        ("from:\"Carol Diaz\"", set([4, 15, 20, 21])),
        ("from:smith", alice_ids.clone()),
        ("to:bob", set((1..=50).filter(|n| *n != 2 && *n != 10))),
        ("cc:bob", set([17])),
        ("to:carol@example.org", set([10, 17])),
        ("subject:budget -subject:re", set([1])),
        ("\"quick brown fox\"", set([13])),
        ("quick brown fox", set([13, 14])),
        ("budget OR offsite", set([1, 2, 3, 4])),
        ("(budget OR offsite) -from:bob", set([1, 3, 4])),
        ("has:attachment", set([1, 6, 7, 8, 11])),
        ("is:unread", set([9])),
        ("is:flagged", set([4])),
        ("is:starred", set([4])),
        ("label:important", set([1])),
        ("label:Important", set([1])),
        ("in:\"sent items\"", set([10, 23])),
        ("in:projects", set([5, 7, 21])),
        ("in:inbox/projects", set([5, 7, 21])),
        ("after:2023-01-01 before:2023-02-01", set([1, 2, 3])),
        ("before:2018-01-03", set([24, 25])),
        ("after:2024-01-01", set([6, 9])),
        ("larger:5M", set([11])),
        ("smaller:5M", set((1..=50).filter(|n| *n != 11))),
        ("attachment:invoice", set([6])),
        ("attachment:98765", set([6])),
        ("98765", set([6])),
        ("nebula", set([7])),
        ("attachment:nebula", set([7])),
        ("body:nebula", set([])),
        ("confidential", set([1])),
        ("reykjavik", set([8])),
        ("attachment:itinerary", set([8])),
        ("offsite barcelona", set([4])),
        ("color", set([])),
        ("agenda", set([5])),
        ("kubernets~", set([12])),
        ("kubernets", set([])),
        ("subject:kubernetis~1", set([12])),
        ("cafe", set([15])),
        ("café", set([15])),
        ("reunion", set([15])),
        ("unreadable", set([16])),
        ("-budget", set((4..=50).collect::<Vec<_>>())),
        ("", set(1..=50)),
        ("!!! ???", set(1..=50)),
        ("subject:status from:bob", set(38..=44)),
        ("status OR budget -from:alice", set([2].into_iter().chain(38..=50))),
    ];
    for (q, want) in cases {
        assert_eq!(nids(ix, q), want, "query `{q}`");
    }

    for bad in ["before:notadate", "is:bogus", "has:wings", "larger:huge"] {
        assert!(ix.search(bad, &opts_src1()).is_err(), "{bad} should be rejected");
    }
    // Pathological nesting is rejected instead of overflowing the stack.
    assert!(ix.search(&"(".repeat(100_000), &opts_src1()).is_err());
    assert!(ix.search(&"-(a ".repeat(10_000), &opts_src1()).is_err());
}

#[test]
fn sorting_paging_snippets_filters() {
    let fx = fixture();
    let ix = &fx.index;
    let first = |sort: Sort, n: usize| -> Vec<u32> {
        let o = SearchOptions { limit: n, sources: vec![SRC], sort: Some(sort), ..Default::default() };
        ix.search("", &o)
            .unwrap()
            .hits
            .iter()
            .map(|h| match h.header.locator {
                Some(Locator::Pst { nid }) => nid,
                _ => 0,
            })
            .collect()
    };
    assert_eq!(first(Sort { key: SortKey::Date, descending: true }, 3), vec![9, 6, 8]);
    assert_eq!(first(Sort { key: SortKey::Date, descending: false }, 2), vec![24, 25]);
    assert_eq!(first(Sort { key: SortKey::Size, descending: true }, 1), vec![11]);
    assert_eq!(first(Sort { key: SortKey::Subject, descending: false }, 1), vec![11]);
    let by_from = first(Sort { key: SortKey::From, descending: false }, 17);
    assert_eq!(set(by_from), set([1, 3, 10].into_iter().chain(24..=37)));

    // Paging is stable and disjoint.
    let page = |offset| {
        let o = SearchOptions {
            offset,
            limit: 10,
            sources: vec![SRC],
            sort: Some(Sort { key: SortKey::Date, descending: false }),
            ..Default::default()
        };
        let r = ix.search("", &o).unwrap();
        assert_eq!(r.total, 50);
        r.hits.into_iter().map(|h| h.header.date.unwrap()).collect::<Vec<_>>()
    };
    let (p0, p1) = (page(0), page(10));
    assert_eq!(p0.len(), 10);
    assert!(p0.windows(2).all(|w| w[0] <= w[1]) && p0.last() <= p1.first());

    // Default limit.
    let r = ix.search("", &SearchOptions::default()).unwrap();
    assert_eq!(r.total, 53);
    assert_eq!(r.hits.len(), 53);

    // Snippets: highlighted body match, subject fallback, escaped preview fallback.
    let hit = |q: &str| ix.search(q, &opts_src1()).unwrap().hits.remove(0);
    assert!(hit("offsite").snippet.contains("<b>offsite</b>"), "{}", hit("offsite").snippet);
    assert!(hit("barcelona").snippet.contains("<b>Barcelona</b>"));
    assert!(hit("kickoff").snippet.contains("<b>Kickoff</b>"));
    assert_eq!(hit("nebula").snippet, "fyi");
    let s = hit("subject:unreadable").snippet;
    assert!(s.contains("<b>Unreadable</b>"), "{s}");

    // Relevance: subject (boost 3) outranks body-only matches.
    let r = ix.search("kubernetes", &opts_src1()).unwrap();
    assert_eq!(r.hits.len(), 1);
    assert!(r.hits[0].score > 0.0);
    let r = ix.search("report", &opts_src1()).unwrap();
    assert_eq!(r.hits[0].header.subject, "Sent report");

    // Source and folder filters.
    let o = SearchOptions { limit: 100, sources: vec![SRC2], ..Default::default() };
    let r = ix.search("budget", &o).unwrap();
    assert_eq!(r.hits.len(), 1);
    assert_eq!(r.hits[0].source, SRC2);
    let o = SearchOptions { limit: 100, ..Default::default() };
    assert_eq!(ix.search("budget", &o).unwrap().total, 4);
    let o = SearchOptions {
        limit: 100,
        sources: vec![SRC],
        folder: Some(FolderId("projects".into())),
        ..Default::default()
    };
    assert_eq!(ix.search("", &o).unwrap().total, 3);
}

#[test]
fn facets() {
    let fx = fixture();
    let o = SearchOptions { limit: 5, facets: true, sources: vec![SRC], ..Default::default() };
    let r = fx.index.search("", &o).unwrap();
    assert_eq!(r.total, 50);
    assert_eq!(r.hits.len(), 5);
    let from: Vec<(&str, u64)> = r.facets_from.iter().map(|f| (f.value.as_str(), f.count)).collect();
    assert_eq!(
        from,
        vec![
            ("alice@example.com", 17),
            ("erin@example.com", 12),
            ("bob@corp.example", 11),
            ("dave@startup.example", 5),
            ("carol@example.org", 4),
            ("billing@vendor.example", 1),
        ]
    );
    let years: Vec<(&str, u64)> = r.facets_year.iter().map(|f| (f.value.as_str(), f.count)).collect();
    assert_eq!(years, vec![("2024", 2), ("2023", 6), ("2022", 5), ("2021", 5), ("2020", 4), ("2019", 1), ("2018", 27)]);
    assert_eq!(r.facets_source, vec![FacetCount { value: "1".into(), count: 50 }]);

    // Facets follow the matched set, across sources.
    let o = SearchOptions { limit: 10, facets: true, ..Default::default() };
    let r = fx.index.search("budget", &o).unwrap();
    assert_eq!(r.total, 4);
    let src: Vec<(&str, u64)> = r.facets_source.iter().map(|f| (f.value.as_str(), f.count)).collect();
    assert_eq!(src, vec![("1", 3), ("2", 1)]);
    assert_eq!(r.facets_from[0], FacetCount { value: "alice@example.com".into(), count: 2 });
    let years: Vec<&str> = r.facets_year.iter().map(|f| f.value.as_str()).collect();
    assert_eq!(years, vec!["2023", "2022"]);

    // Without facets the lists stay empty.
    let r = fx.index.search("budget", &SearchOptions::default()).unwrap();
    assert!(r.facets_from.is_empty() && r.facets_year.is_empty() && r.facets_source.is_empty());
}

#[test]
fn related_messages() {
    let fx = fixture();
    let ix = &fx.index;
    let ids = |v: Vec<(SourceId, Header)>| -> Vec<u32> {
        v.iter()
            .map(|(_, h)| match h.locator {
                Some(Locator::Pst { nid }) => nid,
                _ => 0,
            })
            .collect()
    };
    assert_eq!(ids(ix.related(&["m1@example.com".into()], None).unwrap()), vec![1, 2, 3]);
    assert_eq!(ids(ix.related(&["m2@example.com".into()], None).unwrap()), vec![2, 3]);
    assert_eq!(ids(ix.related(&[], Some("conv-abc")).unwrap()), vec![18, 19]);
    assert_eq!(ids(ix.related(&["nope@x".into()], None).unwrap()), Vec::<u32>::new());
    assert!(ix.related(&[], None).unwrap().is_empty());
    // Cross-source: Message-ID shared by corpus 20/21 and source-2 message 3.
    let rel = ix.related(&["dup@x".into()], None).unwrap();
    assert_eq!(rel.len(), 3);
    assert!(rel.iter().any(|(s, _)| *s == SRC2));
}

#[test]
fn incremental_sync_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let src = corpus_source();
    let events = Mutex::new(Vec::<IndexProgress>::new());
    let record = |p: IndexProgress| events.lock().unwrap().push(p);
    {
        let ix = MailIndex::open(dir.path()).unwrap();
        ix.sync_source(SRC, &src, &record, &cancel).unwrap();
        assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 50);
        assert_eq!(src.calls(), 50);
        let last = events.lock().unwrap().last().cloned().unwrap();
        assert!(last.done);
        assert_eq!((last.indexed, last.total), (50, Some(50)));
        assert_eq!((last.read_errors, last.error.as_deref()), (1, None), "{last:?}");
        let first = events.lock().unwrap()[0].clone();
        assert_eq!(first.total, Some(50), "total estimated from folder counts");

        // Same fingerprint: only the unreadable message (16) is retried, without duplicating it.
        ix.sync_source(SRC, &src, &record, &cancel).unwrap();
        assert_eq!(src.calls(), 51);
        assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 50);
        assert!(events.lock().unwrap().last().unwrap().done);

        // Remove one, add two, new fingerprint: the two new messages (and 16) are read.
        {
            let mut msgs = src.msgs.write().unwrap();
            msgs.retain(|m| m.nid != 13);
            msgs.push(msg(100, "inbox", "Brand new topic", dave(), "2025-01-01", "zebra crossing"));
            msgs.push(msg(101, "inbox", "Another new one", dave(), "2025-01-02", "giraffe"));
        }
        src.touch();
        ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
        assert_eq!(src.calls(), 54);
        assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 51);
        assert_eq!(nids(&ix, "zebra"), set([100]));
        assert_eq!(nids(&ix, "\"quick brown fox\""), set([]));
        assert_eq!(nids(&ix, "fox"), set([14]));

        // Once message 16 reads, the retry completes it and later syncs read nothing.
        src.broken.write().unwrap().clear();
        let body16 = src.msgs.read().unwrap().iter().find(|m| m.nid == 16).unwrap().body.text.clone().unwrap();
        let word = body16.split_whitespace().last().unwrap().to_string();
        ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
        assert_eq!(src.calls(), 55);
        assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 51);
        assert!(nids(&ix, &word).contains(&16), "body of the retried message is indexed");
        ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
        assert_eq!(src.calls(), 55);

        // Fingerprint change without content change: no reads, no doc churn.
        src.touch();
        ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
        assert_eq!(src.calls(), 55);
        assert_eq!(ix.doc_count(None).unwrap(), 51);
    }
    // Reopen from disk: documents and fingerprints persist.
    {
        let ix = MailIndex::open(dir.path()).unwrap();
        assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 51);
        ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
        assert_eq!(src.calls(), 55);
        assert_eq!(nids(&ix, "giraffe"), set([101]));

        ix.remove_source(SRC).unwrap();
        assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 0);
        assert!(ix.search("giraffe", &SearchOptions::default()).unwrap().hits.is_empty());
        // Fingerprint forgotten: the next sync re-reads everything.
        ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
        assert_eq!(src.calls(), 55 + 51);
        assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 51);
    }
    // A meta file from another schema version (or garbage) discards the index.
    std::fs::write(dir.path().join("stv-index.json"), b"{ not json").unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    assert_eq!(ix.doc_count(None).unwrap(), 0);
    ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
    assert_eq!(ix.doc_count(None).unwrap(), 51);
}

#[test]
fn cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let src = corpus_source();

    // Cancelled before starting: nothing indexed.
    let cancel = AtomicBool::new(true);
    assert!(matches!(ix.sync_source(SRC, &src, &noop, &cancel), Err(Error::Cancelled)));
    assert_eq!(ix.doc_count(None).unwrap(), 0);
    assert_eq!(src.calls(), 0);

    // Cancelled mid-way (after the 10th message read): partial work is committed.
    let flag = Arc::new(AtomicBool::new(false));
    *src.cancel_after.write().unwrap() = Some((10, flag.clone()));
    let last = Mutex::new(None);
    let res = ix.sync_source(SRC, &src, &|p| *last.lock().unwrap() = Some(p), &flag);
    assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");
    let p = last.lock().unwrap().clone().unwrap();
    assert!(!p.done);
    assert_eq!(p.error.as_deref(), Some("cancelled"));
    let partial = ix.doc_count(Some(SRC)).unwrap();
    assert!((10..50).contains(&partial), "partial = {partial}");
    let calls_before = src.calls();

    // Resume: only the remaining messages are read.
    *src.cancel_after.write().unwrap() = None;
    flag.store(false, Ordering::SeqCst);
    ix.sync_source(SRC, &src, &noop, &flag).unwrap();
    assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 50);
    assert_eq!(src.calls() - calls_before, 50 - partial as usize);
    assert_eq!(nids(&ix, "nebula"), set([7]));
}

#[test]
fn search_while_another_sync_runs() {
    // MailIndex is Send + Sync and usable from several threads.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MailIndex>();
    let fx = fixture();
    std::thread::scope(|s| {
        let ix = &fx.index;
        let h = s.spawn(move || {
            let src = FakeSource::new(vec![msg(1, "inbox", "Parallel", dave(), "2022-01-01", "concurrent body")]);
            ix.sync_source(SourceId(3), &src, &noop, &AtomicBool::new(false)).unwrap();
        });
        for _ in 0..20 {
            assert_eq!(
                ix.search("budget", &SearchOptions { sources: vec![SRC], ..Default::default() }).unwrap().total,
                3
            );
        }
        h.join().unwrap();
    });
    assert_eq!(fx.index.search("concurrent", &SearchOptions::default()).unwrap().total, 1);
}

#[test]
fn attachment_listing_filters_sorts_and_follows_sync() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut photos = msg(1, "inbox", "Holiday photos", alice(), "2021-01-01", "see attached")
        .attach("beach.JPG", Some("image/jpeg"), vec![0; 5000])
        .attach("image001.png", Some("image/png"), vec![0; 800]);
    photos.attachments[1].0.inline = true;
    let src = FakeSource::new(vec![
        photos,
        msg(2, "inbox", "Q3 numbers", bob(), "2022-02-02", "figures")
            .attach("report.pdf", Some("application/pdf"), vec![0; 20_000])
            .attach("data.xlsx", None, vec![0; 9_000]),
        msg(3, "inbox", "Invite", carol(), "2023-03-03", "calendar").attach(
            "invite.ics",
            Some("text/calendar"),
            vec![0; 300],
        ),
    ]);
    ix.sync_source(SRC, &src, &noop, &cancel).unwrap();

    let q = |f: &dyn Fn(&mut AttachmentQuery)| {
        let mut q = AttachmentQuery { sources: vec![SRC], ..Default::default() };
        f(&mut q);
        ix.attachments(&q).unwrap()
    };
    let names = |p: &AttachmentPage| p.rows.iter().map(|r| r.filename.clone()).collect::<Vec<_>>();
    let newest_first = AttachmentSort { key: AttachmentSortKey::Date, descending: true };

    // Inline parts are hidden by default; date sort follows the carrying message.
    let all = q(&|q| q.sort = newest_first);
    assert_eq!(all.total, 4);
    assert_eq!(names(&all)[0], "invite.ics");
    assert_eq!(names(&all)[3], "beach.JPG");
    assert_eq!(q(&|q| q.include_inline = true).total, 5);
    use AttachmentCategory as C;
    assert_eq!(all.category_counts, [(C::Image, 1), (C::Pdf, 1), (C::Spreadsheet, 1), (C::Calendar, 1)]);

    // Row content.
    let beach = all.rows.iter().find(|r| r.filename == "beach.JPG").unwrap();
    assert_eq!(beach.extension, "jpg");
    assert_eq!(beach.category, AttachmentCategory::Image);
    assert_eq!(beach.message, Locator::Pst { nid: 1 });
    assert_eq!(beach.index, 0);
    assert_eq!(beach.size, 5000);
    assert_eq!(beach.subject, "Holiday photos");
    assert_eq!(beach.from.as_ref().and_then(|a| a.email.as_deref()), Some("alice@example.com"));
    let data = all.rows.iter().find(|r| r.filename == "data.xlsx").unwrap();
    assert_eq!(data.category, AttachmentCategory::Spreadsheet, "classified by extension without a MIME type");

    // Category include-list, with chip counts that ignore it.
    let images = q(&|q| q.categories = vec![AttachmentCategory::Image]);
    assert_eq!(names(&images), ["beach.JPG"]);
    let counts: std::collections::HashMap<_, _> = images.category_counts.iter().copied().collect();
    assert_eq!(counts.get(&AttachmentCategory::Pdf), Some(&1));
    assert_eq!(counts.get(&AttachmentCategory::Calendar), Some(&1));
    assert_eq!(counts.get(&AttachmentCategory::Image), Some(&1));

    // Exclusions.
    assert_eq!(q(&|q| q.exclude_categories = vec![AttachmentCategory::Pdf]).total, 3);
    assert_eq!(q(&|q| q.exclude_extensions = vec![".ICS".into()]).total, 3);
    let no_logo = q(&|q| {
        q.include_inline = true;
        q.text = "-image0".into();
    });
    assert_eq!(no_logo.total, 4, "`-word` prefix-excludes image001.png");

    // Size range and size sort.
    let big = q(&|q| {
        q.min_size = Some(8_000);
        q.sort = AttachmentSort { key: AttachmentSortKey::Size, descending: true };
    });
    assert_eq!(names(&big), ["report.pdf", "data.xlsx"]);
    assert_eq!(q(&|q| q.max_size = Some(1_000)).total, 1);

    // Text: prefix of a file name, or the sender of the carrying message.
    assert_eq!(names(&q(&|q| q.text = "rep".into())), ["report.pdf"]);
    assert_eq!(q(&|q| q.text = "bob".into()).total, 2);

    // Paging.
    let page = q(&|q| {
        q.sort = newest_first;
        q.offset = 1;
        q.limit = 2;
    });
    assert_eq!(page.total, 4);
    assert_eq!(page.rows.len(), 2);

    // Rows follow incremental sync and source removal.
    src.msgs.write().unwrap().retain(|m| m.nid != 2);
    src.touch();
    ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
    assert_eq!(q(&|_| {}).total, 2);
    ix.remove_source(SRC).unwrap();
    assert_eq!(q(&|_| {}).total, 0);
}

#[test]
fn phrases_span_dropped_long_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let long = "x".repeat(80);
    let body = format!("alpha {long} omega");
    let src = FakeSource::new(vec![msg(1, "inbox", "Tokens", dave(), "2022-01-01", &body)]);
    ix.sync_source(SRC, &src, &noop, &AtomicBool::new(false)).unwrap();
    // The over-long token is not indexed but keeps its position, in documents and queries alike.
    assert_eq!(nids(&ix, &format!("\"alpha {long} omega\"")), set([1]));
    assert_eq!(nids(&ix, "\"alpha omega\""), set([]));
}

#[test]
fn pages_beyond_the_index_are_empty_not_fatal() {
    let fx = fixture();
    let ix = &fx.index;
    for (offset, limit, hits) in [(usize::MAX / 2, 10, 0), (48, usize::MAX, 2), (usize::MAX, usize::MAX, 0)] {
        let o = SearchOptions { offset, limit, sources: vec![SRC], ..Default::default() };
        let r = ix.search("", &o).unwrap();
        assert_eq!((r.total, r.hits.len()), (50, hits), "offset {offset}, limit {limit}");
        let q = AttachmentQuery { offset, limit, include_inline: true, ..Default::default() };
        let p = ix.attachments(&q).unwrap();
        assert!(p.rows.len() <= p.total as usize);
    }
}

#[test]
fn a_message_listed_twice_is_indexed_once() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let first = msg(1, "inbox", "Listed twice", alice(), "2022-01-01", "same message in two folders");
    let second = first.clone().with(|h| h.folder = Some(FolderId("projects".into())));
    let src = FakeSource::new(vec![first, second, msg(2, "inbox", "Once", bob(), "2022-01-02", "single")]);
    ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
    assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 2);
    // A resync after a fingerprint change must neither duplicate nor re-read it.
    src.touch();
    ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
    assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 2);
    assert_eq!(src.calls(), 2);
}

#[test]
fn a_panicking_reader_costs_one_message_not_the_sync() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let src = FakeSource::new(vec![
        msg(1, "inbox", "Fine", alice(), "2022-01-01", "readable"),
        msg(2, "inbox", "Malformed", bob(), "2022-01-02", "hidden body"),
        msg(3, "inbox", "Also fine", carol(), "2022-01-03", "readable too"),
    ]);
    src.panicking.write().unwrap().insert(2);
    let last = Mutex::new(None);
    ix.sync_source(SRC, &src, &|p| *last.lock().unwrap() = Some(p), &cancel).unwrap();
    let p = last.lock().unwrap().clone().unwrap();
    assert!(p.done && p.read_errors == 1 && p.error.is_none(), "{p:?}");
    // Indexed from its listing header, and retried on the next sync.
    assert_eq!(nids(&ix, "malformed"), set([2]));
    assert_eq!(nids(&ix, "hidden"), set([]));
    src.panicking.write().unwrap().clear();
    ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
    assert_eq!(nids(&ix, "hidden"), set([2]));
    assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 3);
}

#[test]
fn a_panicking_listing_fails_the_sync_and_is_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let cancel = AtomicBool::new(false);
    let src = FakeSource::new((1..=3).map(|n| msg(n, "inbox", "Item", dave(), "2022-01-01", "body")).collect());
    *src.listing_panics_after.write().unwrap() = Some(1);
    assert!(ix.sync_source(SRC, &src, &noop, &cancel).is_err());
    // The fingerprint was not stored: the next sync lists everything again.
    *src.listing_panics_after.write().unwrap() = None;
    ix.sync_source(SRC, &src, &noop, &cancel).unwrap();
    assert_eq!(ix.doc_count(Some(SRC)).unwrap(), 3);
}

#[test]
fn attachment_type_sort_orders_by_extension_then_name() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    let src = FakeSource::new(vec![msg(1, "inbox", "Files", alice(), "2022-01-01", "files")
        .attach("b.pdf", None, vec![0; 10])
        .attach("A.pdf", None, vec![0; 10])
        .attach("z.doc", None, vec![0; 10])]);
    ix.sync_source(SRC, &src, &noop, &AtomicBool::new(false)).unwrap();
    let q = AttachmentQuery {
        sort: AttachmentSort { key: AttachmentSortKey::Type, descending: false },
        ..Default::default()
    };
    let names: Vec<String> = ix.attachments(&q).unwrap().rows.into_iter().map(|r| r.filename).collect();
    assert_eq!(names, ["z.doc", "A.pdf", "b.pdf"]);
}

/// Calendar, contact and task items with typed details, plus one plain mail.
fn items_source() -> FakeSource {
    let olivia = addr("Olivia Organizer", "olivia@corp.example");
    let listed = Appointment {
        start: Some(When::Time(day("2024-05-10"))),
        end: Some(When::Time(day("2024-05-10") + 3_600_000)),
        location: Some("Board Room 4B".into()),
        organizer: Some(olivia.clone()),
        recurring: true,
        recurrence: Some("Occurs every Friday".into()),
        ..Default::default()
    };
    // The full message adds the attendees (list rows carry table columns only).
    let full = Appointment {
        required: vec![addr("Rita Required", "rita@corp.example")],
        optional: vec![addr("Otto Optional", "")],
        ..listed.clone()
    };
    let mut appt = msg(1, "inbox", "Strategy review", olivia.clone(), "2020-03-01", "agenda to follow")
        .with(|h| h.item = Some(ItemDetails::Appointment(listed)));
    appt.full_item = Some(ItemDetails::Appointment(full));
    let contact = Contact {
        display_name: Some("Connie Contact".into()),
        company: Some("Acme Widgets".into()),
        job_title: Some("Chief Economist".into()),
        department: Some("Research".into()),
        emails: vec![addr("Connie Contact", "connie@acme.example")],
        phones: vec![Labeled { label: "business".into(), value: "(555) 010-1234".into() }],
        addresses: vec![Labeled { label: "business".into(), value: "1 Example Way\nSpringfield, ZZ 00000".into() }],
        web_page: Some("https://widgets.example/research".into()),
        ..Default::default()
    };
    FakeSource::new(vec![
        appt,
        // Meeting request for an all-day event.
        msg(2, "inbox", "Offsite", olivia.clone(), "2021-01-01", "see you there").with(|h| {
            h.message_class = Some("IPM.Schedule.Meeting.Request".into());
            h.item = Some(ItemDetails::Appointment(Appointment {
                start: Some(When::Date("2021-02-01".into())),
                end: Some(When::Date("2021-02-02".into())),
                all_day: true,
                location: Some("Seaside Lodge".into()),
                meeting: Some(MeetingKind::Request),
                ..Default::default()
            }));
        }),
        msg(3, "inbox", "Connie Contact", dave(), "2019-01-01", "met at the conference").with(|h| {
            h.message_class = Some("IPM.Contact".into());
            h.item = Some(ItemDetails::Contact(contact));
        }),
        // Task created in 2018, due mid-2023.
        msg(4, "inbox", "File the report", dave(), "2018-01-01", "").with(|h| {
            h.message_class = Some("IPM.Task".into());
            h.item = Some(ItemDetails::Task(Task {
                due: Some(When::Date("2023-06-30".into())),
                owner: Some("Tom Taskowner".into()),
                ..Default::default()
            }));
        }),
        msg(5, "inbox", "Lunch", erin(), "2022-01-01", "sandwiches"),
        // A contact whose details could not be decoded still counts as a contact.
        msg(6, "inbox", "Someone", dave(), "2019-02-02", "")
            .with(|h| h.message_class = Some("IPM.Contact.Custom".into())),
    ])
}

#[test]
fn item_details_are_searchable_and_dated_by_start() {
    let dir = tempfile::tempdir().unwrap();
    let ix = MailIndex::open(dir.path()).unwrap();
    ix.sync_source(SRC, &items_source(), &noop, &AtomicBool::new(false)).unwrap();

    let cases: Vec<(&str, BTreeSet<u32>)> = vec![
        ("is:appointment", set([1, 2])),
        ("is:meeting", set([2])),
        ("is:contact", set([3, 6])),
        ("is:Task", set([4])),
        ("-is:appointment -is:contact -is:task", set([5])),
        // Item details: bare words, phrases, phone numbers in any punctuation.
        ("\"board room\"", set([1])),
        ("friday", set([1])),
        ("seaside", set([2])),
        ("acme widgets", set([3])),
        ("\"chief economist\" research", set([3])),
        ("555-010-1234", set([3])),
        ("5550101234", set([3])),
        ("springfield", set([3])),
        ("taskowner", set([4])),
        ("body:seaside", set([])),
        // Participants as sender/recipients; attendees come from the full message.
        ("from:\"olivia organizer\"", set([1, 2])),
        ("to:rita", set([1])),
        ("to:rita@corp.example", set([1])),
        ("to:otto", set([1])),
        ("otto", set([1])),
        ("to:connie@acme.example", set([3])),
        // Dates are the appointment start / task due date.
        ("after:2024-01-01", set([1])),
        ("before:2020-06-01", set([3, 6])),
        ("after:2023-06-30 before:2023-07-01", set([4])),
        ("after:2021-02-01 before:2021-02-02", set([2])),
    ];
    for (q, want) in cases {
        assert_eq!(nids(&ix, q), want, "query `{q}`");
    }

    let order = |key| -> Vec<u32> {
        let o = SearchOptions { sources: vec![SRC], sort: Some(Sort { key, descending: true }), ..Default::default() };
        ix.search("", &o)
            .unwrap()
            .hits
            .iter()
            .map(|h| match h.header.locator {
                Some(Locator::Pst { nid }) => nid,
                _ => 0,
            })
            .collect()
    };
    assert_eq!(order(SortKey::Start), [1, 4, 5, 2, 6, 3]);
    // The date sort stays the message date, as in folder listings.
    assert_eq!(order(SortKey::Date), [5, 2, 1, 6, 3, 4]);

    let o = SearchOptions { facets: true, sources: vec![SRC], ..Default::default() };
    let r = ix.search("", &o).unwrap();
    let years: Vec<(&str, u64)> = r.facets_year.iter().map(|f| (f.value.as_str(), f.count)).collect();
    assert_eq!(years, [("2024", 1), ("2023", 1), ("2022", 1), ("2021", 1), ("2019", 2)]);
    // Hits carry the details, including what only the full message has.
    let hit = ix.search("strategy", &o).unwrap().hits.remove(0);
    let Some(ItemDetails::Appointment(a)) = hit.header.item else { panic!("{:?}", hit.header.item) };
    assert_eq!(a.required, [addr("Rita Required", "rita@corp.example")]);
    assert_eq!(hit.header.date, Some(day("2020-03-01")));
}
