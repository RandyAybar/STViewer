//! Test-only in-memory `MailSource`.
#![allow(dead_code)]

use std::collections::HashSet;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use mail_model::*;

#[derive(Clone, Default)]
pub struct FakeMsg {
    pub nid: u32,
    pub header: Header,
    pub body: Body,
    pub attachments: Vec<(AttachmentMeta, Vec<u8>)>,
    /// Natively embedded messages by attachment index.
    pub embedded: Vec<(u32, Message)>,
    /// `message()` returns this as `header.item`: list rows carry fewer details.
    pub full_item: Option<ItemDetails>,
}

pub struct FakeSource {
    pub msgs: RwLock<Vec<FakeMsg>>,
    pub fingerprint: RwLock<Fingerprint>,
    /// `message()` fails for these nids.
    pub broken: RwLock<HashSet<u32>>,
    /// `message()` panics for these nids (a reader bug on malformed input).
    pub panicking: RwLock<HashSet<u32>>,
    /// When set, `iter_headers` panics after yielding `n` headers.
    pub listing_panics_after: RwLock<Option<usize>>,
    pub message_calls: AtomicUsize,
    /// When set, flips the flag after `n` top-level `message()` calls.
    pub cancel_after: RwLock<Option<(usize, Arc<AtomicBool>)>>,
    pub folders: Vec<Folder>,
}

pub fn folder(id: &str, parent: Option<&str>, name: &str) -> Folder {
    Folder {
        id: FolderId(id.into()),
        parent: parent.map(|p| FolderId(p.into())),
        name: name.into(),
        kind: FolderKind::Mail,
        message_count: None,
        unread_count: None,
    }
}

impl FakeSource {
    pub fn new(msgs: Vec<FakeMsg>) -> Self {
        FakeSource {
            msgs: RwLock::new(msgs),
            fingerprint: RwLock::new(Fingerprint { path: PathBuf::from("fake.pst"), size: 1, mtime: 1, head_hash: 1 }),
            broken: RwLock::new(HashSet::new()),
            panicking: RwLock::new(HashSet::new()),
            listing_panics_after: RwLock::new(None),
            message_calls: AtomicUsize::new(0),
            cancel_after: RwLock::new(None),
            folders: vec![
                folder("inbox", None, "Inbox"),
                folder("projects", Some("inbox"), "Projects"),
                folder("sent", None, "Sent Items"),
            ],
        }
    }

    pub fn touch(&self) {
        let mut fp = self.fingerprint.write().unwrap();
        fp.mtime += 1;
        fp.size += 1;
    }

    pub fn calls(&self) -> usize {
        self.message_calls.load(Ordering::SeqCst)
    }

    fn find(&self, nid: u32) -> Option<FakeMsg> {
        self.msgs.read().unwrap().iter().find(|m| m.nid == nid).cloned()
    }
}

impl MailSource for FakeSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Pst
    }
    fn display_name(&self) -> String {
        "fake".into()
    }
    fn fingerprint(&self) -> Result<Fingerprint> {
        Ok(self.fingerprint.read().unwrap().clone())
    }
    fn folders(&self) -> Result<Vec<Folder>> {
        Ok(self.folders.clone())
    }
    fn count(&self, folder: &FolderId) -> Result<u64> {
        Ok(self.msgs.read().unwrap().iter().filter(|m| m.header.folder.as_ref() == Some(folder)).count() as u64)
    }
    fn list(&self, folder: &FolderId, range: Range<u64>, _sort: Sort) -> Result<Vec<Header>> {
        Ok(self
            .msgs
            .read()
            .unwrap()
            .iter()
            .filter(|m| m.header.folder.as_ref() == Some(folder))
            .skip(range.start as usize)
            .take((range.end - range.start) as usize)
            .map(|m| m.header.clone())
            .collect())
    }
    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_> {
        let headers: Vec<Header> = self.msgs.read().unwrap().iter().map(|m| m.header.clone()).collect();
        let panic_after = *self.listing_panics_after.read().unwrap();
        Box::new(headers.into_iter().enumerate().map(move |(i, h)| {
            assert!(panic_after != Some(i), "listing panicked after {i} headers");
            Ok(h)
        }))
    }
    fn message(&self, loc: &Locator) -> Result<Message> {
        match loc {
            Locator::Pst { nid } => {
                let n = self.message_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some((after, flag)) = &*self.cancel_after.read().unwrap() {
                    if n >= *after {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
                if self.broken.read().unwrap().contains(nid) {
                    return Err(Error::Format(format!("broken nid {nid}")));
                }
                assert!(!self.panicking.read().unwrap().contains(nid), "reader panicked on nid {nid}");
                let m = self.find(*nid).ok_or_else(|| Error::NotFound(format!("nid {nid}")))?;
                let mut header = m.header.clone();
                if m.full_item.is_some() {
                    header.item = m.full_item.clone();
                }
                Ok(Message {
                    header,
                    body: m.body.clone(),
                    attachments: m.attachments.iter().map(|(a, _)| a.clone()).collect(),
                    raw_headers: None,
                })
            }
            Locator::Embedded { parent, attachment } => {
                let Locator::Pst { nid } = **parent else { return Err(Error::Unsupported("nested".into())) };
                let m = self.find(nid).ok_or_else(|| Error::NotFound(format!("nid {nid}")))?;
                m.embedded
                    .iter()
                    .find(|(i, _)| i == attachment)
                    .map(|(_, e)| e.clone())
                    .ok_or_else(|| Error::Unsupported("not an embedded message".into()))
            }
            _ => Err(Error::NotFound("locator".into())),
        }
    }
    fn attachment(&self, loc: &Locator, index: u32) -> Result<Vec<u8>> {
        let Locator::Pst { nid } = loc else { return Err(Error::NotFound("locator".into())) };
        let m = self.find(*nid).ok_or_else(|| Error::NotFound(format!("nid {nid}")))?;
        m.attachments
            .iter()
            .find(|(a, _)| a.index == index)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| Error::NotFound(format!("attachment {index}")))
    }
    fn raw_mime(&self, _loc: &Locator) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn properties(&self, _loc: &Locator) -> Result<Vec<PropertyView>> {
        Ok(Vec::new())
    }
}

// ---- message builders ----------------------------------------------------------------------

pub fn addr(name: &str, email: &str) -> Address {
    Address {
        name: (!name.is_empty()).then(|| name.to_string()),
        email: (!email.is_empty()).then(|| email.to_string()),
    }
}

/// UTC ms for `YYYY-MM-DD` at 12:00.
pub fn day(s: &str) -> i64 {
    let mut p = s.split('-').map(|x| x.parse::<u32>().unwrap());
    let (y, m, d) = (p.next().unwrap(), p.next().unwrap(), p.next().unwrap());
    mail_model::time::days_from_civil(i64::from(y), m, d) * mail_model::time::MS_PER_DAY + 12 * 3_600_000
}

pub fn msg(nid: u32, folder: &str, subject: &str, from: Address, date: &str, body: &str) -> FakeMsg {
    FakeMsg {
        nid,
        header: Header {
            locator: Some(Locator::Pst { nid }),
            folder: Some(FolderId(folder.into())),
            subject: subject.into(),
            from: Some(from),
            to: vec![addr("Bob Jones", "bob@corp.example")],
            date: Some(day(date)),
            size: 2_000,
            flags: MessageFlags { read: true, ..Default::default() },
            ..Default::default()
        },
        body: Body { text: Some(body.into()), ..Default::default() },
        ..Default::default()
    }
}

impl FakeMsg {
    pub fn attach(mut self, name: &str, mime: Option<&str>, bytes: Vec<u8>) -> Self {
        let index = self.attachments.len() as u32;
        self.attachments.push((
            AttachmentMeta {
                index,
                filename: Some(name.into()),
                mime_type: mime.map(Into::into),
                size: bytes.len() as u64,
                ..Default::default()
            },
            bytes,
        ));
        self.header.has_attachments = true;
        self
    }
    pub fn with(mut self, f: impl FnOnce(&mut Header)) -> Self {
        f(&mut self.header);
        self
    }
}

pub fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, body) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// Minimal valid one-page PDF (Helvetica text) with a correct xref table.
pub fn tiny_pdf(text: &str) -> Vec<u8> {
    let stream = format!("BT /F1 12 Tf 72 712 Td ({text}) Tj ET");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
    );
    out
}

pub fn noop(_: IndexProgress) {}
