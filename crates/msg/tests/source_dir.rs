//! Directory-of-`.msg` sources: folder tree, counts, sorted paging, locators, fingerprints.

mod common;

use std::fs;
use std::path::PathBuf;

use common::*;
use mail_model::{Error, FolderId, Locator, Sort, SortKey, SourceKind};

const DAY: i64 = 86_400_000;

fn msg(subject: &str, from: &str, day: i64) -> Vec<u8> {
    build_msg(
        &MsgB::default()
            .prop(0x001A, V::Unicode("IPM.Note".into()))
            .prop(0x0037, V::Unicode(subject.into()))
            .prop(0x0C1A, V::Unicode(from.into()))
            .prop(0x0E06, V::Time(filetime(1_600_000_000_000 + day * DAY))),
    )
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn tree(tag: &str) -> TempDir {
    let root = std::env::temp_dir().join(format!("stv-msg-dir-{tag}-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(root.join("sub/deeper")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::write(root.join("a.msg"), msg("Zulu", "Yvonne", 3)).unwrap();
    fs::write(root.join("broken.msg"), b"\xD0\xCF\x11\xE0 definitely not a compound file").unwrap();
    fs::write(root.join("notes.txt"), b"ignored").unwrap();
    fs::write(root.join("sub/b.msg"), msg("alpha", "Mallory", 1)).unwrap();
    fs::write(root.join("sub/c.MSG"), msg("Mike", "alice", 2)).unwrap();
    let tnef = TnefB::new(1252).attr(LVL_MSG, ATT_SUBJECT, sz("From TNEF")).build();
    fs::write(root.join("sub/deeper/winmail.dat"), tnef).unwrap();
    TempDir(root)
}

fn subjects(h: &[mail_model::Header]) -> Vec<String> {
    h.iter().map(|h| h.subject.clone()).collect()
}

#[test]
fn folders_counts_and_sorting() {
    let t = tree("sort");
    let op = stv_msg::opener();
    assert_eq!(op.sniff(&t.0), Some(SourceKind::Msg));
    assert_eq!(op.sniff(&t.0.join("empty")), None);
    let src = op.open(&t.0).unwrap();
    assert_eq!(src.kind(), SourceKind::Msg);

    let folders = src.folders().unwrap();
    let ids: Vec<_> = folders.iter().map(|f| f.id.0.as_str()).collect();
    assert_eq!(ids, ["", "sub", "sub/deeper"], "empty dirs excluded, parents first");
    assert_eq!(folders[1].parent, Some(FolderId(String::new())));
    assert_eq!(folders[2].parent, Some(FolderId("sub".into())));
    assert_eq!(folders[2].name, "deeper");
    let counts: Vec<_> = folders.iter().map(|f| src.count(&f.id).unwrap()).collect();
    assert_eq!(counts, [2, 2, 1]);
    assert_eq!(folders.iter().map(|f| f.message_count).collect::<Vec<_>>(), [Some(2), Some(2), Some(1)]);

    let sub = FolderId("sub".into());
    let by = |key, descending| src.list(&sub, 0..10, Sort { key, descending }).unwrap();
    assert_eq!(subjects(&by(SortKey::Date, false)), ["alpha", "Mike"]);
    assert_eq!(subjects(&by(SortKey::Date, true)), ["Mike", "alpha"]);
    assert_eq!(subjects(&by(SortKey::Subject, false)), ["alpha", "Mike"], "case-insensitive");
    assert_eq!(subjects(&by(SortKey::From, false)), ["Mike", "alpha"], "alice < Mallory");
    assert_eq!(subjects(&by(SortKey::Native, false)), ["alpha", "Mike"], "file name order");
    assert_eq!(subjects(&by(SortKey::Native, true)), ["Mike", "alpha"]);
    assert_eq!(subjects(&src.list(&sub, 1..10, Sort { key: SortKey::Date, descending: false }).unwrap()), ["Mike"]);
    assert!(src.list(&sub, 5..10, Sort::default()).unwrap().is_empty());

    let h = &by(SortKey::Date, false)[0];
    assert_eq!(h.locator, Some(Locator::File { path: "sub/b.msg".into() }));
    assert_eq!(h.folder, Some(sub.clone()));
    assert_eq!(h.date, Some(1_600_000_000_000 + DAY));

    let deeper = src.list(&FolderId("sub/deeper".into()), 0..10, Sort::default()).unwrap();
    assert_eq!(subjects(&deeper), ["From TNEF"]);
    assert!(matches!(src.count(&FolderId("nope".into())), Err(Error::NotFound(_))));
}

#[test]
fn unreadable_files_and_locators() {
    let t = tree("loc");
    let src = stv_msg::opener().open(&t.0).unwrap();
    // The corrupt file still occupies its row (named after the file) so pages match count().
    let root = src.list(&FolderId(String::new()), 0..10, Sort { key: SortKey::Native, descending: false }).unwrap();
    assert_eq!(subjects(&root), ["Zulu", "broken.msg"]);
    let broken = root[1].locator.clone().unwrap();
    assert!(matches!(src.message(&broken), Err(Error::Format(_))));

    let all: Vec<_> = src.iter_headers().collect();
    assert_eq!(all.len(), 5);
    assert_eq!(all.iter().filter(|r| r.is_ok()).count(), 4);
    assert!(all.iter().flatten().all(|h| h.locator.is_some() && h.folder.is_some()));

    let m = src.message(&Locator::File { path: "sub/c.MSG".into() }).unwrap();
    assert_eq!(m.header.subject, "Mike");
    assert!(matches!(src.message(&Locator::File { path: "../outside.msg".into() }), Err(Error::NotFound(_))));
    assert!(matches!(src.message(&Locator::File { path: "notes.txt".into() }), Err(Error::NotFound(_))));
}

#[test]
fn fingerprint_tracks_changes() {
    let t = tree("fp");
    let src = stv_msg::opener().open(&t.0).unwrap();
    let a = src.fingerprint().unwrap();
    assert_eq!(a, src.fingerprint().unwrap());
    fs::write(t.0.join("sub/new.msg"), msg("New", "x", 9)).unwrap();
    let b = src.fingerprint().unwrap();
    assert_ne!(a.head_hash, b.head_hash);
    assert!(b.size > a.size);

    // Single-file fingerprint hashes the head of the file.
    let file = stv_msg::opener().open(&t.0.join("a.msg")).unwrap();
    let f1 = file.fingerprint().unwrap();
    assert_eq!(f1.size, fs::metadata(t.0.join("a.msg")).unwrap().len());
    fs::write(t.0.join("a.msg"), msg("Zulu 2", "Yvonne", 3)).unwrap();
    assert_ne!(f1.head_hash, file.fingerprint().unwrap().head_hash);
}
