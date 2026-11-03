//! TNEF decoding against streams built by the test-only writer in `common/`.

mod common;

use common::*;
use mail_model::{AttachmentKind, Error, Locator, SourceKind};
use mapi_props::{NamedId, PropKey, PropValue};

/// 2010-04-28 12:40:56 UTC.
const SENT_MS: i64 = 1_272_458_456_000;

fn inner_tnef() -> Vec<u8> {
    TnefB::new(1252)
        .attr(LVL_MSG, ATT_MESSAGE_CLASS, sz("IPM.Note"))
        .attr(LVL_MSG, ATT_SUBJECT, sz("Inner TNEF"))
        .attr(LVL_MSG, ATT_MSG_PROPS, prop_list(&[(K::Id(0x0E07), V::I32(1))], &[]))
        .attr(LVL_ATT, ATT_ATTACH_REND_DATA, rend_data_file())
        .attr(LVL_ATT, ATT_ATTACH_TITLE, sz("deep.bin"))
        .attr(LVL_ATT, ATT_ATTACH_DATA, vec![7, 7, 7])
        .build()
}

fn sample_tnef() -> Vec<u8> {
    let mut subject = b"Caf\xe9 meeting".to_vec();
    subject.push(0);
    TnefB::new(1252)
        .attr(LVL_MSG, ATT_MESSAGE_CLASS, sz("IPM.Microsoft Mail.Note"))
        .attr(LVL_MSG, ATT_FROM, trp("Zoe Sender", "SMTP:zoe@example.com"))
        .attr(LVL_MSG, ATT_SUBJECT, subject)
        .attr(LVL_MSG, ATT_DATE_SENT, dtr(2010, 4, 28, 12, 40, 56))
        .attr(LVL_MSG, ATT_BODY, sz("Legacy body"))
        .attr(LVL_MSG, ATT_PRIORITY, vec![1, 0])
        .attr(LVL_MSG, ATT_MESSAGE_STATUS, vec![0x20])
        .attr(
            LVL_MSG,
            ATT_MSG_PROPS,
            prop_list(
                &[
                    (K::Id(0x1035), V::Unicode("<tnef@example.com>".into())),
                    (
                        K::Name(PS_PUBLIC_STRINGS, "Keywords".into()),
                        V::MultiUnicode(vec!["Alpha".into(), "Beta".into()]),
                    ),
                    (K::Lid(PSETID_COMMON, 0x8580), V::I32(7)),
                    (K::Id(0x1000), V::Unicode("MAPI body".into())),
                    (K::Id(0x1013), V::Binary(b"<b>hi</b>".to_vec())),
                    (K::Id(0x3FDE), V::I32(65001)),
                    (K::Id(0x0E1B), V::Bool(true)),
                    (K::Id(0x6001), V::I16(-3)),
                    (K::Id(0x6002), V::MultiString8(vec![b"x\xe9".to_vec(), b"yy".to_vec()])),
                    (K::Id(0x6003), V::F64(2.5)),
                ],
                &[],
            ),
        )
        .attr(
            LVL_MSG,
            ATT_RECIP_TABLE,
            recip_table(&[
                vec![
                    (K::Id(0x3001), V::Unicode("Rcpt One".into())),
                    (K::Id(0x39FE), V::Unicode("one@example.com".into())),
                    (K::Id(0x0C15), V::I32(1)),
                ],
                vec![
                    (K::Id(0x3001), V::String8(b"Rcpt Tw\xf6".to_vec())),
                    (K::Id(0x39FE), V::String8(b"two@example.com".to_vec())),
                    (K::Id(0x0C15), V::I32(2)),
                ],
            ]),
        )
        .attr(LVL_ATT, ATT_ATTACH_REND_DATA, rend_data_file())
        .attr(LVL_ATT, ATT_ATTACH_TITLE, sz("FILEON~1.TXT"))
        .attr(LVL_ATT, ATT_ATTACH_MODIFY_DATE, dtr(2010, 4, 28, 12, 40, 56))
        .attr(LVL_ATT, ATT_ATTACH_DATA, b"file payload".to_vec())
        .attr(
            LVL_ATT,
            ATT_ATTACHMENT,
            prop_list(
                &[
                    (K::Id(0x3707), V::Unicode("file one.txt".into())),
                    (K::Id(0x370E), V::Unicode("text/plain".into())),
                    (K::Id(0x3705), V::I32(1)),
                ],
                &[],
            ),
        )
        .attr(LVL_ATT, ATT_ATTACH_REND_DATA, rend_data_file())
        .attr(LVL_ATT, ATT_ATTACH_TITLE, sz("Inner TNEF"))
        .attr(
            LVL_ATT,
            ATT_ATTACHMENT,
            prop_list(
                &[(K::Id(0x3705), V::I32(5)), (K::Id(0x3001), V::Unicode("Inner TNEF".into()))],
                &[(0x3701, Obj(IID_IMESSAGE, inner_tnef()))],
            ),
        )
        .build()
}

#[test]
fn legacy_attributes_and_encapsulated_props() {
    let bytes = sample_tnef();
    let m = stv_msg::message_from_tnef_bytes(&bytes, None).unwrap();
    let h = &m.header;
    assert_eq!(h.subject, "Café meeting", "attSubject decoded with attOemCodepage 1252");
    assert_eq!(h.message_class.as_deref(), Some("IPM.Note"), "legacy class name mapped");
    let from = h.from.as_ref().unwrap();
    assert_eq!((from.name.as_deref(), from.email.as_deref()), (Some("Zoe Sender"), Some("zoe@example.com")));
    assert_eq!(h.date, Some(SENT_MS));
    assert_eq!(h.to.len(), 1);
    assert_eq!(h.to[0].email.as_deref(), Some("one@example.com"));
    assert_eq!(h.cc[0].name.as_deref(), Some("Rcpt Twö"));
    assert_eq!(h.labels, ["Alpha", "Beta"]);
    assert_eq!(h.message_id.as_deref(), Some("tnef@example.com"));
    assert!(h.flags.read);
    assert_eq!(m.body.html.as_deref(), Some("<b>hi</b>"));

    assert_eq!(m.attachments.len(), 2);
    let a = &m.attachments[0];
    assert_eq!(a.filename.as_deref(), Some("file one.txt"), "attAttachment long filename wins over attAttachTitle");
    assert_eq!(a.mime_type.as_deref(), Some("text/plain"));
    assert_eq!(a.size, 12);
    assert_eq!(a.kind, AttachmentKind::File);
    assert_eq!(m.attachments[1].kind, AttachmentKind::EmbeddedMessage);

    let raw = stv_msg::decode_mapi(&bytes, &[]).unwrap();
    let get = |k: PropKey| raw.props.get(&k).map(|p| p.value.clone());
    assert_eq!(get(PropKey::Id(0x1000)), Some(PropValue::String("MAPI body".into())), "MAPI props override legacy");
    assert_eq!(get(PropKey::Id(0x0017)), Some(PropValue::Int32(2)), "attPriority 1 = high");
    assert_eq!(get(PropKey::Id(0x6001)), Some(PropValue::Int16(-3)));
    assert_eq!(get(PropKey::Id(0x6002)), Some(PropValue::MultiString(vec!["xé".into(), "yy".into()])));
    assert_eq!(get(PropKey::Id(0x6003)), Some(PropValue::Float64(2.5)));
    assert_eq!(get(PropKey::Named { guid: PSETID_COMMON, id: NamedId::Lid(0x8580) }), Some(PropValue::Int32(7)));
    assert_eq!(raw.attachments[0].props[&PropKey::Id(0x3008)].value, PropValue::SysTime(SENT_MS));
}

#[test]
fn attachments_and_embedded_tnef() {
    let bytes = sample_tnef();
    assert_eq!(stv_msg::attachment_from_tnef_bytes(&bytes, 0).unwrap(), b"file payload");
    let inner = stv_msg::attachment_from_tnef_bytes(&bytes, 1).unwrap();
    assert_eq!(inner, inner_tnef(), "embedded payload is the nested TNEF stream without the IID");
    let m = stv_msg::message_from_tnef_bytes(&inner, None).unwrap();
    assert_eq!(m.header.subject, "Inner TNEF");
    assert!(matches!(stv_msg::attachment_from_tnef_bytes(&bytes, 2), Err(Error::NotFound(_))));

    let dir = std::env::temp_dir().join(format!("stv-msg-tnef-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("winmail.dat");
    std::fs::write(&path, &bytes).unwrap();
    let op = stv_msg::opener();
    assert_eq!(op.sniff(&path), Some(SourceKind::Tnef));
    let src = op.open(&path).unwrap();
    assert_eq!(src.kind(), SourceKind::Tnef);
    let root = Locator::File { path: String::new() };
    assert_eq!(src.message(&root).unwrap().header.subject, "Café meeting");
    let emb = Locator::Embedded { parent: Box::new(root.clone()), attachment: 1 };
    let em = src.message(&emb).unwrap();
    assert_eq!(em.header.subject, "Inner TNEF");
    assert_eq!(em.attachments[0].filename.as_deref(), Some("deep.bin"));
    assert_eq!(src.attachment(&emb, 0).unwrap(), [7, 7, 7]);
    let deeper = Locator::Embedded { parent: Box::new(emb), attachment: 0 };
    assert!(matches!(src.message(&deeper), Err(Error::Unsupported(_))));
    drop(src);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn bad_checksum_is_tolerated() {
    let mut bytes = sample_tnef();
    // Corrupt every attribute checksum: walk the attribute stream.
    let mut pos = 6;
    while pos + 9 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[pos + 5..pos + 9].try_into().unwrap()) as usize;
        let ck = pos + 9 + len;
        bytes[ck] ^= 0x5A;
        pos = ck + 2;
    }
    let m = stv_msg::message_from_tnef_bytes(&bytes, None).unwrap();
    assert_eq!(m.header.subject, "Café meeting");
    assert_eq!(m.attachments.len(), 2);
}

#[test]
fn damaged_tnef_keeps_what_decodes() {
    let bytes = sample_tnef();
    let is_format = |r: mail_model::Result<mail_model::Message>| matches!(r, Err(Error::Format(_)));
    // Only a bad signature, or nothing decodable at all, fails the stream.
    assert!(is_format(stv_msg::message_from_tnef_bytes(&bytes[..3], None)));
    assert!(is_format(stv_msg::message_from_tnef_bytes(&[], None)));
    let mut bad_sig = bytes.clone();
    bad_sig[0] = 0;
    assert!(is_format(stv_msg::message_from_tnef_bytes(&bad_sig, None)));
    assert!(is_format(stv_msg::message_from_tnef_bytes(&[0x78, 0x9F, 0x3E, 0x22, 1, 0], None)), "no attributes");
    let mut lone_bad_level = TnefB::new(1252).build();
    lone_bad_level.truncate(6);
    lone_bad_level.extend_from_slice(&[0x5A, 0, 0x80, 1, 0, 1, 0, 0, 0, b'x', b'x', 0]);
    assert!(is_format(stv_msg::message_from_tnef_bytes(&lone_bad_level, None)), "nothing decodable");

    // Truncated mid-attribute: everything before the cut survives.
    let cut = stv_msg::message_from_tnef_bytes(&bytes[..bytes.len() - 5], None).unwrap();
    assert_eq!(cut.header.subject, "Café meeting");
    assert_eq!(cut.attachments[0].filename.as_deref(), Some("file one.txt"));

    // An attribute of unknown level is skipped.
    let mut bad_level = bytes.clone();
    bad_level[6] = 0x5A;
    let m = stv_msg::message_from_tnef_bytes(&bad_level, None).unwrap();
    assert_eq!(m.header.subject, "Café meeting");
    assert_eq!(m.attachments.len(), 2);

    // Unknown property type inside attMsgProps: earlier properties and other attributes survive.
    let bad_type = TnefB::new(1252)
        .attr(LVL_MSG, ATT_SUBJECT, sz("Still here"))
        .attr(
            LVL_MSG,
            ATT_MSG_PROPS,
            [&[2u8, 0, 0, 0, 0x03, 0x00, 0x17, 0x00, 2, 0, 0, 0][..], &[0xFF, 0x0F, 0x01, 0x60, 0, 0, 0, 0]].concat(),
        )
        .build();
    let raw = stv_msg::decode_mapi(&bad_type, &[]).unwrap();
    assert_eq!(raw.props[&PropKey::Id(0x0017)].value, PropValue::Int32(2), "property before the bad one");
    assert_eq!(stv_msg::message_from_tnef_bytes(&bad_type, None).unwrap().header.subject, "Still here");
}
