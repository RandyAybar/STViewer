mod common;

use std::path::Path;

use common::{fixtures, PDF, PNG};
use mail_model::{AttachmentKind, Error, FolderId, FolderKind, Header, Locator, MailSource, Sort, SortKey, SourceKind};
use stv_mbox::{MboxSource, OpenOptions};

struct Opened {
    _cache: tempfile::TempDir,
    src: MboxSource,
}

fn open_path(p: &Path) -> Opened {
    let cache = tempfile::tempdir().unwrap();
    let src =
        MboxSource::open(p, &OpenOptions { cache_dir: Some(cache.path().to_path_buf()), no_cache: false }).unwrap();
    Opened { _cache: cache, src }
}

fn open(rel: &str) -> Opened {
    open_path(&fixtures().join(rel))
}

fn all(src: &MboxSource, folder: &str) -> Vec<Header> {
    let f = FolderId(folder.into());
    let n = src.count(&f).unwrap();
    src.list(&f, 0..n, Sort { key: SortKey::Native, descending: false }).unwrap()
}

fn subjects(h: &[Header]) -> Vec<&str> {
    h.iter().map(|h| h.subject.as_str()).collect()
}

fn ts(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
    mail_model::time::days_from_civil(y, mo, d) * mail_model::time::MS_PER_DAY + (h * 3600 + mi * 60) * 1000
}

#[test]
fn sniffing() {
    let o = stv_mbox::opener();
    let f = fixtures();
    assert_eq!(o.sniff(&f.join("basic.mbox")), Some(SourceKind::Mbox));
    assert_eq!(o.sniff(&f.join("crlf.mbox")), Some(SourceKind::Mbox));
    assert_eq!(o.sniff(&f.join("single.eml")), Some(SourceKind::Eml));
    assert_eq!(o.sniff(&f.join("thunderbird")), Some(SourceKind::Thunderbird));
    assert_eq!(o.sniff(&f.join("thunderbird/Mail/Local Folders/Inbox")), Some(SourceKind::Mbox));
    assert_eq!(o.sniff(&f.join("emldir")), Some(SourceKind::Eml));
    assert_eq!(o.sniff(&f.join("apple")), Some(SourceKind::AppleMail));
    assert_eq!(o.sniff(&f.join("corrupt/garbage.bin")), None);
    assert_eq!(o.sniff(&f.join("thunderbird/Mail/Local Folders/Inbox.msf")), None);
    assert_eq!(o.sniff(&f.join("does-not-exist")), None);
}

#[test]
fn basic_mboxo_headers() {
    let o = open("basic.mbox");
    let src = &o.src;
    assert_eq!(src.kind(), SourceKind::Mbox);
    let folders = src.folders().unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].id, FolderId(String::new()));
    assert_eq!(folders[0].message_count, Some(3));
    let h = all(src, "");
    assert_eq!(subjects(&h), vec!["Café meeting", "Re: Café meeting", "Attachments galore"]);
    let from = h[0].from.as_ref().unwrap();
    assert_eq!(from.name.as_deref(), Some("Jürgen"));
    assert_eq!(from.email.as_deref(), Some("jurgen@example.com"));
    assert_eq!(h[0].to.len(), 2);
    assert_eq!(h[0].to[1].email.as_deref(), Some("bob@example.com"));
    assert_eq!(h[0].cc[0].name.as_deref(), Some("Carol C."));
    assert_eq!(h[0].date, Some(ts(2022, 3, 1, 9, 0)));
    assert_eq!(h[0].message_id.as_deref(), Some("m1@example.com"));
    assert!(h[0].flags.read);
    // No Date header: falls back to the From_ line date.
    assert_eq!(h[1].date, Some(ts(2022, 3, 1, 11, 0)));
    assert_eq!(h[1].in_reply_to.as_deref(), Some("m1@example.com"));
    assert_eq!(h[1].references, vec!["m1@example.com"]);
    assert!(!h[1].flags.read, "Status: O means unread");
    assert!(h[1].flags.flagged, "X-Status: F");
    assert!(h[2].has_attachments);
    assert!(!h[0].has_attachments);
    for x in &h {
        assert!(matches!(x.locator, Some(Locator::Mbox { ref file, .. }) if file.is_empty()));
        assert_eq!(x.folder, Some(FolderId(String::new())));
    }
    // Message bodies: `From ` inside the body did not split, `>From ` is unescaped.
    let m = src.message(h[0].locator.as_ref().unwrap()).unwrap();
    let text = m.body.text.unwrap();
    assert!(text.contains("From the desk of Jurgen"));
    assert!(text.contains("\nFrom escaped line"));
    assert!(m.raw_headers.unwrap().starts_with("From: =?utf-8?B?"));
    let props = src.properties(h[0].locator.as_ref().unwrap()).unwrap();
    assert!(props.iter().any(|p| p.tag == "Message-ID" && p.value.contains("m1@example.com")));
    let raw = src.raw_mime(h[0].locator.as_ref().unwrap()).unwrap().unwrap();
    assert!(raw.starts_with(b"From: "));
    assert!(!raw.windows(6).any(|w| w == b">From "));
    assert!(raw.ends_with(b"From escaped line\n"));
}

#[test]
fn attachments_inline_nested_and_cross_format() {
    let o = open("basic.mbox");
    let src = &o.src;
    let h = all(src, "");
    let loc = h[2].locator.clone().unwrap();
    let m = src.message(&loc).unwrap();
    assert_eq!(m.body.text.as_deref().map(str::trim_end), Some("Plain body ünïcode"));
    let html = m.body.html.unwrap();
    assert!(html.contains("HTML body café"), "{html}");
    let a = &m.attachments;
    let names: Vec<_> = a.iter().map(|x| x.filename.clone().unwrap_or_default()).collect();
    assert_eq!(names, vec!["", "report.pdf", "Inner message.eml", "winmail.dat", "fwd.msg"]);
    for (i, x) in a.iter().enumerate() {
        assert_eq!(x.index, i as u32);
    }
    assert_eq!(a[0].content_id.as_deref(), Some("logo@x"));
    assert!(a[0].inline);
    assert_eq!(a[0].mime_type.as_deref(), Some("image/png"));
    assert!(!a[1].inline);
    assert_eq!(a[1].size, PDF.len() as u64);
    assert_eq!(a[2].kind, AttachmentKind::EmbeddedMessage);
    assert_eq!(a[3].kind, AttachmentKind::Tnef);
    assert_eq!(a[4].kind, AttachmentKind::EmbeddedMessage);
    assert_eq!(src.attachment(&loc, 0).unwrap(), PNG);
    assert_eq!(src.attachment(&loc, 1).unwrap(), PDF);
    assert_eq!(src.attachment(&loc, 3).unwrap(), b"\x78\x9f\x3e\x22\x00\x00\x01");
    assert!(matches!(src.attachment(&loc, 99), Err(Error::NotFound(_))));

    // Nested rfc822, two levels deep.
    let inner = Locator::Embedded { parent: Box::new(loc.clone()), attachment: 2 };
    let im = src.message(&inner).unwrap();
    assert_eq!(im.header.subject, "Inner message");
    assert_eq!(im.header.locator.as_ref(), Some(&inner));
    assert_eq!(im.body.text.as_deref().map(str::trim), Some("Inner body"));
    assert_eq!(im.attachments.len(), 2);
    assert_eq!(im.attachments[1].filename.as_deref(), Some("data.csv"));
    assert_eq!(src.attachment(&inner, 1).unwrap(), b"a,b\n1,2");
    let deep = Locator::Embedded { parent: Box::new(inner.clone()), attachment: 0 };
    let dm = src.message(&deep).unwrap();
    assert_eq!(dm.header.subject, "Deepest");
    assert_eq!(dm.body.text.as_deref().map(str::trim), Some("Deepest body"));
    let raw = src.raw_mime(&deep).unwrap().unwrap();
    assert!(raw.starts_with(b"From: Deep"));

    // Cross-format embedded items are left to stv-library.
    for idx in [3, 4, 1] {
        let e = Locator::Embedded { parent: Box::new(loc.clone()), attachment: idx };
        assert!(matches!(src.message(&e), Err(Error::Unsupported(_))), "attachment {idx}");
    }
}

#[test]
fn mime_helpers_match_source() {
    let bytes = std::fs::read(fixtures().join("single.eml")).unwrap();
    let m = stv_mbox::message_from_mime(&bytes, None, None).unwrap();
    assert_eq!(m.attachments.len(), 5);
    let h = stv_mbox::header_from_mime(&bytes, None, Some(FolderId("x".into()))).unwrap();
    assert_eq!(h.subject, "Attachments galore");
    assert_eq!(h.date, Some(ts(2022, 3, 2, 13, 30)));
    assert_eq!(h.folder, Some(FolderId("x".into())));
    assert_eq!(stv_mbox::attachment_from_mime(&bytes, 1).unwrap(), PDF);
    assert!(stv_mbox::message_from_mime(b"", None, None).is_err());

    let o = open("single.eml");
    assert_eq!(o.src.kind(), SourceKind::Eml);
    let h = all(&o.src, "");
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].locator, Some(Locator::File { path: String::new() }));
    let sm = o.src.message(h[0].locator.as_ref().unwrap()).unwrap();
    assert_eq!(sm.attachments, m.attachments);
}

#[test]
fn crlf_with_bom_and_blank_lines() {
    let o = open("crlf.mbox");
    let h = all(&o.src, "");
    assert_eq!(subjects(&h), vec!["CRLF message 0", "CRLF message 1", "CRLF message 2"]);
    let raw = o.src.raw_mime(h[1].locator.as_ref().unwrap()).unwrap().unwrap();
    assert!(raw.starts_with(b"From: Sender 1"));
    assert!(raw.ends_with(b"Body line 1\r\n"), "{:?}", String::from_utf8_lossy(&raw));
    let m = o.src.message(h[2].locator.as_ref().unwrap()).unwrap();
    assert_eq!(m.body.text.as_deref().map(str::trim_end), Some("Body line 2"));
}

#[test]
fn mboxrd_unescape() {
    let o = open("rd.mbox");
    let h = all(&o.src, "");
    assert_eq!(subjects(&h), vec!["mboxrd", "second"]);
    let raw = o.src.raw_mime(h[0].locator.as_ref().unwrap()).unwrap().unwrap();
    let s = String::from_utf8(raw).unwrap();
    assert!(s.contains("\nFrom quoted once\n"));
    assert!(s.contains("\n>From quoted twice\n"));
    assert!(s.contains("\n> From not an escape\n"));
}

#[test]
fn mboxcl2_content_length() {
    let o = open("cl2.mbox");
    let h = all(&o.src, "");
    assert_eq!(subjects(&h), vec!["cl2 one", "cl2 two", "wrong length", "after wrong"]);
    let raw = o.src.raw_mime(h[0].locator.as_ref().unwrap()).unwrap().unwrap();
    assert!(raw.ends_with(b"Line one\nFrom someone Mon Jan  1 00:00:00 2001\nLine three\n"));
    let m = o.src.message(h[2].locator.as_ref().unwrap()).unwrap();
    assert_eq!(m.body.text.as_deref().map(str::trim), Some("short body"));
}

#[test]
fn smime_encrypted_and_signed() {
    let o = open("smime.mbox");
    let h = all(&o.src, "");
    assert_eq!(subjects(&h), vec!["Encrypted", "Clear signed", "Opaque signed"]);
    assert!(h[0].flags.encrypted);
    assert!(h[1].flags.signed && h[2].flags.signed);
    let enc = o.src.message(h[0].locator.as_ref().unwrap()).unwrap();
    assert!(enc.header.flags.encrypted);
    assert!(enc.body.text.unwrap().contains("cannot decrypt"));
    assert_eq!(enc.attachments[0].filename.as_deref(), Some("smime.p7m"));
    let sig = o.src.message(h[1].locator.as_ref().unwrap()).unwrap();
    assert!(sig.header.flags.signed);
    assert_eq!(sig.body.text.as_deref().map(str::trim), Some("Signed visible text"));
    assert!(sig.attachments.iter().any(|a| a.filename.as_deref() == Some("smime.p7s")));
    let op = o.src.message(h[2].locator.as_ref().unwrap()).unwrap();
    assert!(op.header.flags.signed);
    assert_eq!(op.header.subject, "Opaque signed");
    assert_eq!(op.body.text.as_deref().map(str::trim), Some("Opaque signed text"));
    assert_eq!(op.attachments.len(), 1);
    assert_eq!(op.attachments[0].filename.as_deref(), Some("note.txt"));
    assert_eq!(o.src.attachment(h[2].locator.as_ref().unwrap(), 0).unwrap(), b"note");
}

#[test]
fn gmail_labels_and_threads() {
    let o = open("gmail.mbox");
    let folders = o.src.folders().unwrap();
    let find = |id: &str| folders.iter().find(|f| f.id.0 == id).unwrap_or_else(|| panic!("{id} in {folders:#?}"));
    assert_eq!(find("").name, "All Mail");
    assert_eq!(find("").message_count, Some(3));
    let inbox = find("label:Inbox");
    assert_eq!(
        (inbox.kind, inbox.message_count, inbox.parent.clone()),
        (FolderKind::Label, Some(2), Some(FolderId(String::new())))
    );
    assert_eq!(find("label:Work, Projects").message_count, Some(1));
    let sub = find("label:Work/Sub");
    assert_eq!(sub.parent, Some(FolderId("label:Work".into())));
    assert_eq!(sub.name, "Sub");
    assert_eq!(find("label:Work").message_count, Some(0));
    let ih = all(&o.src, "label:Inbox");
    assert_eq!(subjects(&ih), vec!["Thread start", "Re: Thread start"]);
    assert!(ih.iter().all(|h| h.folder == Some(FolderId("label:Inbox".into()))));
    let h = all(&o.src, "");
    assert_eq!(h[0].conversation.as_deref(), Some("1001"));
    assert_eq!(h[2].conversation.as_deref(), Some("1001"));
    assert_eq!(h[1].labels, vec!["Work, Projects", "Work/Sub", "Unread"]);
    assert!(!h[1].flags.read && h[0].flags.read);
    assert!(h[2].flags.flagged);
    assert_eq!(o.src.count(&FolderId("label:Work/Sub".into())).unwrap(), 1);
    assert!(matches!(o.src.count(&FolderId("label:Nope".into())), Err(Error::NotFound(_))));
}

#[test]
fn thunderbird_tree_flags_and_deleted() {
    let o = open("thunderbird");
    assert_eq!(o.src.kind(), SourceKind::Thunderbird);
    let folders = o.src.folders().unwrap();
    let ids: Vec<&str> = folders.iter().map(|f| f.id.0.as_str()).collect();
    for want in [
        "",
        "Mail",
        "Mail/Local Folders",
        "Mail/Local Folders/Inbox",
        "Mail/Local Folders/Inbox#deleted",
        "Mail/Local Folders/Inbox.sbd/Work",
        "Mail/Local Folders/Sent",
        "Mail/Local Folders/Sent.sbd/Archive",
        "Mail/Local Folders/Trash",
    ] {
        assert!(ids.contains(&want), "{want} missing from {ids:?}");
    }
    assert!(!ids
        .iter()
        .any(|i| i.contains("filterlog") || i.contains(".msf") || i.contains(".json") || i.contains("stray")));
    // Parents precede children.
    for (i, f) in folders.iter().enumerate() {
        if let Some(p) = &f.parent {
            assert!(folders[..i].iter().any(|x| &x.id == p), "{:?} before parent", f.id);
        }
    }
    let work = folders.iter().find(|f| f.id.0 == "Mail/Local Folders/Inbox.sbd/Work").unwrap();
    assert_eq!(work.parent, Some(FolderId("Mail/Local Folders/Inbox".into())));
    let inbox = all(&o.src, "Mail/Local Folders/Inbox");
    assert_eq!(subjects(&inbox), vec!["TB read", "TB unread", "TB flagged"]);
    assert!(inbox[0].flags.read && !inbox[1].flags.read && inbox[2].flags.flagged);
    let del_folder = folders.iter().find(|f| f.id.0.ends_with("#deleted")).unwrap();
    assert_eq!(del_folder.kind, FolderKind::Deleted);
    let del = all(&o.src, "Mail/Local Folders/Inbox#deleted");
    assert_eq!(subjects(&del), vec!["TB deleted"]);
    assert!(del[0].flags.deleted);
    assert_eq!(o.src.count(&FolderId("Mail/Local Folders/Trash".into())).unwrap(), 0);
    assert_eq!(o.src.count(&FolderId("Mail/Local Folders".into())).unwrap(), 0);
    match &inbox[0].locator {
        Some(Locator::Mbox { file, .. }) => assert_eq!(file, "Mail/Local Folders/Inbox"),
        other => panic!("{other:?}"),
    }
    let m = o.src.message(inbox[1].locator.as_ref().unwrap()).unwrap();
    assert_eq!(m.body.text.as_deref().map(str::trim), Some("Thunderbird body 2"));
    assert_eq!(m.header.folder, Some(FolderId("Mail/Local Folders/Inbox".into())));
    let dm = o.src.message(del[0].locator.as_ref().unwrap()).unwrap();
    assert_eq!(dm.header.folder, Some(FolderId("Mail/Local Folders/Inbox#deleted".into())));
    // iter_headers covers every message exactly once with folder + locator.
    let every: Vec<Header> = o.src.iter_headers().map(Result::unwrap).collect();
    assert_eq!(every.len(), 6);
    assert!(every.iter().all(|h| h.locator.is_some() && h.folder.is_some()));
}

#[test]
fn eml_directory_tree() {
    let o = open("emldir");
    assert_eq!(o.src.kind(), SourceKind::Eml);
    let folders = o.src.folders().unwrap();
    let ids: Vec<&str> = folders.iter().map(|f| f.id.0.as_str()).collect();
    assert_eq!(ids, vec!["", "sub", "sub/deeper"]);
    assert_eq!(subjects(&all(&o.src, "")), vec!["Top level"]);
    let deeper = all(&o.src, "sub/deeper");
    assert_eq!(subjects(&deeper), vec!["Deeper", "Upper ext"]);
    assert_eq!(deeper[0].locator, Some(Locator::File { path: "sub/deeper/c.eml".into() }));
    let m = o.src.message(deeper[0].locator.as_ref().unwrap()).unwrap();
    assert_eq!(m.body.text.as_deref().map(str::trim), Some("Eml body 3"));
    assert_eq!(m.header.folder, Some(FolderId("sub/deeper".into())));
    assert!(matches!(o.src.message(&Locator::File { path: "../basic.mbox".into() }), Err(Error::NotFound(_))));
}

#[test]
fn apple_mail_tree() {
    let o = open("apple");
    assert_eq!(o.src.kind(), SourceKind::AppleMail);
    let folders = o.src.folders().unwrap();
    let inbox_id = "V10/1A2B3C4D-0000-0000-0000-000000000001/INBOX.mbox";
    let work_id = "V10/1A2B3C4D-0000-0000-0000-000000000001/Work.mbox";
    let find = |id: &str| folders.iter().find(|f| f.id.0 == id).unwrap_or_else(|| panic!("{id} in {folders:#?}"));
    assert_eq!(find(inbox_id).name, "INBOX");
    assert_eq!(find(&format!("{work_id}/Sub.mbox")).parent, Some(FolderId(work_id.into())));
    assert!(!folders.iter().any(|f| f.id.0.contains("MailData")));
    let inbox = all(&o.src, inbox_id);
    let mut s = subjects(&inbox);
    s.sort();
    assert_eq!(s, vec!["Apple partial", "Apple partitioned flagged", "Apple read"]);
    let flagged = inbox.iter().find(|h| h.subject == "Apple partitioned flagged").unwrap();
    assert!(flagged.flags.flagged && flagged.flags.read);
    let partial = inbox.iter().find(|h| h.subject == "Apple partial").unwrap();
    assert!(!partial.flags.read);
    let del = all(&o.src, &format!("{inbox_id}#deleted"));
    assert_eq!(subjects(&del), vec!["Apple deleted"]);
    // Partial emlx: attachment body lives in Attachments/<id>/<section>/.
    let loc = partial.locator.clone().unwrap();
    let m = o.src.message(&loc).unwrap();
    assert_eq!(m.attachments.len(), 1);
    assert_eq!(m.attachments[0].filename.as_deref(), Some("report.pdf"));
    assert_eq!(m.attachments[0].size, PDF.len() as u64);
    assert_eq!(o.src.attachment(&loc, 0).unwrap(), PDF);
    let work = all(&o.src, work_id);
    assert_eq!(subjects(&work), vec!["Apple work"]);
    assert!(!work[0].flags.read);
    assert_eq!(subjects(&all(&o.src, &format!("{work_id}/Sub.mbox"))), vec!["Apple nested"]);

    // Exported bundle holding a plain mbox.
    let e = open("apple_export");
    let h = all(&e.src, "Archive.mbox");
    assert_eq!(subjects(&h), vec!["Exported one", "Exported two"]);
    assert!(matches!(&h[0].locator, Some(Locator::Mbox { file, .. }) if file == "Archive.mbox/mbox"));
}

#[test]
fn sorting_and_paging() {
    let o = open("thunderbird");
    let f = FolderId("Mail/Local Folders/Inbox".into());
    let by = |key, descending| {
        subjects(&o.src.list(&f, 0..10, Sort { key, descending }).unwrap())
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(by(SortKey::Date, false), vec!["TB read", "TB unread", "TB flagged"]);
    assert_eq!(by(SortKey::Date, true), vec!["TB flagged", "TB unread", "TB read"]);
    assert_eq!(by(SortKey::Subject, false), vec!["TB flagged", "TB read", "TB unread"]);
    assert_eq!(by(SortKey::From, true), vec!["TB flagged", "TB unread", "TB read"]);
    let page = o.src.list(&f, 1..2, Sort { key: SortKey::Date, descending: true }).unwrap();
    assert_eq!(subjects(&page), vec!["TB unread"]);
    assert!(o.src.list(&f, 5..9, Sort::default()).unwrap().is_empty());
    let sizes: Vec<u64> =
        o.src.list(&f, 0..3, Sort { key: SortKey::Size, descending: false }).unwrap().iter().map(|h| h.size).collect();
    assert!(sizes.windows(2).all(|w| w[0] <= w[1]));
}

#[test]
fn corrupt_inputs_do_not_panic() {
    let f = fixtures();
    assert!(MboxSource::open(&f.join("corrupt/garbage.bin"), &OpenOptions::default()).is_err());
    let t = open("corrupt/truncated.mbox");
    let h = all(&t.src, "");
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].subject, "cut off");
    let _ = t.src.message(h[0].locator.as_ref().unwrap());
    // Bogus locators.
    let bad = Locator::Mbox { file: String::new(), offset: 3, len: 10 };
    assert!(matches!(t.src.message(&bad), Err(Error::NotFound(_))));
    assert!(t.src.message(&Locator::Pst { nid: 1 }).is_err());
    // emlx with an overlong byte count is clamped; missing count line is a format error.
    let e = open("corrupt/bad.emlx");
    let h = all(&e.src, "");
    assert_eq!(h[0].subject, "short");
    assert_eq!(e.src.message(h[0].locator.as_ref().unwrap()).unwrap().body.text.as_deref(), Some("body"));
    let n = open("corrupt/nocount.emlx");
    let h = all(&n.src, "");
    assert!(matches!(n.src.message(h[0].locator.as_ref().unwrap()), Err(Error::Format(_))));
    // Random truncations of a real mailbox never panic.
    let data = std::fs::read(f.join("basic.mbox")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    for cut in (40..data.len()).step_by(97) {
        let p = dir.path().join(format!("cut{cut}"));
        std::fs::write(&p, &data[..cut]).unwrap();
        let src = MboxSource::open(&p, &OpenOptions { no_cache: true, ..Default::default() }).unwrap();
        for h in src.iter_headers() {
            let h = h.unwrap();
            let loc = h.locator.unwrap();
            if let Ok(m) = src.message(&loc) {
                for a in &m.attachments {
                    let _ = src.attachment(&loc, a.index);
                    let _ = src.message(&Locator::Embedded { parent: Box::new(loc.clone()), attachment: a.index });
                }
            }
        }
    }
}

/// Directory link to `target` (symlink on Unix, junction on Windows); `false` if the
/// platform/filesystem refuses.
fn dir_link(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link).is_ok();
    #[cfg(windows)]
    return std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|o| o.status.success());
}

#[test]
fn directory_link_cycles_are_walked_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/a.eml"), b"Subject: loop\n\nbody\n").unwrap();
    // Three links back to the root per level: without cycle detection this is 3^depth walks.
    for name in ["l1", "l2", "l3"] {
        if !dir_link(root, &root.join("sub").join(name)) {
            eprintln!("note: cannot create directory links here; skipping");
            return;
        }
    }
    let o = open_path(root);
    assert_eq!(subjects(&all(&o.src, "sub")), vec!["loop"]);
    assert_eq!(o.src.iter_headers().count(), 1);
}

#[test]
fn append_detection_cache_and_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let path = dir.path().join("live.mbox");
    std::fs::copy(fixtures().join("rd.mbox"), &path).unwrap();
    let opts = OpenOptions { cache_dir: Some(cache.path().to_path_buf()), no_cache: false };
    let src = MboxSource::open(&path, &opts).unwrap();
    assert_eq!(src.count(&FolderId(String::new())).unwrap(), 2);
    assert_eq!(src.load_outcomes(), vec!["fresh"]);
    let fp1 = src.fingerprint().unwrap();
    drop(src);

    let again = MboxSource::open(&path, &opts).unwrap();
    assert_eq!(again.count(&FolderId(String::new())).unwrap(), 2);
    assert_eq!(again.load_outcomes(), vec!["cached"]);

    // Append two messages; fingerprint changes, the cache is extended incrementally.
    let mut add = std::fs::read(&path).unwrap();
    add.extend_from_slice(b"\nFrom c@example.com Mon Feb  7 10:00:00 2022\nSubject: third\n\nx\n\nFrom d@example.com Mon Feb  7 11:00:00 2022\nSubject: fourth\n\ny\n");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&path, &add).unwrap();
    let fp2 = again.fingerprint().unwrap();
    assert_ne!(fp1, fp2);
    assert!(fp2.size > fp1.size);
    // iter_headers refreshes stale data.
    let subs: Vec<String> = again.iter_headers().map(|h| h.unwrap().subject).collect();
    assert_eq!(subs, vec!["mboxrd", "second", "third", "fourth"]);
    assert_eq!(again.load_outcomes(), vec!["incremental"]);
    assert_eq!(again.count(&FolderId(String::new())).unwrap(), 4);
}

/// `calendar-edge.mbox`: invites from different generators and their edge cases.
#[test]
fn calendar_edge_invites() {
    use mail_model::{ItemDetails, MeetingKind, TaskStatus, When};

    let o = open("calendar-edge.mbox");
    let rows = all(&o.src, "");
    assert_eq!(rows.len(), 19);
    let mut items = std::collections::HashMap::new();
    for row in &rows {
        let m = o.src.message(row.locator.as_ref().unwrap()).unwrap();
        // The cheap list-row flag agrees with the parsed message: only "Edge application ics"
        // carries a named calendar file; unnamed calendar parts are the invite body.
        assert_eq!(row.has_attachments, m.header.has_attachments, "{}", row.subject);
        assert_eq!(row.has_attachments, row.subject == "Edge application ics", "{}", row.subject);
        items.insert(row.subject.trim_start_matches("Edge ").to_owned(), m.header.item);
    }
    let appt = |name: &str| match items.get(name) {
        Some(Some(ItemDetails::Appointment(a))) => a.clone(),
        other => panic!("{name}: {other:?}"),
    };
    let span = |name: &str| {
        let a = appt(name);
        (a.start, a.end)
    };
    let t = |y, mo, d, h, mi| Some(When::Time(ts(y, mo, d, h, mi)));

    // Skipped 02:30 takes the pre-change offset; repeated 01:30 is its first occurrence.
    assert_eq!(span("DST gap"), (t(2026, 3, 8, 10, 30), t(2026, 3, 8, 10, 30)));
    assert_eq!(span("DST overlap"), (t(2026, 11, 1, 8, 30), t(2026, 11, 1, 10, 30)));
    let d = appt("date duration");
    assert!(d.all_day);
    assert_eq!((d.start, d.end), (Some(When::Date("2026-07-04".into())), Some(When::Date("2026-07-07".into()))));
    let master = appt("exception first");
    assert_eq!((master.start, master.end), (t(2026, 3, 2, 9, 0), t(2026, 3, 2, 10, 0)));
    assert_eq!(master.recurrence.as_deref(), Some("FREQ=WEEKLY;COUNT=5"));
    assert_eq!(appt("counter").meeting, Some(MeetingKind::Counter));
    assert_eq!(appt("tentative reply").meeting, Some(MeetingKind::Tentative));
    assert_eq!(appt("lowercase props").location.as_deref(), Some("Lowercase lagoon"));
    assert_eq!(appt("tab fold").location.as_deref(), Some("Tabfolded tapir hall"));
    assert_eq!(span("quoted tzid"), (t(2026, 6, 1, 16, 0), t(2026, 6, 1, 17, 0)));
    assert_eq!(span("end before start"), (t(2026, 6, 1, 10, 0), None));
    assert_eq!(appt("iana no vtz Tokyo").start, t(2026, 6, 1, 0, 0));
    assert_eq!(appt("windows name no vtz").start, t(2026, 6, 1, 7, 0));
    assert_eq!(appt("utc offset tzid").start, t(2026, 6, 1, 7, 0), "the file's VTIMEZONE");
    let no_start = appt("no dtstart");
    assert_eq!((no_start.start, no_start.meeting), (None, Some(MeetingKind::Request)));
    assert_eq!(appt("application ics").start, t(2026, 4, 4, 9, 0));
    assert_eq!(span("vcalendar 1.0"), (t(2026, 4, 5, 9, 0), t(2026, 4, 5, 10, 0)));
    assert_eq!(items["garbage calendar"], None);

    let Some(Some(ItemDetails::Task(task))) = items.get("only vtodo completed") else { panic!("no task") };
    assert_eq!((task.status, task.percent_complete), (Some(TaskStatus::Complete), Some(100)));
    assert_eq!(
        (task.due.clone(), task.completed.clone()),
        (Some(When::Date("2026-12-01".into())), Some(When::Date("2026-11-30".into())))
    );

    let roles = appt("attendee roles");
    let emails = |v: &[mail_model::Address]| v.iter().filter_map(|a| a.email.clone()).collect::<Vec<_>>();
    assert_eq!(emails(&roles.required), ["alice@example.org"], "organizer (CHAIR) not repeated");
    assert_eq!(emails(&roles.optional), ["bob@example.org"]);
    assert_eq!(emails(&roles.resources), ["carol@example.org", "projector@example.org", "room2b@example.org"]);
    assert_eq!(roles.meeting, Some(MeetingKind::Request));
}
