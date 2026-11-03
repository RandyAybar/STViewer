//! End-to-end tests on PST/OST files produced by the test writer in `common`, covering every
//! format (ANSI, Unicode, Unicode-4K), every encoding, 4K zlib compression, multi-level
//! B-trees, XBLOCK data trees, SIBLOCK subnode trees, multi-block heaps and row matrices,
//! embedded messages, and corruption handling.

mod common;

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;

use common::*;
use mail_model::{AttachmentKind, Error, FolderKind, Locator, MailSource, Sort, SortKey, SourceKind};
use stv_pst::crypt::{NDB_CRYPT_CYCLIC, NDB_CRYPT_NONE, NDB_CRYPT_PERMUTE};
use stv_pst::ndb::{Backing, Format, Ndb};
use stv_pst::store::PstFile;
use stv_pst::PstSource;

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("stv-pst-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}-{}", std::process::id()))
}

fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
    let p = temp_path(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

struct Config {
    name: &'static str,
    writer: fn() -> Writer,
    ext: &'static str,
    client: &'static [u8; 2],
}

fn configs() -> Vec<Config> {
    vec![
        Config {
            name: "ansi-none",
            writer: || Writer::new(Format::Ansi, NDB_CRYPT_NONE, false),
            ext: "pst",
            client: b"SM",
        },
        Config {
            name: "ansi-permute-small-pages",
            writer: || {
                let mut w = Writer::new(Format::Ansi, NDB_CRYPT_PERMUTE, false);
                w.page_entries = Some(3);
                w.sl_max = Some(2);
                w
            },
            ext: "pst",
            client: b"SM",
        },
        Config {
            name: "unicode-permute",
            writer: || Writer::new(Format::Unicode, NDB_CRYPT_PERMUTE, false),
            ext: "pst",
            client: b"SM",
        },
        Config {
            name: "unicode-cyclic-multiblock-heaps",
            writer: || {
                let mut w = Writer::new(Format::Unicode, NDB_CRYPT_CYCLIC, false);
                w.heap_cap = 700;
                w.page_entries = Some(4);
                w.sl_max = Some(3);
                w
            },
            ext: "pst",
            client: b"SM",
        },
        Config {
            name: "4k-none-zlib",
            writer: || Writer::new(Format::Unicode4k, NDB_CRYPT_NONE, true),
            ext: "ost",
            client: b"SO",
        },
        Config {
            name: "4k-permute-zlib-small-pages",
            writer: || {
                let mut w = Writer::new(Format::Unicode4k, NDB_CRYPT_PERMUTE, true);
                w.page_entries = Some(5);
                w.heap_cap = 2000;
                w
            },
            ext: "ost",
            client: b"SO",
        },
        Config {
            name: "4k-cyclic-uncompressed",
            writer: || Writer::new(Format::Unicode4k, NDB_CRYPT_CYCLIC, false),
            ext: "ost",
            client: b"SO",
        },
    ]
}

const BULK: usize = 300;

fn names(v: &[mail_model::Address]) -> Vec<String> {
    v.iter().filter_map(|a| a.name.clone()).collect()
}
fn emails(v: &[mail_model::Address]) -> Vec<String> {
    v.iter().filter_map(|a| a.email.clone()).collect()
}

/// Appointments: list rows carry start and location from the contents table, the full
/// message adds attendees, and Start orders by start rather than by creation. The task's
/// details are no table columns: its row is completed from the message.
fn check_calendar(src: &dyn MailSource, cal: &mail_model::FolderId, fx: &Fixture, ctx: &str) {
    use mail_model::{ItemDetails, When};
    let appt = |h: &mail_model::Header| match &h.item {
        Some(ItemDetails::Appointment(a)) => a.clone(),
        other => panic!("{ctx}: {:?} has no appointment: {other:?}", h.subject),
    };
    let subjects = |hs: &[mail_model::Header]| hs.iter().map(|h| h.subject.clone()).collect::<Vec<_>>();
    let by = |key, descending| Sort { key, descending };
    let rows = src.list(cal, 0..10, by(SortKey::Date, false)).unwrap();
    assert_eq!(subjects(&rows), ["Board meeting", "Dentist", "File report"], "{ctx}");
    let row = appt(&rows[0]);
    assert_eq!(row.start, Some(When::Time(T0 + 30 * 86_400_000)), "{ctx}: start from the table row");
    assert_eq!(row.location.as_deref(), Some("Room 4C"), "{ctx}");
    assert!(row.required.is_empty(), "{ctx}: rows hold no recipients");
    let due = When::Date(ymd(task_due()));
    match &rows[2].item {
        Some(ItemDetails::Task(t)) => {
            assert_eq!(t.due.as_ref(), Some(&due), "{ctx}: due date from the message");
            assert_eq!(t.owner.as_deref(), Some("Terry Task"), "{ctx}");
        }
        other => panic!("{ctx}: task row has {other:?}"),
    }

    let by_start = src.list(cal, 0..10, by(SortKey::Start, false)).unwrap();
    assert_eq!(subjects(&by_start), ["Dentist", "File report", "Board meeting"], "{ctx}");
    assert!(by_start.windows(2).all(|w| w[0].start_ms() <= w[1].start_ms()), "{ctx}: key is Header::start_ms");

    let full = appt(&src.message(rows[0].locator.as_ref().unwrap()).unwrap().header);
    assert_eq!(full.end, Some(When::Time(T0 + 30 * 86_400_000 + 3_600_000)), "{ctx}");
    assert_eq!(names(&full.required), ["Bob Jones"], "{ctx}");
    assert_eq!(names(&full.optional), ["Carol"], "{ctx}");
    assert_eq!(fx.calendar.len(), 3);
}

/// `YYYY-MM-DD` of a UTC ms timestamp.
fn ymd(ms: i64) -> String {
    let (y, m, d) = mail_model::time::civil_from_days(ms.div_euclid(mail_model::time::MS_PER_DAY));
    format!("{y:04}-{m:02}-{d:02}")
}

fn check_store(src: &dyn MailSource, fx: &Fixture, ctx: &str) {
    let folders = src.folders().unwrap();
    let fnames: Vec<_> = folders.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(fnames, ["Synthetic Store", "Inbox", "Bulk", "Calendar"], "{ctx}: search root pruned, IPM first");
    let root = &folders[0];
    let inbox = &folders[1];
    let bulk = &folders[2];
    assert_eq!(root.parent, None);
    assert_eq!(inbox.parent.as_ref(), Some(&root.id));
    assert_eq!(bulk.parent.as_ref(), Some(&inbox.id));
    assert_eq!(folders[3].kind, FolderKind::Calendar);
    assert_eq!(inbox.kind, FolderKind::Mail);
    assert_eq!(inbox.message_count, Some(2));
    assert_eq!(inbox.unread_count, Some(1));
    assert_eq!(inbox.id.0, "nid:8042");

    assert_eq!(src.count(&inbox.id).unwrap(), 2);
    assert_eq!(src.count(&bulk.id).unwrap(), fx.bulk.len() as u64);
    assert_eq!(src.count(&folders[3].id).unwrap(), fx.calendar.len() as u64);
    check_calendar(src, &folders[3].id, fx, ctx);
    assert_eq!(src.count(&root.id).unwrap(), 0);

    // Inbox list from the contents table.
    let by_date = |desc| Sort { key: SortKey::Date, descending: desc };
    let hs = src.list(&inbox.id, 0..10, by_date(true)).unwrap();
    assert_eq!(hs.iter().map(|h| h.subject.as_str()).collect::<Vec<_>>(), ["Quarterly report", "RE: Lunch?"], "{ctx}");
    let h = &hs[0];
    assert_eq!(h.locator, Some(Locator::Pst { nid: 0x200004 }));
    assert_eq!(h.folder.as_ref(), Some(&inbox.id));
    assert_eq!(h.from.as_ref().and_then(|a| a.name.as_deref()), Some("Alice Smith"));
    assert_eq!(names(&h.to), ["Bob Jones", "Carol"]);
    assert_eq!(names(&h.cc), ["Dave"]);
    assert_eq!(h.date, Some(T0 + 3_600_000));
    assert_eq!(h.size, 123_456);
    assert!(!h.flags.read && h.has_attachments);
    assert_eq!(h.labels, ["Red", "Finance"], "{ctx}: named Keywords column resolved through the name map");
    assert_eq!(h.message_class.as_deref(), Some("IPM.Note"));
    let asc = src.list(&inbox.id, 0..10, by_date(false)).unwrap();
    assert_eq!(asc[0].subject, "RE: Lunch?");

    // Bulk: sorting and paging over a row matrix spanning several blocks.
    let all_native = src.list(&bulk.id, 0..10_000, Sort { key: SortKey::Native, descending: false }).unwrap();
    assert_eq!(all_native.len(), fx.bulk.len());
    for (i, h) in all_native.iter().enumerate() {
        assert_eq!(h.locator, Some(Locator::Pst { nid: fx.bulk[i].nid }), "{ctx}: native order");
        assert_eq!(h.subject, fx.bulk[i].subject);
    }
    let dates = src.list(&bulk.id, 0..fx.bulk.len() as u64, by_date(false)).unwrap();
    assert!(dates.windows(2).all(|w| w[0].date <= w[1].date), "{ctx}: date asc");
    let mut expect: Vec<_> = fx.bulk.iter().map(|m| m.date).collect();
    expect.sort();
    assert_eq!(dates.iter().map(|h| h.date.unwrap()).collect::<Vec<_>>(), expect);
    let n = fx.bulk.len() as u64;
    let (a, b) = (n / 3, n / 3 + 17);
    let page = src.list(&bulk.id, a..b, by_date(false)).unwrap();
    assert_eq!(page, dates[a as usize..b as usize].to_vec(), "{ctx}: page equals slice of full listing");
    let desc = src.list(&bulk.id, 0..fx.bulk.len() as u64, by_date(true)).unwrap();
    assert!(desc.windows(2).all(|w| w[0].date >= w[1].date));
    let sizes = src.list(&bulk.id, 0..fx.bulk.len() as u64, Sort { key: SortKey::Size, descending: true }).unwrap();
    assert!(sizes.windows(2).all(|w| w[0].size >= w[1].size));
    let from = src.list(&bulk.id, 0..fx.bulk.len() as u64, Sort { key: SortKey::From, descending: false }).unwrap();
    let fk: Vec<String> = from.iter().map(|h| h.from.as_ref().unwrap().name.clone().unwrap().to_lowercase()).collect();
    assert!(fk.windows(2).all(|w| w[0] <= w[1]), "{ctx}: from asc");
    let subj = src.list(&inbox.id, 0..2, Sort { key: SortKey::Subject, descending: false }).unwrap();
    assert_eq!(subj[0].subject, "RE: Lunch?", "{ctx}: subject sort ignores RE:");
    assert!(src.list(&bulk.id, n + 100..n + 200, by_date(false)).unwrap().is_empty());
    assert_eq!(src.list(&bulk.id, n - 10..n + 100, by_date(false)).unwrap().len(), 10);

    // iter_headers covers every folder.
    let all: Vec<_> = src.iter_headers().collect::<Result<_, _>>().unwrap();
    assert_eq!(all.len(), fx.bulk.len() + fx.inbox.len() + fx.calendar.len());
    assert!(all.iter().all(|h| h.locator.is_some() && h.folder.is_some()));

    // Full message.
    let loc = Locator::Pst { nid: 0x200004 };
    let m = src.message(&loc).unwrap();
    assert_eq!(m.header.subject, "Quarterly report");
    assert_eq!(m.header.from.as_ref().unwrap().email.as_deref(), Some("alice@example.com"));
    assert_eq!(emails(&m.header.to), ["bob@example.com", "carol@example.com"], "{ctx}: recipients table");
    assert_eq!(emails(&m.header.cc), ["dave@example.com"]);
    assert_eq!(m.header.message_id.as_deref(), Some(format!("{}@test", 0x200004).as_str()));
    assert_eq!(m.header.folder.as_ref(), Some(&inbox.id));
    assert_eq!(m.body.text.as_deref(), Some("See attached report. ".repeat(400).as_str()), "{ctx}: subnode body");
    assert_eq!(m.body.html.as_deref(), Some("<html><body><p>See attached café</p></body></html>"));
    let props = src.properties(&loc).unwrap();
    assert!(props.iter().any(|p| p.name == "PidTagSubject" && p.value == "Quarterly report"));
    assert!(props.iter().any(|p| p.name == "PidNameKeywords"));
    let a = &m.attachments;
    assert_eq!(a.len(), 3);
    assert_eq!((a[0].index, a[0].filename.as_deref(), a[0].size), (0, Some("report.bin"), 150_000));
    assert_eq!(a[0].mime_type.as_deref(), Some("application/octet-stream"));
    assert_eq!((a[1].filename.as_deref(), a[1].kind), (Some("note.txt"), AttachmentKind::File));
    assert_eq!((a[2].filename.as_deref(), a[2].kind), (Some("Forwarded inner"), AttachmentKind::EmbeddedMessage));
    assert_eq!(src.attachment(&loc, 0).unwrap(), big_attachment(), "{ctx}: XBLOCK attachment payload");
    assert_eq!(src.attachment(&loc, 1).unwrap(), b"hello attachment");
    assert!(matches!(src.attachment(&loc, 2), Err(Error::Unsupported(_))));
    assert!(matches!(src.attachment(&loc, 9), Err(Error::NotFound(_))));
    assert_eq!(src.raw_mime(&loc).unwrap(), None);

    // Embedded messages, two levels deep.
    let e1 = Locator::Embedded { parent: Box::new(loc.clone()), attachment: 2 };
    let m1 = src.message(&e1).unwrap();
    assert_eq!(m1.header.subject, "Forwarded inner");
    assert!(src.properties(&e1).unwrap().iter().any(|p| p.name == "PidTagSubject" && p.value == "Forwarded inner"));
    assert_eq!(m1.header.from.as_ref().unwrap().name.as_deref(), Some("Inner Sender"));
    assert_eq!(m1.body.text.as_deref(), Some("inner body"));
    assert_eq!(emails(&m1.header.to), ["bob@example.com"]);
    assert_eq!(m1.attachments[0].kind, AttachmentKind::EmbeddedMessage);
    let e2 = Locator::Embedded { parent: Box::new(e1.clone()), attachment: 0 };
    let m2 = src.message(&e2).unwrap();
    assert_eq!(m2.header.subject, "Level 2");
    assert_eq!(src.attachment(&e2, 0).unwrap(), b"deep");
    // A file attachment is not a native embedded message.
    let bad = Locator::Embedded { parent: Box::new(loc.clone()), attachment: 1 };
    assert!(matches!(src.message(&bad), Err(Error::Unsupported(_))));
    assert!(matches!(src.message(&Locator::Pst { nid: 0xDEAD4 }), Err(Error::NotFound(_))));
    assert!(matches!(src.message(&Locator::File { path: "x.eml".into() }), Err(Error::Unsupported(_))));
    assert!(src.list(&mail_model::FolderId("nid:zz".into()), 0..1, by_date(false)).is_err());
}

#[test]
fn synthetic_stores_all_formats() {
    let fx = fixture(BULK);
    for c in configs() {
        let bytes = build_store((c.writer)(), &fx, c.client);
        let path = write_temp(&format!("{}.{}", c.name, c.ext), &bytes);
        let expect_kind = if c.ext == "ost" { SourceKind::Ost } else { SourceKind::Pst };
        assert_eq!(stv_pst::opener().sniff(&path), Some(expect_kind), "{}", c.name);
        {
            let src = stv_pst::opener().open(&path).unwrap();
            assert_eq!(src.kind(), expect_kind);
            check_store(&*src, &fx, c.name);
            let fp = src.fingerprint().unwrap();
            assert_eq!(fp.size, bytes.len() as u64);
        }
        std::fs::remove_file(&path).ok();
    }
}

/// Small synthetic files committed under `testdata/pst/` (inputs for the `dump` example and a
/// stable regression corpus). Regenerate with
/// `cargo test -p stv-pst --test synthetic -- --ignored regenerate_committed_fixtures`.
const COMMITTED_BULK: usize = 40;

fn committed() -> Vec<(&'static str, Writer, &'static [u8; 2])> {
    vec![
        ("synthetic-ansi-permute.pst", Writer::new(Format::Ansi, NDB_CRYPT_PERMUTE, false), b"SM"),
        ("synthetic-unicode-cyclic.pst", Writer::new(Format::Unicode, NDB_CRYPT_CYCLIC, false), b"SM"),
        ("synthetic-4k-zlib.ost", Writer::new(Format::Unicode4k, NDB_CRYPT_NONE, true), b"SO"),
    ]
}

fn committed_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/pst")
}

#[test]
#[ignore]
fn regenerate_committed_fixtures() {
    let fx = fixture(COMMITTED_BULK);
    for (name, w, client) in committed() {
        std::fs::write(committed_dir().join(name), build_store(w, &fx, client)).unwrap();
    }
}

#[test]
fn committed_fixtures_read_back() {
    let fx = fixture(COMMITTED_BULK);
    for (name, _, _) in committed() {
        let path = committed_dir().join(name);
        let src = PstSource::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        check_store(&src, &fx, name);
    }
}

#[test]
fn sniff_rejects_non_pst() {
    let p = write_temp("not-a-pst.pst", b"From someone\r\nSubject: x\r\n\r\nbody");
    assert_eq!(stv_pst::opener().sniff(&p), None);
    assert!(matches!(stv_pst::opener().open(&p), Err(Error::Format(_))));
    std::fs::remove_file(&p).ok();
    assert_eq!(stv_pst::opener().sniff(&std::env::temp_dir()), None);
}

#[test]
fn four_k_blocks_are_really_compressed() {
    let fx = fixture(20);
    let bytes = build_store(Writer::new(Format::Unicode4k, NDB_CRYPT_NONE, true), &fx, b"SO");
    let path = write_temp("4k-compressed-check.ost", &bytes);
    let src = PstSource::open(&path).unwrap();
    let ndb = &src.file().ndb;
    let compressed = ndb
        .all_blocks()
        .unwrap()
        .iter()
        .filter(|b| {
            let alloc = Format::Unicode4k.block_alloc_size(b.cb as usize);
            let t = b.ib as usize + alloc - 24;
            let inflated = u16::from_le_bytes([bytes[t + 18], bytes[t + 19]]);
            inflated as usize != b.cb as usize
        })
        .count();
    assert!(compressed > 10, "only {compressed} compressed blocks");
    // Every block decodes.
    for b in ndb.all_blocks().unwrap() {
        ndb.block(b.bid).unwrap();
    }
    drop(src);
    std::fs::remove_file(&path).ok();
}

#[test]
fn btree_paging_multi_level() {
    for fmt in [Format::Ansi, Format::Unicode, Format::Unicode4k] {
        let mut w = Writer::new(fmt, NDB_CRYPT_NONE, false);
        w.page_entries = Some(3);
        let fx = fixture(60);
        let bytes = build_store(w, &fx, b"SM");
        let path = write_temp(&format!("btree-{fmt:?}.pst"), &bytes);
        let src = PstSource::open(&path).unwrap();
        let ndb = &src.file().ndb;
        let nodes = ndb.all_nodes().unwrap();
        assert!(nodes.len() > 60 && nodes.windows(2).all(|w| w[0].nid < w[1].nid), "{fmt:?}: sorted leaves");
        for n in &nodes {
            assert_eq!(ndb.node(n.nid).unwrap(), *n, "{fmt:?}: lookup 0x{:X}", n.nid);
            // Keys between and around existing ones are absent.
            if !nodes.iter().any(|m| m.nid == n.nid + 1) {
                assert!(matches!(ndb.node(n.nid + 1), Err(Error::NotFound(_))));
            }
        }
        assert!(matches!(ndb.node(0), Err(Error::NotFound(_))));
        assert!(matches!(ndb.node(u32::MAX), Err(Error::NotFound(_))));
        let blocks = ndb.all_blocks().unwrap();
        for b in &blocks {
            assert_eq!(ndb.block_entry(b.bid).unwrap(), *b);
            // Reserved low bit is ignored on lookup.
            assert_eq!(ndb.block_entry(b.bid | 1).unwrap(), *b);
        }
        drop(src);
        std::fs::remove_file(&path).ok();
    }
}

/// Some eDiscovery exports write B-tree pages out of key order. Binary search then
/// misses entries a walk still finds; every node, block and message must stay reachable.
#[test]
fn out_of_order_btree_pages_stay_readable() {
    let mut w = Writer::new(Format::Unicode, NDB_CRYPT_NONE, false);
    w.page_entries = Some(3);
    let fx = fixture(40);
    let mut bytes = build_store(w, &fx, b"SM");
    // Unicode header: NBT root page at 224, BBT root page at 240. Swap the first and last
    // entries of each root (an intermediate page: count @488, entry size @490, level @491).
    for at in [224usize, 240] {
        let ib = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
        let page = &mut bytes[ib..ib + 512];
        let (count, size, level) = (page[488] as usize, page[490] as usize, page[491]);
        assert!(level > 0 && count >= 3, "fixture must have a multi-level tree");
        let (first, last) = (0..size, (count - 1) * size..count * size);
        let tmp = page[first.clone()].to_vec();
        page.copy_within(last.clone(), first.start);
        page[last].copy_from_slice(&tmp);
    }
    let path = write_temp("out-of-order-btree.pst", &bytes);
    let src = PstSource::open(&path).unwrap();
    let ndb = &src.file().ndb;
    let nodes = ndb.all_nodes().unwrap();
    assert!(nodes.windows(2).any(|w| w[0].nid > w[1].nid), "tree really is out of order");
    for n in &nodes {
        assert_eq!(ndb.node(n.nid).unwrap(), *n, "lookup 0x{:X}", n.nid);
    }
    for b in ndb.all_blocks().unwrap() {
        assert_eq!(ndb.block_entry(b.bid).unwrap(), b);
    }
    assert!(matches!(ndb.node(u32::MAX), Err(Error::NotFound(_))));
    let mut opened = 0;
    for h in src.iter_headers() {
        let h = h.unwrap();
        src.message(h.locator.as_ref().unwrap()).unwrap();
        opened += 1;
    }
    assert!(opened >= 40, "{opened} messages");
    drop(src);
    std::fs::remove_file(&path).ok();
}

#[test]
fn header_and_page_crcs_are_consistent() {
    for (fmt, client) in [(Format::Ansi, b"SM"), (Format::Unicode, b"SM"), (Format::Unicode4k, b"SO")] {
        let bytes = build_store(Writer::new(fmt, NDB_CRYPT_PERMUTE, false), &fixture(5), client);
        let h = stv_pst::ndb::parse_header(&bytes).unwrap();
        assert_eq!(h.format, fmt);
        assert_eq!(h.crc_partial.0, h.crc_partial.1, "{fmt:?}: dwCRCPartial");
    }
}

/// Overwrite the data block of one message: that message fails with `Error::Format`, the
/// folder listing (contents table) and every other message keep working.
#[test]
fn corrupt_message_block_is_isolated() {
    let fx = fixture(30);
    let mut bytes = build_store(Writer::new(Format::Unicode, NDB_CRYPT_NONE, false), &fx, b"SM");
    let victim = fx.bulk[7].nid;
    let (ib, cb) = {
        let path = write_temp("corrupt-src.pst", &bytes);
        let src = PstSource::open(&path).unwrap();
        let ndb = &src.file().ndb;
        let e = ndb.block_entry(ndb.node(victim).unwrap().bid_data).unwrap();
        let r = (e.ib as usize, e.cb as usize);
        drop(src);
        std::fs::remove_file(&path).ok();
        r
    };
    bytes[ib..ib + cb].fill(0xFF);
    let path = write_temp("corrupt.pst", &bytes);
    let src = PstSource::open(&path).unwrap();
    let bulk = src.folders().unwrap().into_iter().find(|f| f.name == "Bulk").unwrap();
    assert_eq!(src.list(&bulk.id, 0..100, Sort::default()).unwrap().len(), 30);
    assert!(matches!(src.message(&Locator::Pst { nid: victim }), Err(Error::Format(_))));
    for m in fx.bulk.iter().filter(|m| m.nid != victim) {
        assert_eq!(src.message(&Locator::Pst { nid: m.nid }).unwrap().header.subject, m.subject);
    }
    drop(src);
    std::fs::remove_file(&path).ok();
}

fn exercise(src: &dyn MailSource) {
    let Ok(folders) = src.folders() else { return };
    for f in &folders {
        let _ = src.count(&f.id);
        for key in [SortKey::Native, SortKey::Date, SortKey::From, SortKey::Subject, SortKey::Size] {
            let _ = src.list(&f.id, 0..40, Sort { key, descending: true });
        }
    }
    let locs: Vec<Locator> = src.iter_headers().filter_map(|h| h.ok()?.locator).take(40).collect();
    for loc in locs {
        if let Ok(m) = src.message(&loc) {
            for a in &m.attachments {
                let _ = src.attachment(&loc, a.index);
                let e = Locator::Embedded { parent: Box::new(loc.clone()), attachment: a.index };
                let _ = src.message(&e);
            }
        }
    }
}

#[test]
fn truncated_files_error_without_panic() {
    let bytes = build_store(Writer::new(Format::Unicode, NDB_CRYPT_PERMUTE, false), &fixture(10), b"SM");
    for cut in [0, 3, 100, 520, 600, 0x4400, bytes.len() / 2, bytes.len() - 600, bytes.len() - 1] {
        let path = write_temp(&format!("trunc-{cut}.pst"), &bytes[..cut]);
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
            if let Ok(src) = PstSource::open(&path) {
                exercise(&src);
            }
        }));
        assert!(r.is_ok(), "panic on file truncated at {cut}");
        std::fs::remove_file(&path).ok();
    }
}

fn u64_le(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Hostile structures that random byte flips practically never produce: every one must fail
/// fast with an error, without panicking, looping or allocating out of proportion to the file.
#[test]
fn hostile_structures_fail_fast() {
    let started = std::time::Instant::now();
    let fx = fixture(5);
    let mut w = Writer::new(Format::Unicode, NDB_CRYPT_NONE, false);

    // Data tree: XXBLOCK → 1000 × the same XBLOCK → 1000 × the same 8000-byte block (8 GB).
    let leaf = w.add_block(vec![7u8; 8000], false);
    let short = {
        let (a, b) = (w.add_block(vec![1u8; 100], false), w.add_block(vec![2u8; 100], false));
        let mut x = vec![0x01, 1, 2, 0];
        x.extend_from_slice(&150u32.to_le_bytes()); // lcbTotal smaller than the 200 stored bytes
        x.extend_from_slice(&a.to_le_bytes());
        x.extend_from_slice(&b.to_le_bytes());
        w.add_block(x, true)
    };
    let fan = |w: &mut Writer, sig: u8, level: u8, n: usize, entry: &[u64]| {
        let mut b = vec![sig, level];
        b.extend_from_slice(&(n as u16).to_le_bytes());
        b.extend_from_slice(&u32::MAX.to_le_bytes()); // lcbTotal / padding
        for _ in 0..n {
            entry.iter().for_each(|v| b.extend_from_slice(&v.to_le_bytes()));
        }
        w.add_block(b, true)
    };
    let x = fan(&mut w, 0x01, 1, 1000, &[leaf]);
    let xx = fan(&mut w, 0x01, 2, 1000, &[x]);
    // Subnode trees: a SIBLOCK repeating one SLBLOCK, and a SIBLOCK below a SIBLOCK.
    let sl = fan(&mut w, 0x02, 0, 300, &[0x1F, leaf, 0]);
    let si = fan(&mut w, 0x02, 1, 500, &[0x1F, sl]);
    let si2 = fan(&mut w, 0x02, 1, 500, &[0x1F, si]);

    // Property context whose BTH index pages each point 500 times at the page below, 8 levels.
    let mut h = HeapBuilder::new(Format::Unicode, 0xBC, w.max_block);
    let mut page = h.alloc(vec![0x37, 0x00, 0x03, 0x00, 1, 0, 0, 0]);
    for _ in 0..8 {
        let rec: Vec<u8> = [0u8, 0].into_iter().chain(page.to_le_bytes()).collect();
        page = h.alloc(rec.repeat(500));
    }
    h.root = h.alloc([0xB5, 2, 6, 8].into_iter().chain(page.to_le_bytes()).collect());
    let bth = w.add_blocks(h.build());
    const NID_BAD_PC: u32 = 0x1F_FFE4;
    w.add_node(NID_BAD_PC, bth, 0, NID_INBOX);

    let path = write_temp("hostile.pst", &build_store(w, &fx, b"SM"));
    let src = PstSource::open(&path).unwrap();
    let ndb = &src.file().ndb;
    assert!(matches!(ndb.data_tree(xx), Err(Error::Format(_))));
    assert!(matches!(ndb.read_data(xx), Err(Error::Format(_))));
    assert!(matches!(ndb.data_tree(x), Err(Error::Format(_))), "1000 × one 8 KB block exceeds the file");
    assert_eq!(ndb.data_tree(short).unwrap().len(), 2);
    assert!(matches!(ndb.read_data(short), Err(Error::Format(_))), "data beyond lcbTotal");
    assert!(matches!(ndb.subnodes(si), Err(Error::Format(_))));
    assert!(matches!(ndb.subnodes(si2), Err(Error::Format(_))));
    assert_eq!(ndb.subnodes(sl).unwrap().len(), 300);
    let bad_pc = src.message(&Locator::Pst { nid: NID_BAD_PC });
    assert!(matches!(&bad_pc, Err(Error::Format(m)) if m.contains("BTH page")), "{:?}", bad_pc.map(|_| ()));
    assert_eq!(src.message(&Locator::Pst { nid: 0x200004 }).unwrap().header.subject, "Quarterly report");
    drop(src);
    std::fs::remove_file(&path).ok();

    let mut w = Writer::new(Format::Unicode, NDB_CRYPT_NONE, false);
    w.page_entries = Some(3);
    let base = build_store(w, &fixture(BULK), b"SM");
    let open = |name: &str, bytes: &[u8], f: &dyn Fn(&PstSource)| {
        let path = write_temp(name, bytes);
        if let Ok(src) = PstSource::open(&path) {
            f(&src);
        }
        std::fs::remove_file(&path).ok();
    };

    // NBT leaf page whose entries are too short for an NBTENTRY.
    let mut b = base.clone();
    let mut ib = u64_le(&b, 224) as usize;
    while b[ib + 491] > 0 {
        ib = u64_le(&b, ib + 16) as usize; // first child
    }
    b[ib + 490] = 16;
    open("short-nbt-entries.pst", &b, &|src| exercise(src));

    // Root NBT/BBT pages whose every child pointer loops back to the root.
    let mut b = base.clone();
    for at in [224usize, 240] {
        let root = u64_le(&b, at);
        let (count, level) = (b[root as usize + 488] as usize, b[root as usize + 491]);
        assert!(level > 0 && count > 1);
        for i in 0..count {
            let o = root as usize + i * 24 + 16;
            b[o..o + 8].copy_from_slice(&root.to_le_bytes());
        }
    }
    let ndb = Ndb::new(Backing::Vec(b)).unwrap();
    assert!(ndb.node(1).is_err() && ndb.node(0x21).is_err() && ndb.block_entry(4).is_err());
    assert!(ndb.all_nodes().is_err() && ndb.all_blocks().is_err());

    // Row matrix whose first block is shorter than the last: the row count must not
    // address blocks past the end.
    let bulk_contents = (NID_BULK & !0x1F) | 0x0E;
    let (bid, ib) = {
        let f = PstFile::from_bytes(base.clone()).unwrap();
        let node = f.node(bulk_contents).unwrap();
        let rows = node.subnodes.iter().find_map(|e| f.ndb.data_tree(e.bid_data).ok().filter(|t| t.len() > 1));
        let first = rows.expect("multi-block row matrix")[0];
        (first, f.ndb.block_entry(first).unwrap().ib)
    };
    let mut b = base;
    let key: Vec<u8> = bid.to_le_bytes().into_iter().chain(ib.to_le_bytes()).collect();
    let at = b.windows(16).rposition(|w| w == key).expect("BBT entry of the first row block");
    b[at + 16..at + 18].copy_from_slice(&8u16.to_le_bytes()); // cb: 8 bytes, less than one row
    let f = PstFile::from_bytes(b).unwrap();
    let t = f.table(f.node(bulk_contents).unwrap()).unwrap();
    assert!(t.row_count < BULK);
    for i in 0..t.row_count {
        let _ = t.row(i);
    }

    assert!(started.elapsed() < std::time::Duration::from_secs(60), "took {:?}", started.elapsed());
}

/// Random byte corruption anywhere in the file must never panic (overflow checks included in
/// debug builds) nor hang.
#[test]
fn random_corruption_never_panics() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let bases = [
        build_store(Writer::new(Format::Unicode, NDB_CRYPT_PERMUTE, false), &fixture(12), b"SM"),
        build_store(Writer::new(Format::Ansi, NDB_CRYPT_NONE, false), &fixture(12), b"SM"),
        build_store(Writer::new(Format::Unicode4k, NDB_CRYPT_NONE, true), &fixture(12), b"SO"),
    ];
    for iter in 0..300 {
        let base = &bases[iter % bases.len()];
        let mut b = base.clone();
        let flips = 1 + rng.below(40);
        for _ in 0..flips {
            // Bias towards structures (end of file holds the B-tree pages).
            let pos = if rng.below(3) == 0 { b.len() - 1 - rng.below(b.len() / 8) } else { rng.below(b.len()) };
            b[pos] = rng.next() as u8;
        }
        let path = write_temp(&format!("fuzz-{iter}.pst"), &b);
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
            if let Ok(src) = PstSource::open(&path) {
                exercise(&src);
            }
        }));
        std::fs::remove_file(&path).ok();
        assert!(r.is_ok(), "panic in fuzz iteration {iter}");
    }
}
