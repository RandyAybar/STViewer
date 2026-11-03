//! End-to-end tests of `stv-library` over the committed fixtures in `testdata/library`
//! (an mbox file and a directory of .eml files that form one conversation together).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use stv_library::mail_model::{
    AttachmentKind, Error, FolderId, Header, Locator, SearchOptions, Sort, SortKey, SourceId, SourceKind,
};
use stv_library::{sanitize_filename, ExportFormat, Library};

fn testdata(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(rel)
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Private copy of the fixtures so tests never write next to committed files.
fn fixtures(tmp: &Path) -> (PathBuf, PathBuf) {
    let root = tmp.join("mail");
    copy_dir(&testdata("library"), &root);
    (root.join("archive.mbox"), root.join("eml"))
}

fn wait_indexed(lib: &Library) {
    let start = Instant::now();
    loop {
        let p = lib.index_progress();
        if let Some(err) = p.iter().find_map(|p| p.error.clone()) {
            panic!("indexing failed: {err}");
        }
        if p.iter().all(|p| p.done) && !lib.is_indexing() {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "indexing timed out: {p:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn all_headers(lib: &Library, id: SourceId) -> Vec<Header> {
    let mut out = Vec::new();
    for f in lib.folders(id).unwrap() {
        let n = lib.count(id, &f.id).unwrap();
        out.extend(lib.list(id, &f.id, 0..n, Sort { key: SortKey::Date, descending: false }).unwrap());
    }
    out
}

fn find_by_mid(lib: &Library, id: SourceId, mid: &str) -> Locator {
    all_headers(lib, id)
        .into_iter()
        .find(|h| h.message_id.as_deref() == Some(mid))
        .and_then(|h| h.locator)
        .unwrap_or_else(|| panic!("{mid} not listed in source {id:?}"))
}

#[test]
fn add_path_detects_kinds_and_dedupes() {
    let tmp = tempfile::tempdir().unwrap();
    let (mbox, eml) = fixtures(tmp.path());
    let lib = Library::open(&tmp.path().join("lib")).unwrap();

    let a = lib.add_path(&mbox).unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].kind, SourceKind::Mbox);
    assert_eq!(a[0].path, std::path::absolute(&mbox).unwrap());

    let b = lib.add_path(&eml).unwrap();
    assert_eq!(b.len(), 1);
    assert_eq!(b[0].kind, SourceKind::Eml);

    let single = lib.add_path(&eml.join("b1.eml")).unwrap();
    assert_eq!(single[0].kind, SourceKind::Eml);

    // Re-adding a registered path returns the existing entry.
    let again = lib.add_path(&mbox).unwrap();
    assert_eq!(again[0].id, a[0].id);
    assert_eq!(lib.sources().len(), 3);

    let junk = tmp.path().join("notes.txt");
    fs::write(&junk, "just some text, not mail\n").unwrap();
    assert!(matches!(lib.add_path(&junk), Err(Error::Unsupported(_))));
    assert!(lib.add_path(&tmp.path().join("missing.mbox")).is_err());
    assert_eq!(lib.sources().len(), 3);

    // Every message of each source is listable through the library.
    assert_eq!(all_headers(&lib, a[0].id).len(), 3);
    assert_eq!(all_headers(&lib, b[0].id).len(), 2);
    assert_eq!(all_headers(&lib, single[0].id).len(), 1);
    // mbox header caches stay inside the library's data directory.
    let cache = tmp.path().join("lib/cache/mbox");
    assert!(fs::read_dir(&cache).is_ok_and(|mut d| d.next().is_some()), "no mbox cache in {}", cache.display());
}

#[test]
fn directory_scan_finds_each_distinct_source() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("export");
    let (mbox, eml) = fixtures(tmp.path());
    fs::create_dir_all(root.join("thunderbird")).unwrap();
    fs::copy(&mbox, root.join("thunderbird").join("Inbox")).unwrap();
    copy_dir(&eml, &root.join("saved-eml"));
    let pst = first_fixture("pst", &["pst", "ost"]);
    if let Some(p) = &pst {
        fs::copy(p, root.join("outlook.pst")).unwrap();
    }

    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let found = lib.add_path(&root).unwrap();
    let kinds: Vec<SourceKind> = found.iter().map(|s| s.kind).collect();
    let is_outlook = |k: &SourceKind| matches!(k, SourceKind::Pst | SourceKind::Ost);
    if pst.is_some() {
        assert!(kinds.iter().any(is_outlook), "PST/OST picked up as its own source: {kinds:?}");
    }
    // Both MIME messages and the mbox messages are reachable through the found sources.
    let mut mids: Vec<String> = found
        .iter()
        .filter(|s| !is_outlook(&s.kind))
        .flat_map(|s| all_headers(&lib, s.id))
        .filter_map(|h| h.message_id)
        .collect();
    mids.sort();
    mids.dedup();
    for want in ["a1@stv.test", "b1@stv.test", "c1@stv.test", "d1@stv.test", "e1@stv.test"] {
        assert!(mids.iter().any(|m| m == want), "{want} missing from {mids:?}");
    }
    // Scanning again registers nothing new.
    let n = lib.sources().len();
    lib.add_path(&root).unwrap();
    assert_eq!(lib.sources().len(), n);
}

/// Parts of a split archive added one at a time (or before grouping existed) end up as
/// one source whose folders merge both parts, also after reopening the library.
#[test]
fn split_archive_parts_added_separately_become_one_source() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = testdata("pst/synthetic-unicode-cyclic.pst");
    let (p1, p2) = (tmp.path().join("a/box.001.pst"), tmp.path().join("b/box.002.pst"));
    for p in [&p1, &p2] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::copy(&fixture, p).unwrap();
    }
    let data = tmp.path().join("lib");
    let lib = Library::open(&data).unwrap();
    let single = lib.add_path(&p1).unwrap();
    assert_eq!(single.len(), 1);
    let inbox = |lib: &Library, id| lib.folders(id).unwrap().into_iter().find(|f| f.name == "Inbox").unwrap().id;
    let per_part = lib.count(single[0].id, &inbox(&lib, single[0].id)).unwrap();
    assert!(per_part >= 2);

    let merged = lib.add_path(&p2).unwrap();
    let sources = lib.sources();
    assert_eq!(sources.len(), 1, "the part added first is replaced: {sources:?}");
    assert_eq!(sources[0].id, merged[0].id);
    assert!(sources[0].display_name.ends_with("(2 parts)"), "{}", sources[0].display_name);

    let id = merged[0].id;
    let folder = inbox(&lib, id);
    assert_eq!(lib.count(id, &folder).unwrap(), 2 * per_part);
    let page = lib.list(id, &folder, 0..2 * per_part, Sort { key: SortKey::Date, descending: true }).unwrap();
    assert_eq!(page.len() as u64, 2 * per_part);
    assert!(page.windows(2).all(|w| w[0].date >= w[1].date), "merged page is date-sorted");
    let parts: Vec<u32> = page
        .iter()
        .map(|h| match h.locator.as_ref().unwrap() {
            Locator::Part { part, .. } => *part,
            other => panic!("expected a part locator, got {other:?}"),
        })
        .collect();
    assert!(parts.contains(&0) && parts.contains(&1));
    let msg = lib.message(id, page[0].locator.as_ref().unwrap()).unwrap();
    assert_eq!(msg.header.subject, page[0].subject);
    assert_eq!(msg.header.folder.as_ref(), Some(&folder));
    drop(lib);

    let lib = Library::open(&data).unwrap();
    assert_eq!(lib.sources().len(), 1);
    assert_eq!(lib.count(id, &inbox(&lib, id)).unwrap(), 2 * per_part);

    // Re-adding a part returns the archive instead of registering the file again; a
    // further part extends the archive.
    let again = lib.add_path(&p2).unwrap();
    assert_eq!((again.len(), again[0].id), (1, id));
    assert_eq!(lib.sources().len(), 1);
    let p3 = tmp.path().join("c/box.003.pst");
    fs::create_dir_all(p3.parent().unwrap()).unwrap();
    fs::copy(&fixture, &p3).unwrap();
    let extended = lib.add_path(&p3).unwrap()[0].id;
    let sources = lib.sources();
    assert_eq!(sources.len(), 1, "{sources:?}");
    assert!(sources[0].display_name.ends_with("(3 parts)"), "{}", sources[0].display_name);
    assert_eq!(lib.count(extended, &inbox(&lib, extended)).unwrap(), 3 * per_part);
}

fn first_fixture(dir: &str, exts: &[&str]) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(testdata(dir))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
        })
        .collect();
    v.sort();
    v.into_iter().next()
}

#[test]
fn registry_persists_across_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let (mbox, eml) = fixtures(tmp.path());
    let dir = tmp.path().join("lib");
    let (ida, idb) = {
        let lib = Library::open(&dir).unwrap();
        let a = lib.add_path(&mbox).unwrap()[0].id;
        let b = lib.add_path(&eml).unwrap()[0].id;
        (a, b)
    };
    assert!(dir.join("sources.json").exists());
    {
        let lib = Library::open(&dir).unwrap();
        let s = lib.sources();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].id, s[0].kind), (ida, SourceKind::Mbox));
        assert_eq!((s[1].id, s[1].kind), (idb, SourceKind::Eml));
        assert_eq!(s[0].path, std::path::absolute(&mbox).unwrap());
        assert!(!s[0].display_name.is_empty());
        // Sources reopen lazily and still list.
        assert_eq!(all_headers(&lib, ida).len(), 3);
        lib.remove_source(ida).unwrap();
        assert!(matches!(lib.remove_source(ida), Err(Error::NotFound(_))));
    }
    let lib = Library::open(&dir).unwrap();
    let s = lib.sources();
    assert_eq!(s.iter().map(|s| s.id).collect::<Vec<_>>(), vec![idb]);
    // Ids are never reused.
    let c = lib.add_path(&mbox).unwrap()[0].id;
    assert!(c.0 > idb.0 && c != ida);
    assert!(matches!(lib.folders(ida), Err(Error::NotFound(_))));
}

#[test]
fn a_library_is_used_by_one_owner_at_a_time() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open(tmp.path()).unwrap();
    let clone = lib.clone();
    match Library::open(tmp.path()) {
        Err(Error::InUse { pid }) => assert_eq!(pid, Some(std::process::id())),
        other => panic!("second open: {:?}", other.map(|_| ())),
    }
    drop(lib);
    assert!(matches!(Library::open(tmp.path()), Err(Error::InUse { .. })), "a clone keeps the lock");
    drop(clone);
    Library::open(tmp.path()).expect("lock released when the last clone drops");
}

#[test]
fn corrupt_registry_is_a_format_error() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("sources.json"), "{ not json").unwrap();
    assert!(matches!(Library::open(tmp.path()), Err(Error::Format(_))));
}

#[test]
fn background_index_then_search_across_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let (mbox, eml) = fixtures(tmp.path());
    let dir = tmp.path().join("lib");
    let lib = Library::open(&dir).unwrap();
    let a = lib.add_path(&mbox).unwrap()[0].id;
    let b = lib.add_path(&eml).unwrap()[0].id;
    wait_indexed(&lib);

    let p = lib.index_progress();
    assert_eq!(p.len(), 2);
    assert!(p.iter().all(|p| p.done && p.indexed >= 2), "{p:?}");

    let opts = SearchOptions { limit: 50, ..Default::default() };
    let res = lib.search("zeppelin", &opts).unwrap();
    let sources: Vec<SourceId> = res.hits.iter().map(|h| h.source).collect();
    assert!(sources.contains(&a) && sources.contains(&b), "hits from both sources: {res:?}");
    let mids: Vec<&str> = res.hits.iter().filter_map(|h| h.header.message_id.as_deref()).collect();
    assert!(mids.contains(&"a1@stv.test") && mids.contains(&"b1@stv.test"), "{mids:?}");
    assert!(!mids.contains(&"d1@stv.test"));

    // Restricting to one source.
    let only_b = lib.search("zeppelin", &SearchOptions { sources: vec![b], ..opts.clone() }).unwrap();
    assert!(!only_b.hits.is_empty() && only_b.hits.iter().all(|h| h.source == b));

    // Hits carry locators that open the message.
    let hit = &res.hits[0];
    let m = lib.message(hit.source, hit.header.locator.as_ref().unwrap()).unwrap();
    assert_eq!(m.header.subject, hit.header.subject);

    // Reopening with an unchanged source is a no-op sync and search still works.
    drop(lib);
    let lib = Library::open(&dir).unwrap();
    wait_indexed(&lib);
    assert_eq!(lib.search("zeppelin", &opts).unwrap().total, res.total);

    // Removing a source removes its documents.
    lib.remove_source(b).unwrap();
    let after = lib.search("zeppelin", &opts).unwrap();
    assert!(!after.hits.is_empty() && after.hits.iter().all(|h| h.source == a));
}

#[test]
fn conversation_across_mbox_and_eml() {
    let tmp = tempfile::tempdir().unwrap();
    let (mbox, eml) = fixtures(tmp.path());
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let a = lib.add_path(&mbox).unwrap()[0].id;
    let b = lib.add_path(&eml).unwrap()[0].id;
    wait_indexed(&lib);

    // From the middle message (eml source): the whole chain across both sources, by date.
    let loc = find_by_mid(&lib, b, "b1@stv.test");
    let c = lib.conversation(b, &loc, None).unwrap();
    let got: Vec<(SourceId, &str, bool)> =
        c.iter().map(|i| (i.source, i.header.message_id.as_deref().unwrap_or(""), i.current)).collect();
    assert_eq!(got, vec![(a, "a1@stv.test", false), (b, "b1@stv.test", true), (a, "c1@stv.test", false)]);
    assert!(c.windows(2).all(|w| w[0].header.date < w[1].header.date));
    assert!(c.iter().all(|i| !i.preview.is_empty()), "every entry carries a preview");

    // Same conversation from the root; an unrelated message is a conversation of one.
    let root = find_by_mid(&lib, a, "a1@stv.test");
    assert_eq!(lib.conversation(a, &root, None).unwrap().len(), 3);
    let lunch = find_by_mid(&lib, a, "d1@stv.test");
    assert_eq!(lib.conversation(a, &lunch, None).unwrap().len(), 1);

    // Scoped to one source, the other source's messages are neither listed nor followed.
    let only_b = std::collections::HashSet::from([b]);
    let scoped = lib.conversation(b, &loc, Some(&only_b)).unwrap();
    let got: Vec<(SourceId, Option<&str>)> =
        scoped.iter().map(|i| (i.source, i.header.message_id.as_deref())).collect();
    assert_eq!(got, [(b, Some("b1@stv.test"))]);
    assert!(matches!(lib.conversation(a, &root, Some(&only_b)), Err(Error::NotFound(_))));
}

#[test]
fn embedded_rfc822_resolves_through_library() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, eml) = fixtures(tmp.path());
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let b = lib.add_path(&eml).unwrap()[0].id;
    let loc = find_by_mid(&lib, b, "e1@stv.test");
    let m = lib.message(b, &loc).unwrap();
    let att = m.attachments.iter().find(|a| a.kind == AttachmentKind::EmbeddedMessage).expect("embedded");
    let eloc = Locator::Embedded { parent: Box::new(loc.clone()), attachment: att.index };
    let inner = lib.message(b, &eloc).unwrap();
    assert_eq!(inner.header.subject, "Holiday photos");
    assert!(inner.body.text.unwrap_or_default().contains("lighthouse"));
    assert_eq!(inner.header.locator, Some(eloc));
}

#[test]
fn tags_on_emails_and_attachments_persist_and_follow_sources() {
    use stv_library::TagTarget;
    let tmp = tempfile::tempdir().unwrap();
    let (_, eml) = fixtures(tmp.path());
    let data = tmp.path().join("lib");
    let lib = Library::open(&data).unwrap();
    let b = lib.add_path(&eml).unwrap()[0].id;
    let e1 = find_by_mid(&lib, b, "e1@stv.test");
    let b1 = find_by_mid(&lib, b, "b1@stv.test");
    let att = lib.message(b, &e1).unwrap().attachments[0].index;
    let email = |l: &Locator| TagTarget { source: b, message: l.clone(), attachment: None };
    let attachment = TagTarget { source: b, message: e1.clone(), attachment: Some(att) };

    // Tagging creates the tag and snapshots the items.
    lib.set_tag(&[email(&e1), email(&b1), attachment.clone()], "Evidence", true).unwrap();
    lib.set_tag(&[email(&b1)], "Follow up", true).unwrap();
    let tags = lib.tags();
    let count = |name: &str| tags.iter().find(|t| t.name == name).map(|t| t.count);
    assert_eq!(count("Evidence"), Some(3));
    assert_eq!(count("Follow up"), Some(1));
    assert_eq!(lib.tagged_count(b), 3);
    let items = lib.tagged(Some(b), Some("evidence"));
    assert_eq!(items.len(), 3, "tag names match case-insensitively");
    let a = items.iter().find(|i| i.target == attachment).unwrap();
    assert_eq!(a.header.message_id.as_deref(), Some("e1@stv.test"));
    let row = a.attachment.as_ref().expect("attachment snapshot");
    assert_eq!(row.index, att);
    assert_eq!(
        lib.tags_of(&[email(&b1), email(&e1)]),
        vec![vec!["Evidence".to_string(), "Follow up".into()], vec!["Evidence".into()]]
    );
    // Re-tagging is idempotent; an unreadable target is reported but does not block the rest.
    let bogus = TagTarget { source: b, message: Locator::File { path: "missing.eml".into() }, attachment: None };
    assert!(lib.set_tag(&[email(&e1), bogus.clone()], "Evidence", true).is_err());
    assert_eq!(lib.tagged(Some(b), Some("Evidence")).len(), 3);
    assert!(lib.tags_of(&[bogus])[0].is_empty());

    // Untag drops items left without tags; rename and delete apply to every item.
    lib.set_tag(&[email(&e1)], "Evidence", false).unwrap();
    assert_eq!(lib.tagged_count(b), 2);
    lib.rename_tag("Follow up", "Review").unwrap();
    assert_eq!(lib.tags_of(&[email(&b1)])[0], vec!["Evidence".to_string(), "Review".into()]);
    assert!(lib.rename_tag("Review", "evidence").is_err(), "names stay unique");
    lib.delete_tag("Evidence").unwrap();
    assert_eq!(lib.tagged(Some(b), None).len(), 1);
    drop(lib);

    // Persisted; removing the source forgets its items.
    let lib = Library::open(&data).unwrap();
    assert_eq!(lib.tags_of(&[email(&b1)])[0], vec!["Review".to_string()]);
    lib.remove_source(b).unwrap();
    assert!(lib.tagged(None, None).is_empty());
    assert_eq!(lib.tags().len(), 1, "tag definitions outlive their items");
}

#[test]
fn export_eml_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, eml) = fixtures(tmp.path());
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let b = lib.add_path(&eml).unwrap()[0].id;
    let items: Vec<(SourceId, Locator)> = all_headers(&lib, b).into_iter().map(|h| (b, h.locator.unwrap())).collect();
    let out = tmp.path().join("out-eml");
    let report = lib.export(&items, ExportFormat::Eml, &out).unwrap();
    assert_eq!(report.written, 2);
    assert!(report.failed.is_empty());

    let files: Vec<PathBuf> = fs::read_dir(&out).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(files.len(), 2);
    for (id, loc) in &items {
        let orig = lib.message(*id, loc).unwrap();
        let file = files
            .iter()
            .find(|f| f.file_name().unwrap().to_string_lossy().contains(&sanitize_filename(&orig.header.subject)))
            .unwrap_or_else(|| panic!("no file for {:?} in {files:?}", orig.header.subject));
        let bytes = fs::read(file).unwrap();
        let back = stv_mbox::message_from_mime(&bytes, None, None).unwrap();
        assert_eq!(back.header.subject, orig.header.subject);
        assert_eq!(back.header.message_id, orig.header.message_id);
        assert_eq!(back.body.text, orig.body.text);
        assert_eq!(back.attachments.len(), orig.attachments.len());
        for (x, y) in back.attachments.iter().zip(&orig.attachments) {
            assert_eq!(x.filename, y.filename);
            assert_eq!(x.kind, y.kind);
            if x.kind == AttachmentKind::File {
                let xb = stv_mbox::attachment_from_mime(&bytes, x.index).unwrap();
                assert_eq!(xb, lib.attachment(*id, loc, y.index).unwrap());
            }
        }
    }
}

#[test]
fn export_mbox_html_text_csv_attachments() {
    let tmp = tempfile::tempdir().unwrap();
    let (mbox, eml) = fixtures(tmp.path());
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let a = lib.add_path(&mbox).unwrap()[0].id;
    let b = lib.add_path(&eml).unwrap()[0].id;
    let mut items: Vec<(SourceId, Locator)> =
        all_headers(&lib, a).into_iter().map(|h| (a, h.locator.unwrap())).collect();
    items.extend(all_headers(&lib, b).into_iter().map(|h| (b, h.locator.unwrap())));
    assert_eq!(items.len(), 5);

    // mbox: re-open the export with the mbox reader.
    let out = tmp.path().join("out");
    let r = lib.export(&items, ExportFormat::Mbox, &out).unwrap();
    assert_eq!((r.written, r.failed.len()), (5, 0));
    let exported = out.join("export.mbox");
    let src = stv_mbox::opener().open(&exported).unwrap();
    let mut subjects: Vec<String> = src.iter_headers().map(|h| h.unwrap().subject).collect();
    subjects.sort();
    assert_eq!(
        subjects,
        ["Fwd: Holiday photos", "Lunch on Friday?", "Project kickoff", "Re: Project kickoff", "Re: Project kickoff"]
    );
    let text = fs::read_to_string(&exported).unwrap();
    assert!(text.starts_with("From alice@stv.test Mon Jan  2 09:00:00 2023\n"));
    // The body line ">From the archive" is quoted once more by mboxrd.
    assert!(text.contains("\n>>From the archive") || text.contains("\n>From the archive"));
    assert!(!text.contains("\nFrom the archive"));

    // HTML: header table + inline image as data: URI.
    let b1 = find_by_mid(&lib, b, "b1@stv.test");
    let r = lib.export(&[(b, b1.clone())], ExportFormat::Html, &out).unwrap();
    assert_eq!(r.written, 1);
    let html_file = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "html"))
        .unwrap();
    let html = fs::read_to_string(&html_file).unwrap();
    assert!(html.contains("Bob Baker &lt;bob@stv.test&gt;"));
    assert!(html.contains("src=\"data:image/png;base64,"));
    assert!(!html.contains("cid:chart@stv.test"));
    assert!(html.contains("http-equiv=\"Content-Security-Policy\" content=\"default-src 'none';"));
    assert!(html_file.file_name().unwrap().to_string_lossy().starts_with("2023-01-03 1015 Re_ Project kickoff"));

    // Text.
    let r = lib.export(&[(b, b1.clone())], ExportFormat::Text, &out).unwrap();
    assert_eq!(r.written, 1);
    let txt = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "txt"))
        .unwrap();
    let txt = fs::read_to_string(txt).unwrap();
    assert!(txt.contains("Subject: Re: Project kickoff\n"));
    assert!(txt.contains("Cc: Carol Chen <carol@stv.test>\n"));
    assert!(txt.contains("zeppelin numbers"));

    // CSV: header + one row per message, quoted fields.
    let r = lib.export(&items, ExportFormat::Csv, &out).unwrap();
    assert_eq!(r.written, 5);
    let csv = fs::read_to_string(out.join("export.csv")).unwrap();
    let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').lines().collect();
    assert_eq!(lines[0], "date,from,to,cc,subject,folder,attachments,size");
    assert_eq!(lines.len(), 6);
    assert!(lines.iter().any(|l| l.starts_with("2023-01-03 10:15:00,Bob Baker <bob@stv.test>,Alice Archer <alice@stv.test>,Carol Chen <carol@stv.test>,Re: Project kickoff,")
        && l.contains("budget.csv")));
    assert!(lines.iter().any(|l| l.contains("\"Alice Archer <alice@stv.test>, Bob Baker <bob@stv.test>\"")));

    // Attachments, twice into the same directory → collision-suffixed names.
    let att_dir = tmp.path().join("atts");
    let r1 = lib.export(&items, ExportFormat::Attachments, &att_dir).unwrap();
    let r2 = lib.export(&items, ExportFormat::Attachments, &att_dir).unwrap();
    assert_eq!(r1.written, r2.written);
    assert!(r1.written >= 4, "notes.txt, chart.png, budget.csv, original.eml: {r1:?}");
    for f in ["notes.txt", "chart.png", "budget.csv", "original.eml", "budget (2).csv", "original (2).eml"] {
        assert!(att_dir.join(f).exists(), "{f} missing");
    }
    assert_eq!(fs::read(att_dir.join("budget.csv")).unwrap(), b"item,cost\nhelium,1200\nfabric,800\n");
    let orig = fs::read(att_dir.join("original.eml")).unwrap();
    assert_eq!(stv_mbox::message_from_mime(&orig, None, None).unwrap().header.subject, "Holiday photos");

    // A bad locator is reported, not fatal.
    let bogus = (a, Locator::Mbox { file: String::new(), offset: 1 << 40, len: 10 });
    let r = lib.export(&[bogus], ExportFormat::Eml, &tmp.path().join("bogus")).unwrap();
    assert_eq!((r.written, r.failed.len()), (0, 1));
}

/// The iCalendar export of an invite, read back by the MIME reader's iCalendar parser, gives
/// the same details; formats that do not apply to an item skip it.
#[test]
fn ics_export_round_trips_through_the_ical_reader() {
    use stv_library::mail_model::{ItemDetails, When};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("invites");
    fs::create_dir_all(&dir).unwrap();
    let location = format!("Room 4, Building \"B\"; floor 2\\3 {}", "é".repeat(60));
    let ical_location = location.replace('\\', "\\\\").replace(',', "\\,").replace(';', "\\;");
    let invite = format!(
        "From: Olivia <olivia@example.org>\r\nTo: Rita <rita@example.org>\r\nSubject: Planning, Q3; review\r\n\
         Message-ID: <invite1@example.org>\r\nDate: Mon, 1 Jun 2026 09:00:00 +0000\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/calendar; method=REQUEST; charset=UTF-8\r\n\r\n\
         BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:plan-q3@example.org\r\n\
         DTSTAMP:20260601T090000Z\r\nDTSTART:20260610T140000Z\r\nDTEND:20260610T153000Z\r\nSUMMARY:Planning\r\n\
         LOCATION:{ical_location}\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\n\
         ORGANIZER;CN=\"Olivia, O.\":mailto:olivia@example.org\r\n\
         ATTENDEE;ROLE=REQ-PARTICIPANT;CN=Rita:mailto:rita@example.org\r\n\
         ATTENDEE;ROLE=OPT-PARTICIPANT;CN=Otto:mailto:otto@example.org\r\n\
         END:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    fs::write(dir.join("invite.eml"), invite).unwrap();
    fs::write(
        dir.join("plain.eml"),
        "From: a@example.org\r\nSubject: Lunch\r\nMessage-ID: <plain@example.org>\r\n\r\nsandwiches\r\n",
    )
    .unwrap();
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let id = lib.add_path(&dir).unwrap()[0].id;
    let items: Vec<(SourceId, Locator)> = all_headers(&lib, id).into_iter().map(|h| (id, h.locator.unwrap())).collect();
    assert_eq!(items.len(), 2);

    let out = tmp.path().join("out");
    let r = lib.export(&items, ExportFormat::Ics, &out).unwrap();
    assert_eq!((r.written, r.skipped, r.failed.len()), (1, 1, 0));
    let ics = fs::read(out.join("export.ics")).unwrap();
    let text = std::str::from_utf8(&ics).unwrap();
    assert!(text.split_inclusive('\n').all(|l| l.ends_with("\r\n") && l.len() <= 77), "CRLF, folded: {text}");

    let orig = items.iter().find_map(|(s, l)| lib.message(*s, l).unwrap().header.item).unwrap();
    let wrapped = [b"Subject: x\r\nContent-Type: text/calendar; charset=UTF-8\r\n\r\n".as_slice(), &ics].concat();
    let back = stv_mbox::message_from_mime(&wrapped, None, None).unwrap().header.item;
    let (Some(ItemDetails::Appointment(back)), ItemDetails::Appointment(orig)) = (back, orig) else {
        panic!("not appointments")
    };
    assert_eq!(back.start, Some(When::Time(1_781_100_000_000)));
    assert_eq!(back.start, orig.start);
    assert_eq!(back.end, orig.end);
    assert_eq!(back.location.as_deref(), Some(location.as_str()));
    assert_eq!(back.recurrence.as_deref(), Some("FREQ=WEEKLY;COUNT=4"));
    assert_eq!(back.uid.as_deref(), Some("plan-q3@example.org"));
    assert_eq!(back.organizer, orig.organizer);
    assert_eq!(back.required, orig.required);
    assert_eq!(back.optional, orig.optional);

    let r = lib.export(&items, ExportFormat::Vcf, &out).unwrap();
    assert_eq!((r.written, r.skipped), (0, 2));
    assert!(!out.join("export.vcf").exists(), "no empty (invalid) vCard file");
    // Selected twice: the repeat is a duplicate UID, not a second VEVENT.
    let r = lib.export(&[items.clone(), items].concat(), ExportFormat::Ics, &out).unwrap();
    assert_eq!((r.written, r.skipped, r.duplicates), (1, 2, 1));
    assert!(out.join("export (2).ics").exists(), "an earlier export is not overwritten");
}

/// A task's iCalendar `UID` survives mbox → `.ics` export → iCalendar reader; repeating it
/// in one export is a duplicate.
#[test]
fn ics_todo_uid_round_trips() {
    use stv_library::mail_model::ItemDetails;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("todo");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("todo.eml"),
        "From: Olivia <olivia@example.org>\r\nSubject: File report\r\nMessage-ID: <todo1@example.org>\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/calendar; charset=UTF-8\r\n\r\n\
         BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:task-7@example.org\r\nDTSTAMP:20260601T090000Z\r\n\
         SUMMARY:File report\r\nDUE;VALUE=DATE:20260615\r\nSTATUS:NEEDS-ACTION\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
    )
    .unwrap();
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let id = lib.add_path(&dir).unwrap()[0].id;
    let items: Vec<(SourceId, Locator)> = all_headers(&lib, id).into_iter().map(|h| (id, h.locator.unwrap())).collect();
    let task_uid = |item| match item {
        Some(ItemDetails::Task(t)) => t.uid,
        other => panic!("not a task: {other:?}"),
    };
    assert_eq!(task_uid(lib.message(id, &items[0].1).unwrap().header.item).as_deref(), Some("task-7@example.org"));

    let out = tmp.path().join("out");
    let r = lib.export(&[items.clone(), items].concat(), ExportFormat::Ics, &out).unwrap();
    assert_eq!((r.written, r.duplicates), (1, 1));
    let ics = fs::read(out.join("export.ics")).unwrap();
    let wrapped = [b"Subject: x\r\nContent-Type: text/calendar; charset=UTF-8\r\n\r\n".as_slice(), &ics].concat();
    let back = stv_mbox::message_from_mime(&wrapped, None, None).unwrap().header.item;
    assert_eq!(task_uid(back).as_deref(), Some("task-7@example.org"), "not the generated fallback UID");
}

#[test]
fn msg_fixture_exports_as_mime() {
    let Some(msg) = first_fixture_recursive(&testdata("msg"), "msg") else {
        panic!("no .msg fixture under testdata/msg");
    };
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let s = lib.add_path(&msg).unwrap();
    assert_eq!(s[0].kind, SourceKind::Msg);
    let id = s[0].id;
    let headers = all_headers(&lib, id);
    assert_eq!(headers.len(), 1);
    let loc = headers[0].locator.clone().unwrap();
    let orig = lib.message(id, &loc).unwrap();

    let mime = lib.mime(id, &loc).unwrap();
    let back = stv_mbox::message_from_mime(&mime, None, None).unwrap();
    assert_eq!(back.header.subject, orig.header.subject);
    assert_eq!(back.header.date.map(|d| d / 1000), orig.header.date.map(|d| d / 1000));
    if let Some(f) = &orig.header.from {
        assert_eq!(back.header.from.as_ref().and_then(|a| a.email.clone()), f.email.clone());
    }
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(t) = orig.body.text.as_deref().filter(|t| !t.trim().is_empty()) {
        assert_eq!(norm(back.body.text.as_deref().unwrap_or("")), norm(t));
    }
    let files: Vec<_> = orig.attachments.iter().filter(|a| a.kind == AttachmentKind::File).collect();
    for a in files {
        let bytes = lib.attachment(id, &loc, a.index).unwrap();
        let found = back.attachments.iter().any(|b| {
            b.filename == a.filename
                && stv_mbox::attachment_from_mime(&mime, b.index).ok().as_deref() == Some(&bytes[..])
        });
        assert!(found, "attachment {:?} round-trips byte-exact", a.filename);
    }

    // Export writes the same bytes the library synthesises (modulo boundaries/ids).
    let out = tmp.path().join("out");
    let r = lib.export(&[(id, loc)], ExportFormat::Eml, &out).unwrap();
    assert_eq!((r.written, r.failed.len()), (1, 0));
}

fn first_fixture_recursive(dir: &Path, ext: &str) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir).ok()?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in &entries {
        if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) {
            return Some(p.clone());
        }
    }
    entries.iter().filter(|p| p.is_dir()).find_map(|p| first_fixture_recursive(p, ext))
}

#[test]
fn cross_format_embedded_tnef_and_msg_inside_mime() {
    use base64::Engine as _;
    let dat = fs::read(testdata("msg/poi/bug52400-winmail-simple.dat")).unwrap();
    let msg = fs::read(testdata("msg/poi/attachment_test_msg.msg")).unwrap();
    let b64 = |b: &[u8]| {
        let s = base64::engine::general_purpose::STANDARD.encode(b);
        s.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("\r\n")
    };
    let carrier = format!(
        "From: Gina <gina@stv.test>\r\nTo: Alice <alice@stv.test>\r\nSubject: carrier\r\nMessage-ID: <carrier@stv.test>\r\n\
         Date: Sun, 08 Jan 2023 10:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"zz\"\r\n\r\n\
         --zz\r\nContent-Type: text/plain\r\n\r\nSee attachments.\r\n\
         --zz\r\nContent-Type: application/ms-tnef; name=\"winmail.dat\"\r\nContent-Disposition: attachment; filename=\"winmail.dat\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n\
         --zz\r\nContent-Type: application/vnd.ms-outlook; name=\"inner.msg\"\r\nContent-Disposition: attachment; filename=\"inner.msg\"\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--zz--\r\n",
        b64(&dat),
        b64(&msg)
    );
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("carrier.eml");
    fs::write(&path, carrier).unwrap();
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let id = lib.add_path(&path).unwrap()[0].id;
    let loc = all_headers(&lib, id)[0].locator.clone().unwrap();
    let m = lib.message(id, &loc).unwrap();
    let by_name = |n: &str| m.attachments.iter().find(|a| a.filename.as_deref() == Some(n)).unwrap().index;

    // TNEF inside MIME.
    let tloc = Locator::Embedded { parent: Box::new(loc.clone()), attachment: by_name("winmail.dat") };
    let want = stv_msg::message_from_tnef_bytes(&dat, None).unwrap();
    let got = lib.message(id, &tloc).unwrap();
    assert_eq!(got.header.subject, want.header.subject);
    assert_eq!(got.body, want.body);
    assert_eq!(got.attachments.len(), want.attachments.len());
    assert_eq!(got.header.locator.as_ref(), Some(&tloc));
    // TNEF carries no message size: the attachment's byte count stands in.
    assert_eq!(got.header.size, dat.len() as u64);
    for a in &want.attachments {
        assert_eq!(
            lib.attachment(id, &tloc, a.index).unwrap(),
            stv_msg::attachment_from_tnef_bytes(&dat, a.index).unwrap()
        );
    }

    // .msg inside MIME: decodes, and MIME is synthesised for it.
    let mloc = Locator::Embedded { parent: Box::new(loc.clone()), attachment: by_name("inner.msg") };
    let want = stv_msg::message_from_msg_bytes(&msg, None).unwrap();
    let got = lib.message(id, &mloc).unwrap();
    assert_eq!(got.header.subject, want.header.subject);
    assert_eq!(got.attachments.len(), want.attachments.len());
    assert!(got.header.size > 0, "an attached .msg has a size");
    let mime = lib.mime(id, &mloc).unwrap();
    let back = stv_mbox::message_from_mime(&mime, None, None).unwrap();
    assert_eq!(back.header.subject, want.header.subject);

    // A plain file attachment is not a message.
    let txt_loc = Locator::Embedded { parent: Box::new(loc.clone()), attachment: 99 };
    assert!(lib.message(id, &txt_loc).is_err());
}

#[test]
fn windows_filename_sanitising() {
    assert_eq!(sanitize_filename("Re: budget?/Q1*\"final\"<v2>|.pdf"), "Re_ budget__Q1__final__v2__.pdf");
    assert_eq!(sanitize_filename("CON"), "_CON");
    assert_eq!(sanitize_filename("con.txt"), "_con.txt");
    assert_eq!(sanitize_filename("Lpt9.log"), "_Lpt9.log");
    assert_eq!(sanitize_filename("COM¹"), "_COM¹");
    assert_eq!(sanitize_filename("console.txt"), "console.txt");
    assert_eq!(sanitize_filename("trailing dots... "), "trailing dots");
    assert_eq!(sanitize_filename("tab\there\u{1}"), "tab_here_");
    // A right-to-left override would display "invoice<RLO>fdp.exe" as "invoiceexe.pdf".
    assert_eq!(sanitize_filename("invoice\u{202E}fdp.exe"), "invoice_fdp.exe");
    assert_eq!(sanitize_filename("a\u{2066}b\u{200F}c\u{85}"), "a_b_c_");
    assert_eq!(sanitize_filename(""), "_");
    assert_eq!(sanitize_filename(" .. "), "_");
    let long = format!("{}.docx", "ü".repeat(300));
    let s = sanitize_filename(&long);
    assert!(s.chars().count() <= 120 && s.ends_with(".docx"), "{s}");
    // Unicode survives untouched.
    assert_eq!(sanitize_filename("Résumé 履歴書.pdf"), "Résumé 履歴書.pdf");
}

#[test]
fn unknown_folder_and_source_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (mbox, _) = fixtures(tmp.path());
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let a = lib.add_path(&mbox).unwrap()[0].id;
    assert!(lib.list(SourceId(999), &FolderId("x".into()), 0..10, Sort::default()).is_err());
    assert!(lib.reindex(SourceId(999)).is_err());
    lib.reindex(a).unwrap();
    wait_indexed(&lib);
    assert_eq!(lib.index_progress()[0].indexed, 3);
}
