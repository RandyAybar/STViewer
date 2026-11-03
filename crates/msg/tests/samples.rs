//! Real-world `.msg` / TNEF files from the Apache POI test corpus (Apache-2.0, see
//! `testdata/msg/README.md`). Expected values mirror the assertions in POI's own tests.

use std::path::{Path, PathBuf};

use mail_model::{AttachmentKind, Error, Locator, MailSource, Message, SourceKind};
use mapi_props::{PropKey, PropValue};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/msg/poi").join(name)
}

fn open(name: &str) -> Box<dyn MailSource> {
    stv_msg::opener().open(&sample(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn root() -> Locator {
    Locator::File { path: String::new() }
}

fn load(name: &str) -> Message {
    open(name).message(&root()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn emails(m: &Message) -> Vec<String> {
    m.header.to.iter().filter_map(|a| a.email.clone()).collect()
}

#[test]
fn quick_msg() {
    let m = load("quick.msg");
    assert_eq!(m.header.subject, "Test the content transformer");
    assert_eq!(m.body.text.as_deref(), Some("The quick brown fox jumps over the lazy dog\r\n"));
    assert_eq!(emails(&m), ["kevin.roast@alfresco.org"]);
    assert_eq!(m.header.from.as_ref().unwrap().name.as_deref(), Some("Kevin Roast"));
    assert!(m.attachments.is_empty());
}

#[test]
fn simple_test_msg() {
    let m = load("simple_test_msg.msg");
    assert_eq!(m.header.subject, "test message");
    assert_eq!(m.body.text.as_deref(), Some("This is a test message."));
    assert_eq!(emails(&m), ["travis@overwrittenstack.com"]);
    let headers = m.raw_headers.as_deref().unwrap();
    assert!(headers.starts_with("Return-path:"));
    assert!(headers.contains("Envelope-to: travis@overwrittenstack.com"));
}

#[test]
fn attachment_test_msg() {
    let src = open("attachment_test_msg.msg");
    let m = src.message(&root()).unwrap();
    assert_eq!(m.header.subject, "test pièce jointe 1");
    assert!(m.body.text.as_deref().unwrap().starts_with("contenu\r\n\r\n"));
    assert_eq!(emails(&m), ["nicolas1.23456@free.fr"]);
    let names: Vec<_> = m.attachments.iter().map(|a| a.filename.clone().unwrap()).collect();
    assert_eq!(names, ["test-unicode.doc", "pj1.txt"]);
    assert_eq!(m.attachments[0].size, 24064);
    assert_eq!(src.attachment(&root(), 0).unwrap().len(), 24064);
    let txt = src.attachment(&root(), 1).unwrap();
    assert_eq!(txt.len(), 89);
}

#[test]
fn embedded_message_in_msg() {
    let src = open("attachment_msg_pdf.msg");
    let m = src.message(&root()).unwrap();
    assert_eq!(m.attachments.len(), 2);
    let pdf = m.attachments.iter().find(|a| a.filename.as_deref() == Some("smbprn.00009008.KdcPjl.pdf")).unwrap();
    let pdf_bytes = src.attachment(&root(), pdf.index).unwrap();
    assert_eq!(pdf_bytes.len(), 13539);
    assert!(pdf_bytes.starts_with(b"%PDF"));
    let emb = m.attachments.iter().find(|a| a.kind == AttachmentKind::EmbeddedMessage).unwrap();
    let loc = Locator::Embedded { parent: Box::new(root()), attachment: emb.index };
    let inner = src.message(&loc).unwrap();
    assert_eq!(inner.header.subject, "Test Attachment");
    assert_eq!(inner.header.to.len(), 1);
    assert_eq!(inner.header.to[0].name.as_deref(), Some("Nick Booth"));
    // The embedded message re-serialises into a standalone .msg with identical content.
    let standalone = src.attachment(&root(), emb.index).unwrap();
    let again = stv_msg::message_from_msg_bytes(&standalone, None).unwrap();
    assert_eq!(again.header.subject, "Test Attachment");
    assert_eq!(again.body, inner.body);
    assert_eq!(again.header.date, inner.header.date);
    // Sizes: .msg files store neither PR_MESSAGE_SIZE nor (for embedded messages) PR_ATTACH_SIZE.
    let file_len = std::fs::metadata(sample("attachment_msg_pdf.msg")).unwrap().len();
    assert_eq!(m.header.size, file_len);
    assert!(emb.size > 0, "embedded message attachment reports its storage size");
    assert_eq!(inner.header.size, emb.size, "opened embedded message matches its attachment row");
    assert_eq!(again.header.size, standalone.len() as u64, "in-memory .msg falls back to its byte length");
}

#[test]
fn keywords_named_property() {
    let m = load("keywords.msg");
    assert_eq!(m.header.subject, "Test Keywords");
    assert_eq!(m.header.labels, ["TODO", "Currently Important", "Currently To Do", "Test"]);
    // PidLidCurrentVersionName (PSETID_Common, 0x8554) resolved through __nameid_version1.0.
    let raw = stv_msg::decode_mapi(&std::fs::read(sample("keywords.msg")).unwrap(), &[]).unwrap();
    let key =
        mapi_props::PropKey::Named { guid: mapi_props::names::PSETID_COMMON, id: mapi_props::NamedId::Lid(0x8554) };
    assert_eq!(raw.props[&key].value, mapi_props::PropValue::String("16.0".into()));
}

#[test]
fn unicode_and_codepage_samples() {
    assert!(load("example_received_unicode.msg").body.text.unwrap().starts_with("..less you are Nick....."));

    // Apache POI Test7BitCodepage expectations.
    let m = load("ASCII_CP1251_LCID1049.msg");
    assert_eq!(m.header.subject, "Subject автоматически Subject");
    assert_eq!(m.body.text.as_deref(), Some("Body автоматически Body"));
    assert_eq!(
        m.body.html.as_deref(),
        Some("<!DOCTYPE html><html><meta charset=\\\"windows-1251\\\"><body>HTML автоматически</body></html>")
    );
    let m = load("ASCII_UTF-8_CP1252_LCID1031.msg");
    assert_eq!(m.header.subject, "Subject öäü Subject");
    assert_eq!(m.body.text.as_deref(), Some("Body öäü Body"));
    let m = load("ASCII_UTF-8_CP1252_LCID1031_HTML.msg");
    assert_eq!(m.header.subject, "Subject öäü Subject");
    assert_eq!(
        m.body.html.as_deref(),
        Some("<!DOCTYPE html><html><meta charset=\\\"utf-8\\\"><body>HTML öäü</body></html>")
    );
    let m = load("HTMLBodyBinary_CP1251.msg");
    assert_eq!(m.header.subject, "Subject öäü Subject");
    assert_eq!(
        m.body.html.as_deref(),
        Some("<!DOCTYPE html><html><meta charset=\\\"utf-8\\\"><body>HTML автоматически</body></html>")
    );
    let m = load("HTMLBodyBinary_UTF-8.msg");
    assert_eq!(m.header.subject, "Subject öäü Subject");
    assert_eq!(
        m.body.html.as_deref(),
        Some("<!DOCTYPE html><html><meta charset=\\\"utf-8\\\"><body>HTML öäü</body></html>")
    );

    let m = load("cyrillic_message.msg");
    assert!(m.header.subject.starts_with("Автоматический ответ подсистемы"));
    assert_eq!(m.header.to[0].name.as_deref(), Some("Свиридов Дмитрий Владимирович"));
    let m = load("chinese-traditional.msg");
    assert_eq!(m.header.subject, "Alfresco MSG format testing ( MSG 格式測試 )");
    assert!(m.body.html.unwrap().contains("text/html; charset=big5"));
}

#[test]
fn appointment_class() {
    let m = load("msgClassAppointment.msg");
    assert_eq!(m.header.message_class.as_deref(), Some("IPM.Appointment"));
    assert_eq!(m.header.subject, "Quick brown fox");
    let Some(mail_model::ItemDetails::Appointment(a)) = &m.header.item else {
        panic!("no appointment details: {:?}", m.header.item)
    };
    // Values as Outlook shows them for this sample (2017-02-28 18:30–19:00 UTC).
    assert_eq!(a.start, Some(mail_model::When::Time(1_488_306_600_000)));
    assert_eq!(a.end, Some(mail_model::When::Time(1_488_308_400_000)));
    assert_eq!(a.location.as_deref(), Some("under lazy dog"));
    assert_eq!(a.busy_status, Some(mail_model::BusyStatus::Busy));
    assert!(a.uid.as_deref().is_some_and(|u| u.starts_with("040000008200E000")));
}

#[test]
fn quick_winmail_dat() {
    let src = open("quick-winmail.dat");
    assert_eq!(src.kind(), SourceKind::Tnef);
    let m = src.message(&root()).unwrap();
    assert_eq!(m.header.message_class.as_deref(), Some("IPM.Note"), "IPM.Microsoft Mail.Note mapped");
    // Only PidTagConversationTopic carries it; convert falls back to it (POI: getSubject()).
    assert_eq!(m.header.subject, "This is a test message");
    let raw = stv_msg::decode_mapi(&std::fs::read(sample("quick-winmail.dat")).unwrap(), &[]).unwrap();
    assert_eq!(
        raw.props[&mapi_props::PropKey::Id(0x0070)].value,
        mapi_props::PropValue::String("This is a test message".into())
    );
    // PidTagRtfCompressed survives TNEF decoding byte-exact (POI compares with message.rtf).
    let PropValue::Binary(rtf) = &raw.props[&PropKey::Id(0x1009)].value else { panic!("RTF not binary") };
    let rtf = mapi_props::rtf::decompress(rtf).unwrap();
    assert_eq!(rtf, std::fs::read(sample("quick-contents/message.rtf")).unwrap());
    assert!(m.body.text.as_deref().unwrap().starts_with("These are five files."));
    let names: Vec<_> = m.attachments.iter().map(|a| a.filename.clone().unwrap()).collect();
    assert_eq!(names, ["quick.doc", "quick.html", "quick.pdf", "quick.txt", "quick.xml"]);
    let txt = src.attachment(&root(), 3).unwrap();
    let expected = std::fs::read(sample("quick-contents/quick.txt")).unwrap();
    assert_eq!(txt, expected);
    assert_eq!(m.attachments[3].size, expected.len() as u64);
    // attAttachModifyDate: 28-Apr-2010 12:40:56 (POI TestAttachments).
    let a = &raw.attachments[3].props[&mapi_props::PropKey::Id(0x3008)];
    assert_eq!(a.value, mapi_props::PropValue::SysTime(1_272_458_456_000));
}

#[test]
fn other_winmail_samples() {
    let m = load("winmail-sample1.dat");
    assert_eq!(m.attachments.len(), 2);
    assert_eq!(m.attachments[0].filename.as_deref(), Some("zappa_av1.jpg"));
    let m = load("bug52400-winmail-simple.dat");
    assert_eq!(m.header.subject, "Testing TNEF Message");
    assert_eq!(m.header.from.as_ref().unwrap().email.as_deref(), Some("Gaggletest2@anderson5.net"));
    assert!(m.body.html.unwrap().contains("<html>"));
}

#[test]
fn corrupt_samples_fail_cleanly() {
    // POI's out-of-memory regression: a tiny TNEF claiming a huge property.
    let r = open("oom.tnef").message(&root());
    assert!(matches!(r, Err(Error::Format(_))), "{r:?}");
    // OSS-Fuzz minimised .msg with a malformed FAT.
    let src = open("clusterfuzz-testcase-minimized-POIHSMFFuzzer-4735011465854976.msg");
    assert!(matches!(src.message(&root()), Err(Error::Format(_))));
    assert!(matches!(src.attachment(&root(), 0), Err(Error::Format(_))));
}

#[test]
fn sniff_samples() {
    let op = stv_msg::opener();
    assert_eq!(op.sniff(&sample("quick.msg")), Some(SourceKind::Msg));
    assert_eq!(op.sniff(&sample("quick-winmail.dat")), Some(SourceKind::Tnef));
    assert_eq!(op.sniff(&sample("quick-contents/quick.txt")), None);
    assert_eq!(op.sniff(&sample("poi")), None, "missing path");
}
