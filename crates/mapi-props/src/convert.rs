//! [`MapiMessage`] → `mail_model::{Header, Message}`. Shared by PST/OST, MSG and TNEF so
//! every MAPI container renders identically.

use mail_model::{
    Address, AttachmentKind, AttachmentMeta, Body, FolderId, Header, Locator, Message, MessageFlags, PropertyView,
};

use crate::{codepage, get, names, rtf, tags as t, MapiAttachment, MapiMessage, NamedId, PropKey, PropMap, PropValue};

/// Max characters of a single value rendered into [`PropertyView::value`].
const PROPERTY_VIEW_MAX_CHARS: usize = 4096;

fn str_prop(map: &PropMap, id: u16) -> Option<&str> {
    get(map, id).and_then(PropValue::as_str).map(|s| s.trim_end_matches('\0')).filter(|s| !s.trim().is_empty())
}

fn int_prop(map: &PropMap, id: u16) -> Option<i64> {
    get(map, id).and_then(PropValue::as_i64)
}

/// Remove the PST/MSG subject marker: when `PidTagSubject` starts with U+0001 the next char is
/// the (prefix length + 1) and both are not part of the subject.
pub fn strip_subject_marker(s: &str) -> &str {
    let mut it = s.char_indices();
    match (it.next(), it.next()) {
        (Some((_, '\u{1}')), Some((_, _))) => it.next().map_or("", |(i, _)| &s[i..]),
        (Some((_, '\u{1}')), None) => "",
        _ => s,
    }
}

/// Display subject: `PidTagSubject` (marker stripped), else prefix + normalized subject, else
/// `PidTagConversationTopic`.
pub fn subject(map: &PropMap) -> String {
    if let Some(s) = get(map, t::SUBJECT).and_then(PropValue::as_str) {
        let s = strip_subject_marker(s);
        if !s.is_empty() {
            return s.to_owned();
        }
    }
    if let Some(norm) = str_prop(map, t::NORMALIZED_SUBJECT) {
        let prefix = str_prop(map, t::SUBJECT_PREFIX).unwrap_or("");
        return format!("{prefix}{norm}");
    }
    str_prop(map, t::CONVERSATION_TOPIC).unwrap_or("").to_owned()
}

/// Choose a usable e-mail: explicit SMTP property, else the address when its type is SMTP
/// (or it looks like one). Exchange DNs are not returned here.
fn smtp_email(smtp: Option<&str>, addr_type: Option<&str>, email: Option<&str>) -> Option<String> {
    if let Some(s) = smtp {
        return Some(s.trim().to_owned());
    }
    let email = email?.trim();
    let is_smtp = addr_type.is_some_and(|t| t.eq_ignore_ascii_case("SMTP"));
    if is_smtp || (email.contains('@') && !email.starts_with('/')) {
        Some(email.trim_start_matches("SMTP:").trim_start_matches("smtp:").to_owned())
    } else {
        None
    }
}

fn address(name: Option<&str>, email: Option<String>) -> Option<Address> {
    let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
    let email = email.filter(|e| !e.is_empty());
    (name.is_some() || email.is_some()).then_some(Address { name, email })
}

/// Sender: `PidTagSender*` (SMTP preferred over EX DN), falling back to `PidTagSentRepresenting*`.
pub fn sender(map: &PropMap) -> Option<Address> {
    let name = str_prop(map, t::SENDER_NAME).or_else(|| str_prop(map, t::SENT_REPRESENTING_NAME));
    let email = smtp_email(
        str_prop(map, t::SENDER_SMTP_ADDRESS),
        str_prop(map, t::SENDER_ADDRESS_TYPE),
        str_prop(map, t::SENDER_EMAIL_ADDRESS),
    )
    .or_else(|| {
        smtp_email(
            str_prop(map, t::SENT_REPRESENTING_SMTP_ADDRESS),
            str_prop(map, t::SENT_REPRESENTING_ADDRESS_TYPE),
            str_prop(map, t::SENT_REPRESENTING_EMAIL_ADDRESS),
        )
    })
    .or_else(|| {
        str_prop(map, t::SENDER_EMAIL_ADDRESS)
            .or_else(|| str_prop(map, t::SENT_REPRESENTING_EMAIL_ADDRESS))
            .map(str::to_owned)
    });
    address(name, email)
}

/// One recipient row → address (SMTP preferred; EX DN as last resort).
pub fn recipient_address(r: &PropMap) -> Option<Address> {
    let name = str_prop(r, t::DISPLAY_NAME)
        .or_else(|| str_prop(r, t::RECIPIENT_DISPLAY_NAME))
        .or_else(|| str_prop(r, t::TRANSMITTABLE_DISPLAY_NAME));
    let email = smtp_email(str_prop(r, t::SMTP_ADDRESS), str_prop(r, t::ADDRESS_TYPE), str_prop(r, t::EMAIL_ADDRESS))
        .or_else(|| str_prop(r, t::EMAIL_ADDRESS).map(str::to_owned));
    address(name, email)
}

fn display_list(map: &PropMap, id: u16) -> Vec<Address> {
    str_prop(map, id)
        .map(|s| {
            s.split(';')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|n| Address { name: Some(n.to_owned()), email: None })
                .collect()
        })
        .unwrap_or_default()
}

fn clean_id(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('<').trim_end_matches('>').trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn split_ids(s: &str) -> Vec<String> {
    s.split(|c: char| c.is_whitespace() || c == ',').filter_map(clean_id).collect()
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02x}");
        s
    })
}

/// Outlook categories (`PidNameKeywords`).
pub fn labels(map: &PropMap) -> Vec<String> {
    match map.get(&names::keywords_key()).map(|p| &p.value) {
        Some(PropValue::MultiString(v)) => v.iter().filter(|s| !s.trim().is_empty()).cloned().collect(),
        Some(PropValue::String(s)) if !s.trim().is_empty() => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Header from message properties. When `m.recipients` is empty (list views), `to`/`cc`/`bcc`
/// are filled with display names from `PidTagDisplayTo`/`Cc`/`Bcc`.
pub fn header_from_mapi(m: &MapiMessage, locator: Option<Locator>, folder: Option<FolderId>) -> Header {
    let p = &m.props;
    let (mut to, mut cc, mut bcc) = (Vec::new(), Vec::new(), Vec::new());
    if m.recipients.is_empty() {
        to = display_list(p, t::DISPLAY_TO);
        cc = display_list(p, t::DISPLAY_CC);
        bcc = display_list(p, t::DISPLAY_BCC);
    } else {
        for r in &m.recipients {
            let Some(a) = recipient_address(r) else {
                continue;
            };
            match int_prop(r, t::RECIPIENT_TYPE).unwrap_or(1) & 0x0F {
                2 => cc.push(a),
                3 => bcc.push(a),
                _ => to.push(a),
            }
        }
    }

    let msg_flags = int_prop(p, t::MESSAGE_FLAGS);
    let class = str_prop(p, t::MESSAGE_CLASS).map(str::to_owned);
    let class_lc = class.as_deref().unwrap_or("").to_ascii_lowercase();
    let signed = class_lc.starts_with("ipm.note.smime.multipartsigned") || class_lc.starts_with("ipm.note.secure.sign");
    let encrypted = !signed && (class_lc.starts_with("ipm.note.smime") || class_lc.starts_with("ipm.note.secure"));
    let flags = MessageFlags {
        read: msg_flags.is_none_or(|f| f & t::MSGFLAG_READ as i64 != 0),
        flagged: int_prop(p, t::FLAG_STATUS) == Some(2),
        draft: msg_flags.is_some_and(|f| f & t::MSGFLAG_UNSENT as i64 != 0),
        deleted: false,
        encrypted,
        signed,
    };

    let conversation = get(p, t::CONVERSATION_INDEX)
        .and_then(PropValue::as_bytes)
        .filter(|b| b.len() >= 22)
        .map(|b| hex(&b[..22]))
        .or_else(|| str_prop(p, t::CONVERSATION_TOPIC).map(|s| s.trim().to_owned()));

    Header {
        locator,
        folder,
        subject: subject(p),
        from: sender(p),
        to,
        cc,
        bcc,
        date: get(p, t::MESSAGE_DELIVERY_TIME)
            .and_then(PropValue::as_time)
            .or_else(|| get(p, t::CLIENT_SUBMIT_TIME).and_then(PropValue::as_time))
            .or_else(|| get(p, t::CREATION_TIME).and_then(PropValue::as_time)),
        size: int_prop(p, t::MESSAGE_SIZE).map_or(0, |s| s.max(0) as u64),
        has_attachments: !m.attachments.is_empty()
            || msg_flags.is_some_and(|f| f & t::MSGFLAG_HASATTACH as i64 != 0)
            || get(p, t::HAS_ATTACHMENTS).and_then(PropValue::as_bool) == Some(true),
        flags,
        message_id: str_prop(p, t::INTERNET_MESSAGE_ID).and_then(clean_id),
        in_reply_to: str_prop(p, t::IN_REPLY_TO_ID).and_then(|s| split_ids(s).into_iter().next()),
        references: str_prop(p, t::INTERNET_REFERENCES).map(split_ids).unwrap_or_default(),
        conversation,
        labels: labels(p),
        message_class: class,
        item: crate::items::item_details(m),
    }
}

/// Decode `PidTagHtml` bytes: declared codepage, else UTF-8 if valid, else `<meta charset>`, else 1252.
fn decode_html_bytes(b: &[u8], cp: Option<u32>) -> String {
    if let Some(cp) = cp.filter(|&cp| codepage::encoding_for_codepage(cp).is_some()) {
        return codepage::decode_string8(b, Some(cp));
    }
    if let Ok(s) = std::str::from_utf8(b) {
        return s.trim_end_matches('\0').to_owned();
    }
    let head = String::from_utf8_lossy(&b[..b.len().min(2048)]).to_ascii_lowercase();
    if let Some(i) = head.find("charset=") {
        let label: String = head[i + 8..]
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
            .collect();
        if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
            return enc.decode_without_bom_handling(b).0.trim_end_matches('\0').to_owned();
        }
    }
    codepage::decode_string8(b, None)
}

/// Body precedence: `PidTagHtml` → HTML de-encapsulated from `PidTagRtfCompressed` →
/// `PidTagBody` → raw RTF (with its text rendering in `text`).
pub fn body(map: &PropMap) -> Body {
    let cp = int_prop(map, t::INTERNET_CODEPAGE)
        .or_else(|| int_prop(map, t::MESSAGE_CODEPAGE))
        .and_then(|c| u32::try_from(c).ok());
    let mut html = match get(map, t::HTML) {
        Some(PropValue::Binary(b)) if !b.is_empty() => Some(decode_html_bytes(b, cp)),
        Some(PropValue::String(s)) if !s.trim().is_empty() => Some(s.clone()),
        _ => None,
    };
    let mut text = get(map, t::BODY).and_then(PropValue::as_str).filter(|s| !s.trim().is_empty()).map(str::to_owned);
    let mut raw_rtf = None;
    if html.is_none() {
        if let Some(bytes) = get(map, t::RTF_COMPRESSED).and_then(PropValue::as_bytes) {
            if let Ok(dec) = rtf::decompress(bytes) {
                let s = rtf::latin1(&dec);
                if let Some(h) = rtf::extract_html(&s) {
                    html = Some(h);
                } else if text.is_none() {
                    let t = rtf::to_text(&s);
                    text = (!t.trim().is_empty()).then_some(t);
                    raw_rtf = Some(s);
                }
            }
        }
    }
    Body { text, html, rtf: raw_rtf }
}

/// Metadata for attachment `index` (its position in `MapiMessage::attachments`).
pub fn attachment_meta(index: u32, a: &MapiAttachment) -> AttachmentMeta {
    let p = &a.props;
    let method = int_prop(p, t::ATTACH_METHOD).unwrap_or(t::ATTACH_BY_VALUE as i64) as i32;
    let mut filename = str_prop(p, t::ATTACH_LONG_FILENAME)
        .or_else(|| str_prop(p, t::ATTACH_FILENAME))
        .or_else(|| str_prop(p, t::DISPLAY_NAME))
        .map(|s| s.trim().to_owned());
    if filename.is_none() && method == t::ATTACH_EMBEDDED_MSG {
        filename = a.embedded.as_ref().map(|m| subject(&m.props)).filter(|s| !s.is_empty());
    }
    let mime_type = str_prop(p, t::ATTACH_MIME_TAG).map(|s| s.trim().to_ascii_lowercase());
    let size = match get(p, t::ATTACH_DATA_BINARY) {
        Some(PropValue::Binary(b)) => b.len() as u64,
        _ => int_prop(p, t::ATTACH_SIZE).map_or(0, |s| s.max(0) as u64),
    };
    let flags = int_prop(p, t::ATTACH_FLAGS).unwrap_or(0) as i32;
    let hidden = get(p, t::ATTACHMENT_HIDDEN).and_then(PropValue::as_bool).unwrap_or(false);
    let is_tnef = filename.as_deref().is_some_and(|f| f.eq_ignore_ascii_case("winmail.dat"))
        || mime_type.as_deref() == Some("application/ms-tnef");
    let kind = match method {
        t::ATTACH_EMBEDDED_MSG => AttachmentKind::EmbeddedMessage,
        _ if is_tnef => AttachmentKind::Tnef,
        t::ATTACH_OLE => AttachmentKind::Ole,
        t::ATTACH_BY_REFERENCE | t::ATTACH_BY_REF_RESOLVE | t::ATTACH_BY_REF_ONLY | t::ATTACH_BY_WEB_REFERENCE => {
            AttachmentKind::Link
        }
        _ => AttachmentKind::File,
    };
    AttachmentMeta {
        index,
        filename,
        mime_type,
        size,
        content_id: str_prop(p, t::ATTACH_CONTENT_ID).and_then(clean_id),
        inline: hidden || flags & t::ATT_MHTML_REF != 0,
        kind,
    }
}

/// `0x0037001F` for fixed ids; `PSETID_Common:0x8516` / `PS_PUBLIC_STRINGS:Keywords` for named.
pub fn tag_string(key: &PropKey, ty: crate::PropType) -> String {
    match key {
        PropKey::Id(id) => format!("0x{id:04X}{:04X}", ty as u16),
        PropKey::Named { guid, id } => {
            let set = names::guid_name(guid).map_or_else(|| names::format_guid(guid), str::to_owned);
            match id {
                NamedId::Lid(l) => format!("{set}:0x{l:04X}"),
                NamedId::Name(n) => format!("{set}:{n}"),
            }
        }
    }
}

/// One [`PropertyView`] per property in `map`, in key order (the property inspector; not
/// part of [`message_from_mapi`] because rendering every value is costly).
pub fn property_views(map: &PropMap) -> Vec<PropertyView> {
    map.iter()
        .map(|(k, p)| PropertyView {
            tag: tag_string(k, p.ty),
            name: crate::tags::name(k).unwrap_or("").to_owned(),
            type_name: p.ty.name().to_owned(),
            value: p.value.display(PROPERTY_VIEW_MAX_CHARS),
        })
        .collect()
}

/// Full message: header with recipients, body, attachment metadata, transport headers.
pub fn message_from_mapi(m: &MapiMessage, locator: Option<Locator>, folder: Option<FolderId>) -> Message {
    Message {
        header: header_from_mapi(m, locator, folder),
        body: body(&m.props),
        attachments: m.attachments.iter().enumerate().map(|(i, a)| attachment_meta(i as u32, a)).collect(),
        raw_headers: get(&m.props, t::TRANSPORT_MESSAGE_HEADERS)
            .and_then(PropValue::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Prop, PropType};

    fn put(map: &mut PropMap, id: u16, ty: PropType, value: PropValue) {
        map.insert(PropKey::Id(id), Prop { ty, value });
    }
    fn s(v: &str) -> PropValue {
        PropValue::String(v.to_owned())
    }

    fn sample() -> MapiMessage {
        let mut p = PropMap::new();
        put(&mut p, t::SUBJECT, PropType::Unicode, s("\u{1}\u{5}RE: Budget"));
        put(&mut p, t::SENDER_NAME, PropType::Unicode, s("Alice"));
        put(&mut p, t::SENDER_ADDRESS_TYPE, PropType::Unicode, s("EX"));
        put(&mut p, t::SENDER_EMAIL_ADDRESS, PropType::Unicode, s("/O=ORG/OU=X/CN=RECIPIENTS/CN=ALICE"));
        put(&mut p, t::SENDER_SMTP_ADDRESS, PropType::Unicode, s("alice@example.com"));
        put(&mut p, t::MESSAGE_DELIVERY_TIME, PropType::SysTime, PropValue::SysTime(1_000));
        put(&mut p, t::CLIENT_SUBMIT_TIME, PropType::SysTime, PropValue::SysTime(500));
        put(&mut p, t::MESSAGE_FLAGS, PropType::Int32, PropValue::Int32(t::MSGFLAG_READ | t::MSGFLAG_HASATTACH));
        put(&mut p, t::FLAG_STATUS, PropType::Int32, PropValue::Int32(2));
        put(&mut p, t::MESSAGE_CLASS, PropType::Unicode, s("IPM.Note.SMIME.MultipartSigned"));
        put(&mut p, t::INTERNET_MESSAGE_ID, PropType::Unicode, s(" <id1@x> "));
        put(&mut p, t::IN_REPLY_TO_ID, PropType::Unicode, s("<id0@x>"));
        put(&mut p, t::INTERNET_REFERENCES, PropType::Unicode, s("<a@x>\r\n <b@x>,<id0@x>"));
        let mut ci = vec![1u8; 22];
        ci.extend_from_slice(&[9, 9, 9, 9, 9]);
        put(&mut p, t::CONVERSATION_INDEX, PropType::Binary, PropValue::Binary(ci));
        put(&mut p, t::MESSAGE_SIZE, PropType::Int32, PropValue::Int32(4321));
        put(&mut p, t::DISPLAY_TO, PropType::Unicode, s("Bob; Carol"));
        put(&mut p, t::BODY, PropType::Unicode, s("plain body"));
        put(&mut p, t::TRANSPORT_MESSAGE_HEADERS, PropType::Unicode, s("Received: x\r\n"));
        p.insert(
            names::keywords_key(),
            Prop { ty: PropType::MultiUnicode, value: PropValue::MultiString(vec!["Red".into(), "Work".into()]) },
        );
        MapiMessage { props: p, recipients: Vec::new(), attachments: Vec::new() }
    }

    #[test]
    fn header_fields() {
        let m = sample();
        let h = header_from_mapi(&m, Some(Locator::Pst { nid: 0x2004 }), None);
        assert_eq!(h.subject, "RE: Budget");
        assert_eq!(h.from, Some(Address { name: Some("Alice".into()), email: Some("alice@example.com".into()) }));
        assert_eq!(h.date, Some(1_000));
        assert!(h.flags.read && h.flags.flagged && h.flags.signed && !h.flags.encrypted && !h.flags.draft);
        assert!(h.has_attachments);
        assert_eq!(h.message_id.as_deref(), Some("id1@x"));
        assert_eq!(h.in_reply_to.as_deref(), Some("id0@x"));
        assert_eq!(h.references, vec!["a@x", "b@x", "id0@x"]);
        assert_eq!(h.conversation.as_deref(), Some("01".repeat(22).as_str()));
        assert_eq!(h.labels, vec!["Red", "Work"]);
        assert_eq!(h.size, 4321);
        // No recipient table → display names.
        assert_eq!(h.to.len(), 2);
        assert_eq!(h.to[1].name.as_deref(), Some("Carol"));
        assert_eq!(h.message_class.as_deref(), Some("IPM.Note.SMIME.MultipartSigned"));
    }

    #[test]
    fn subject_variants() {
        assert_eq!(strip_subject_marker("\u{1}\u{5}FW: x"), "FW: x");
        assert_eq!(strip_subject_marker("\u{1}"), "");
        assert_eq!(strip_subject_marker("plain"), "plain");
        let mut p = PropMap::new();
        put(&mut p, t::SUBJECT_PREFIX, PropType::Unicode, s("FW: "));
        put(&mut p, t::NORMALIZED_SUBJECT, PropType::Unicode, s("Hello"));
        assert_eq!(subject(&p), "FW: Hello");
    }

    #[test]
    fn recipients_and_sender_fallbacks() {
        let mut m = sample();
        m.props.remove(&PropKey::Id(t::SENDER_NAME));
        m.props.remove(&PropKey::Id(t::SENDER_SMTP_ADDRESS));
        m.props.remove(&PropKey::Id(t::SENDER_EMAIL_ADDRESS));
        put(&mut m.props, t::SENT_REPRESENTING_NAME, PropType::Unicode, s("Boss"));
        put(&mut m.props, t::SENT_REPRESENTING_ADDRESS_TYPE, PropType::Unicode, s("SMTP"));
        put(&mut m.props, t::SENT_REPRESENTING_EMAIL_ADDRESS, PropType::Unicode, s("boss@example.com"));
        let mk = |ty: i32, name: &str, at: &str, em: &str, smtp: Option<&str>| {
            let mut r = PropMap::new();
            put(&mut r, t::RECIPIENT_TYPE, PropType::Int32, PropValue::Int32(ty));
            put(&mut r, t::DISPLAY_NAME, PropType::Unicode, s(name));
            put(&mut r, t::ADDRESS_TYPE, PropType::Unicode, s(at));
            put(&mut r, t::EMAIL_ADDRESS, PropType::Unicode, s(em));
            if let Some(x) = smtp {
                put(&mut r, t::SMTP_ADDRESS, PropType::Unicode, s(x));
            }
            r
        };
        m.recipients = vec![
            mk(1, "Bob", "SMTP", "bob@example.com", None),
            mk(2, "Carol", "EX", "/O=ORG/CN=CAROL", Some("carol@example.com")),
            mk(3, "Dan", "EX", "/O=ORG/CN=DAN", None),
            mk(0x1000_0001, "Eve", "SMTP", "eve@example.com", None),
        ];
        let h = header_from_mapi(&m, None, None);
        assert_eq!(h.from.unwrap().email.as_deref(), Some("boss@example.com"));
        assert_eq!(
            h.to.iter().map(|a| a.email.clone().unwrap()).collect::<Vec<_>>(),
            ["bob@example.com", "eve@example.com"]
        );
        assert_eq!(h.cc[0].email.as_deref(), Some("carol@example.com"));
        assert_eq!(h.bcc[0].email.as_deref(), Some("/O=ORG/CN=DAN"));
    }

    #[test]
    fn body_precedence() {
        let mut p = PropMap::new();
        put(&mut p, t::BODY, PropType::Unicode, s("text"));
        put(&mut p, t::INTERNET_CODEPAGE, PropType::Int32, PropValue::Int32(1252));
        put(&mut p, t::HTML, PropType::Binary, PropValue::Binary(b"<p>caf\xe9</p>".to_vec()));
        let b = body(&p);
        assert_eq!(b.html.as_deref(), Some("<p>café</p>"));
        assert_eq!(b.text.as_deref(), Some("text"));
        assert!(b.rtf.is_none());

        // RTF-encapsulated HTML wins over plain text when no HTML prop.
        let enc = "{\\rtf1\\ansi\\fromhtml1 {\\*\\htmltag <b>}hi{\\*\\htmltag </b>}}";
        let rtf_bytes = rtf_mela(enc);
        let mut p = PropMap::new();
        put(&mut p, t::BODY, PropType::Unicode, s("hi"));
        put(&mut p, t::RTF_COMPRESSED, PropType::Binary, PropValue::Binary(rtf_bytes));
        let b = body(&p);
        assert_eq!(b.html.as_deref(), Some("<b>hi</b>"));

        // Plain RTF only → rtf kept, text rendered.
        let mut p = PropMap::new();
        put(
            &mut p,
            t::RTF_COMPRESSED,
            PropType::Binary,
            PropValue::Binary(rtf_mela("{\\rtf1\\ansi Hello\\par World}")),
        );
        let b = body(&p);
        assert!(b.html.is_none());
        assert_eq!(b.text.as_deref(), Some("Hello\nWorld"));
        assert_eq!(b.rtf.as_deref(), Some("{\\rtf1\\ansi Hello\\par World}"));
    }

    fn rtf_mela(s: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(s.len() as u32 + 12).to_le_bytes());
        v.extend_from_slice(&(s.len() as u32).to_le_bytes());
        v.extend_from_slice(&0x414C_454Du32.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn attachments() {
        let mut a = PropMap::new();
        put(&mut a, t::ATTACH_METHOD, PropType::Int32, PropValue::Int32(1));
        put(&mut a, t::ATTACH_FILENAME, PropType::Unicode, s("SHORT~1.PNG"));
        put(&mut a, t::ATTACH_LONG_FILENAME, PropType::Unicode, s("long name.png"));
        put(&mut a, t::ATTACH_MIME_TAG, PropType::Unicode, s("Image/PNG"));
        put(&mut a, t::ATTACH_CONTENT_ID, PropType::Unicode, s("<img1@x>"));
        put(&mut a, t::ATTACH_SIZE, PropType::Int32, PropValue::Int32(999));
        put(&mut a, t::ATTACHMENT_HIDDEN, PropType::Boolean, PropValue::Boolean(true));
        let mut tnef = PropMap::new();
        put(&mut tnef, t::ATTACH_FILENAME, PropType::Unicode, s("WINMAIL.DAT"));
        put(&mut tnef, t::ATTACH_DATA_BINARY, PropType::Binary, PropValue::Binary(vec![0; 7]));
        let mut emb = PropMap::new();
        put(&mut emb, t::ATTACH_METHOD, PropType::Int32, PropValue::Int32(5));
        let mut inner = PropMap::new();
        put(&mut inner, t::SUBJECT, PropType::Unicode, s("Inner"));
        let mut m = sample();
        m.attachments = vec![
            MapiAttachment { props: a, embedded: None },
            MapiAttachment { props: tnef, embedded: None },
            MapiAttachment { props: emb, embedded: Some(Box::new(MapiMessage { props: inner, ..Default::default() })) },
        ];
        let msg = message_from_mapi(&m, None, None);
        let a = &msg.attachments;
        assert_eq!((a[0].index, a[0].filename.as_deref()), (0, Some("long name.png")));
        assert_eq!(a[0].mime_type.as_deref(), Some("image/png"));
        assert_eq!(a[0].content_id.as_deref(), Some("img1@x"));
        assert!(a[0].inline);
        assert_eq!(a[0].size, 999);
        assert_eq!((a[1].kind, a[1].size), (AttachmentKind::Tnef, 7));
        assert_eq!((a[2].kind, a[2].filename.as_deref()), (AttachmentKind::EmbeddedMessage, Some("Inner")));
        assert_eq!(msg.raw_headers.as_deref(), Some("Received: x\r\n"));
        let props = property_views(&m.props);
        let subj = props.iter().find(|p| p.tag == "0x0037001F").unwrap();
        assert_eq!((subj.name.as_str(), subj.type_name.as_str()), ("PidTagSubject", "PtypString"));
        let kw = props.iter().find(|p| p.name == "PidNameKeywords").unwrap();
        assert_eq!((kw.tag.as_str(), kw.value.as_str()), ("PS_PUBLIC_STRINGS:Keywords", "Red; Work"));
    }
}
