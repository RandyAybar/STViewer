//! `.msg` decoding against files built by the test-only writer in `common/`.

mod common;

use common::*;
use mail_model::{AttachmentKind, Error, Locator};
use mapi_props::{NamedId, PropKey, PropValue};

const DATE_MS: i64 = 1_700_000_000_000;

fn named_name(guid: [u8; 16], name: &str) -> PropKey {
    PropKey::Named { guid, id: NamedId::Name(name.into()) }
}

fn inner_msg() -> MsgB {
    MsgB::default()
        .prop(0x001A, V::String8(b"IPM.Note".to_vec()))
        // "Привет мир" in windows-1251, decoded with the embedded message's own codepage.
        .prop(0x0037, V::String8(vec![0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, 0x20, 0xEC, 0xE8, 0xF0]))
        .prop(0x3FFD, V::I32(1251))
        .prop(0x1000, V::Unicode("inner body".into()))
        .named(K::Name(PS_PUBLIC_STRINGS, "Keywords".into()), V::MultiUnicode(vec!["Inner".into()]))
        .with_attachment(AttB::File(vec![
            (K::Id(0x3707), V::Unicode("inner.bin".into())),
            (K::Id(0x3705), V::I32(1)),
            (K::Id(0x3701), V::Binary(vec![0, 1, 2, 255])),
        ]))
}

fn recip(name: &str, email: &str, ty: i32) -> Vec<(K, V)> {
    vec![
        (K::Id(0x3001), V::Unicode(name.into())),
        (K::Id(0x3003), V::Unicode(email.into())),
        (K::Id(0x3002), V::Unicode("SMTP".into())),
        (K::Id(0x39FE), V::Unicode(email.into())),
        (K::Id(0x0C15), V::I32(ty)),
    ]
}

pub fn sample_msg() -> MsgB {
    MsgB::default()
        .prop(0x001A, V::Unicode("IPM.Note".into()))
        .prop(0x0037, V::Unicode("Héllo wörld ✓".into()))
        .prop(0x0C1A, V::Unicode("Alice Example".into()))
        .prop(0x0C1E, V::Unicode("SMTP".into()))
        .prop(0x0C1F, V::Unicode("alice@example.com".into()))
        .prop(0x0042, V::Unicode("Alice Example".into()))
        .prop(0x0064, V::Unicode("SMTP".into()))
        .prop(0x0065, V::Unicode("alice@example.com".into()))
        .prop(0x0039, V::Time(filetime(DATE_MS)))
        .prop(0x0E06, V::Time(filetime(DATE_MS)))
        .prop(0x0E07, V::I32(1))
        .prop(0x1035, V::Unicode("<abc@example.com>".into()))
        .prop(0x1000, V::Unicode("Plain body text".into()))
        .prop(0x1013, V::Binary(b"<p>HTML body</p>".to_vec()))
        .prop(0x3FDE, V::I32(65001))
        .prop(0x0017, V::I32(2))
        .prop(0x0E1B, V::Bool(true))
        .named(K::Name(PS_PUBLIC_STRINGS, "Keywords".into()), V::MultiUnicode(vec!["Red".into(), "Blue".into()]))
        .named(K::Lid(PSETID_COMMON, 0x8554), V::Unicode("16.0".into()))
        .named(K::Lid(PSETID_COMMON, 0x8501), V::MultiI32(vec![1, 2, 3]))
        .named(K::Name(ms_guid(0x1234_5678), "X-Custom".into()), V::Binary(vec![9, 8, 7]))
        .with_recipient(recip("Bob", "bob@example.com", 1))
        .with_recipient(recip("Carol", "carol@example.com", 2))
        .with_recipient(recip("Dave", "dave@example.com", 3))
        .with_attachment(AttB::File(vec![
            (K::Id(0x3707), V::Unicode("report.txt".into())),
            (K::Id(0x3704), V::Unicode("REPORT.TXT".into())),
            (K::Id(0x370E), V::Unicode("text/plain".into())),
            (K::Id(0x3705), V::I32(1)),
            (K::Id(0x3701), V::Binary(b"hello attachment\r\n".to_vec())),
        ]))
        .with_attachment(AttB::Embedded(vec![(K::Id(0x3001), V::Unicode("Inner message".into()))], inner_msg()))
        .with_attachment(AttB::File(vec![
            (K::Id(0x3707), V::Unicode("pic.png".into())),
            (K::Id(0x370E), V::Unicode("image/png".into())),
            (K::Id(0x3712), V::Unicode("img1@x".into())),
            (K::Id(0x3705), V::I32(1)),
            (K::Id(0x3701), V::Binary(vec![0x89, b'P', b'N', b'G'])),
        ]))
}

#[test]
fn unicode_message_roundtrip() {
    let bytes = build_msg(&sample_msg());
    let m = stv_msg::message_from_msg_bytes(&bytes, None).unwrap();
    let h = &m.header;
    assert_eq!(h.subject, "Héllo wörld ✓");
    assert_eq!(h.message_class.as_deref(), Some("IPM.Note"));
    let from = h.from.as_ref().unwrap();
    assert_eq!(from.name.as_deref(), Some("Alice Example"));
    assert_eq!(from.email.as_deref(), Some("alice@example.com"));
    let emails = |v: &[mail_model::Address]| v.iter().map(|a| a.email.clone().unwrap_or_default()).collect::<Vec<_>>();
    assert_eq!(emails(&h.to), ["bob@example.com"]);
    assert_eq!(emails(&h.cc), ["carol@example.com"]);
    assert_eq!(emails(&h.bcc), ["dave@example.com"]);
    assert_eq!(h.to[0].name.as_deref(), Some("Bob"));
    assert_eq!(h.date, Some(DATE_MS));
    assert_eq!(h.message_id.as_deref(), Some("abc@example.com"));
    assert_eq!(h.labels, ["Red", "Blue"]);
    assert!(h.flags.read);
    assert!(h.has_attachments);
    assert_eq!(m.body.text.as_deref(), Some("Plain body text"));
    assert_eq!(m.body.html.as_deref(), Some("<p>HTML body</p>"));

    assert_eq!(m.attachments.len(), 3);
    let a0 = &m.attachments[0];
    assert_eq!(
        (a0.index, a0.filename.as_deref(), a0.mime_type.as_deref()),
        (0, Some("report.txt"), Some("text/plain"))
    );
    assert_eq!(a0.size, 18);
    assert_eq!(a0.kind, AttachmentKind::File);
    assert_eq!(m.attachments[1].kind, AttachmentKind::EmbeddedMessage);
    assert_eq!(m.attachments[1].index, 1);
    assert_eq!(m.attachments[2].content_id.as_deref(), Some("img1@x"));
    assert_eq!(m.attachments[2].size, 4);
    // Property inspector sees the named + fixed properties.
    let props = stv_msg::properties_from_msg_bytes(&bytes).unwrap();
    assert!(props.iter().any(|p| p.tag.starts_with("0x0037")));
    assert!(props.iter().any(|p| p.name == "PidNameKeywords"));
}

#[test]
fn named_and_multivalued_properties() {
    let bytes = build_msg(&sample_msg());
    let raw = stv_msg::decode_mapi(&bytes, &[]).unwrap();
    let get = |k: &PropKey| raw.props.get(k).map(|p| p.value.clone());
    assert_eq!(
        get(&named_name(PS_PUBLIC_STRINGS, "Keywords")),
        Some(PropValue::MultiString(vec!["Red".into(), "Blue".into()]))
    );
    assert_eq!(
        get(&PropKey::Named { guid: PSETID_COMMON, id: NamedId::Lid(0x8554) }),
        Some(PropValue::String("16.0".into()))
    );
    assert_eq!(
        get(&PropKey::Named { guid: PSETID_COMMON, id: NamedId::Lid(0x8501) }),
        Some(PropValue::MultiInt32(vec![1, 2, 3]))
    );
    assert_eq!(get(&named_name(ms_guid(0x1234_5678), "X-Custom")), Some(PropValue::Binary(vec![9, 8, 7])));
    assert_eq!(get(&PropKey::Id(0x0E1B)), Some(PropValue::Boolean(true)));
    assert_eq!(get(&PropKey::Id(0x0039)), Some(PropValue::SysTime(DATE_MS)));
    assert_eq!(raw.recipients.len(), 3);
    // Attachment payloads are not materialised in the property maps.
    assert!(raw.attachments.iter().all(|a| !a.props.contains_key(&PropKey::Id(0x3701))
        || matches!(a.props[&PropKey::Id(0x3701)].value, PropValue::Object(_))));

    // Embedded message shares the root named-property map and uses its own codepage.
    let inner = stv_msg::decode_mapi(&bytes, &[1]).unwrap();
    assert_eq!(inner.props.get(&PropKey::Id(0x0037)).unwrap().value, PropValue::String("Привет мир".into()));
    assert_eq!(
        inner.props.get(&named_name(PS_PUBLIC_STRINGS, "Keywords")).unwrap().value,
        PropValue::MultiString(vec!["Inner".into()])
    );
}

#[test]
fn attachments_and_embedded_extraction() {
    let bytes = build_msg(&sample_msg());
    assert_eq!(stv_msg::attachment_from_msg_bytes(&bytes, 0).unwrap(), b"hello attachment\r\n");
    assert_eq!(stv_msg::attachment_from_msg_bytes(&bytes, 2).unwrap(), [0x89, b'P', b'N', b'G']);
    assert!(matches!(stv_msg::attachment_from_msg_bytes(&bytes, 3), Err(Error::NotFound(_))));

    // The embedded message comes back as a standalone, re-openable .msg.
    let inner = stv_msg::attachment_from_msg_bytes(&bytes, 1).unwrap();
    assert!(inner.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]));
    let m = stv_msg::message_from_msg_bytes(&inner, None).unwrap();
    assert_eq!(m.header.subject, "Привет мир");
    assert_eq!(m.header.labels, ["Inner"], "named map copied into the extracted file");
    assert_eq!(m.body.text.as_deref(), Some("inner body"));
    assert_eq!(m.attachments.len(), 1);
    assert_eq!(m.attachments[0].filename.as_deref(), Some("inner.bin"));
    assert_eq!(stv_msg::attachment_from_msg_bytes(&inner, 0).unwrap(), [0, 1, 2, 255]);
}

#[test]
fn string8_codepages() {
    // cp1252 via PidTagMessageCodepage, incl. multi-valued String8 and recipients.
    let m = MsgB::default()
        .prop(0x3FFD, V::I32(1252))
        .prop(0x0037, V::String8(b"caf\xe9 \x80".to_vec()))
        .prop(0x0C1A, V::String8(b"Ren\xe9e".to_vec()))
        .prop(0x0C1F, V::String8(b"renee@example.com".to_vec()))
        .named(
            K::Name(PS_PUBLIC_STRINGS, "Keywords".into()),
            V::MultiString8(vec![b"\xdcber".to_vec(), b"na\xefve".to_vec()]),
        )
        .with_recipient(vec![(K::Id(0x3001), V::String8(b"J\xfcrgen".to_vec())), (K::Id(0x0C15), V::I32(1))]);
    let msg = stv_msg::message_from_msg_bytes(&build_msg(&m), None).unwrap();
    assert_eq!(msg.header.subject, "café €");
    assert_eq!(msg.header.from.as_ref().unwrap().name.as_deref(), Some("Renée"));
    assert_eq!(msg.header.labels, ["Über", "naïve"]);
    assert_eq!(msg.header.to[0].name.as_deref(), Some("Jürgen"));

    // No codepage property: the locale (1049 = Russian) selects windows-1251.
    let m = MsgB::default().prop(0x3FF1, V::I32(1049)).prop(0x0037, V::String8(vec![0xC4, 0xE0]));
    assert_eq!(stv_msg::message_from_msg_bytes(&build_msg(&m), None).unwrap().header.subject, "Да");
}

#[test]
fn source_resolves_embedded_locators() {
    let dir = std::env::temp_dir().join(format!("stv-msg-synth-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.msg");
    std::fs::write(&path, build_msg(&sample_msg())).unwrap();

    let op = stv_msg::opener();
    assert_eq!(op.sniff(&path), Some(mail_model::SourceKind::Msg));
    let src = op.open(&path).unwrap();
    let root = Locator::File { path: String::new() };
    let headers = src.list(&src.folders().unwrap()[0].id, 0..10, Default::default()).unwrap();
    assert_eq!(headers.len(), 1);
    assert_eq!(headers[0].subject, "Héllo wörld ✓");
    assert_eq!(headers[0].locator.as_ref(), Some(&root));

    let emb = Locator::Embedded { parent: Box::new(root.clone()), attachment: 1 };
    let m = src.message(&emb).unwrap();
    assert_eq!(m.header.subject, "Привет мир");
    assert_eq!(m.header.locator.as_ref(), Some(&emb));
    assert_eq!(src.attachment(&emb, 0).unwrap(), [0, 1, 2, 255]);
    // A plain file attachment is not a message.
    let not_msg = Locator::Embedded { parent: Box::new(root.clone()), attachment: 0 };
    assert!(matches!(src.message(&not_msg), Err(Error::Unsupported(_))));
    assert!(matches!(src.message(&Locator::Pst { nid: 1 }), Err(Error::NotFound(_))));
    assert!(src.raw_mime(&root).unwrap().is_none());
    drop(src);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn corrupt_msg_is_format_error() {
    let bytes = build_msg(&sample_msg());
    fn is_format<T>(r: mail_model::Result<T>) -> bool {
        matches!(r, Err(Error::Format(_)))
    }
    assert!(is_format(stv_msg::message_from_msg_bytes(&bytes[..bytes.len() / 2], None)));
    assert!(is_format(stv_msg::message_from_msg_bytes(&[], None)));
    assert!(is_format(stv_msg::message_from_msg_bytes(b"not a compound file at all", None)));
    let mut junk = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    junk.resize(4096, 0xAB);
    assert!(is_format(stv_msg::message_from_msg_bytes(&junk, None)));

    // A valid compound file that is not an Outlook message.
    let mut cfb = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
    std::io::Write::write_all(&mut cfb.create_stream("/WordDocument").unwrap(), b"doc").unwrap();
    cfb.flush().unwrap();
    let doc = cfb.into_inner().into_inner();
    assert!(is_format(stv_msg::message_from_msg_bytes(&doc, None)));
    assert!(is_format(stv_msg::attachment_from_msg_bytes(&doc, 0)));
}

#[test]
fn deeply_nested_storages_are_rejected_not_recursed() {
    let inner = MsgB::default().prop(0x0037, V::Unicode("inner".into()));
    let bytes = build_msg(
        &MsgB::default().prop(0x0037, V::Unicode("outer".into())).with_attachment(AttB::Embedded(vec![], inner)),
    );
    assert!(stv_msg::attachment_from_msg_bytes(&bytes, 0).is_ok());

    // Hang a 200-deep storage chain under the embedded message.
    let mut cfb = cfb::CompoundFile::open(std::io::Cursor::new(bytes)).unwrap();
    let mut path = String::from("/__attach_version1.0_#00000000/__substg1.0_3701000D");
    for _ in 0..200 {
        path.push_str("/d");
        cfb.create_storage(&path).unwrap();
    }
    cfb.flush().unwrap();
    let deep = cfb.into_inner().into_inner();
    assert_eq!(stv_msg::message_from_msg_bytes(&deep, None).unwrap().header.subject, "outer");
    assert!(matches!(stv_msg::attachment_from_msg_bytes(&deep, 0), Err(Error::Format(_))));
}
