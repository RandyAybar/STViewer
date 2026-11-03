//! RFC 5322 / MIME synthesis from a decoded [`Message`] (for MAPI sources, which have
//! no native MIME), built with `mail-builder`.

use std::borrow::Cow;

use base64::Engine as _;
use mail_builder::headers::address::Address as MbAddress;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::text::Text;
use mail_builder::mime::{BodyPart, MimePart};
use mail_builder::MessageBuilder;
use mail_model::{Address, AttachmentKind, AttachmentMeta, Error, Locator, MailSource, Message, Result};

use crate::embedded;

/// Deepest chain of embedded messages we follow when synthesising.
const MAX_DEPTH: u32 = 16;

pub(crate) enum Payload {
    /// Attachment file content.
    Bytes(Vec<u8>),
    /// An embedded message, already serialised as RFC 5322.
    Message(Vec<u8>),
}

/// RFC 5322 bytes for `loc`: native MIME if the source has it, else synthesised.
pub(crate) fn message_mime(src: &dyn MailSource, loc: &Locator, msg: &Message, depth: u32) -> Result<Vec<u8>> {
    if let Some(raw) = embedded::raw_mime(src, loc)? {
        return Ok(raw);
    }
    synthesize(msg, &mut |meta| {
        if meta.kind == AttachmentKind::EmbeddedMessage {
            if depth >= MAX_DEPTH {
                return Err(Error::Format("embedded messages nested too deeply".into()));
            }
            let eloc = Locator::Embedded { parent: Box::new(loc.clone()), attachment: meta.index };
            let nested = embedded::message(src, &eloc).and_then(|m| message_mime(src, &eloc, &m, depth + 1));
            match nested {
                Ok(bytes) => Ok(Payload::Message(bytes)),
                // An undecodable embedded message is still worth keeping as its raw bytes.
                Err(e) => embedded::attachment(src, loc, meta.index).map(Payload::Bytes).map_err(|_| e),
            }
        } else {
            embedded::attachment(src, loc, meta.index).map(Payload::Bytes)
        }
    })
}

fn norm_id(id: &str) -> &str {
    id.trim().trim_start_matches('<').trim_end_matches('>').trim()
}

fn mb_address(a: &Address) -> MbAddress<'_> {
    let name = a.name.as_deref().filter(|n| !n.trim().is_empty());
    let email = match a.email.as_deref().filter(|e| !e.trim().is_empty()) {
        Some(e) => e,
        // Exchange-internal recipients may only have a display name; keep it visible.
        None => name.filter(|n| n.contains('@')).unwrap_or(""),
    };
    MbAddress::new_address(name.filter(|n| *n != email), email)
}

/// Content-ID values the HTML body references as `cid:...`.
fn referenced_cid<'a>(html: Option<&str>, meta: &'a AttachmentMeta) -> Option<&'a str> {
    let cid = norm_id(meta.content_id.as_deref()?);
    if cid.is_empty() {
        return None;
    }
    let html = html?;
    html.contains(&format!("cid:{cid}")).then_some(cid)
}

/// Base64 with CRLF every 76 columns (RFC 2045 6.8).
fn base64_lines(bytes: &[u8]) -> Vec<u8> {
    let enc = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut out = Vec::with_capacity(enc.len() + enc.len() / 76 * 2 + 2);
    for chunk in enc.as_bytes().chunks(76) {
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn is_7bit(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b < 0x80 && b != 0)
}

fn attachment_name(meta: &AttachmentMeta, embedded: bool) -> String {
    let name = meta.filename.as_deref().map(str::trim).filter(|n| !n.is_empty());
    if embedded {
        let stem = name.map(|n| n.strip_suffix(".msg").or_else(|| n.strip_suffix(".MSG")).unwrap_or(n));
        return match stem {
            Some(s) if s.to_ascii_lowercase().ends_with(".eml") => s.to_owned(),
            Some(s) => format!("{s}.eml"),
            None => format!("message-{}.eml", meta.index),
        };
    }
    match name {
        Some(n) => n.to_owned(),
        None => format!("attachment-{}{}", meta.index, extension_for(meta.mime_type.as_deref().unwrap_or(""))),
    }
}

/// Serialise `msg` as MIME: `multipart/mixed` { `multipart/related` { `multipart/alternative`
/// { text, html }, inline images }, attachments, `message/rfc822` parts }, collapsing levels
/// that would have a single child. Attachments are base64 so their bytes round-trip exactly.
/// `Link`/`Ole` attachments whose content cannot be fetched are omitted.
pub(crate) fn synthesize(msg: &Message, fetch: &mut dyn FnMut(&AttachmentMeta) -> Result<Payload>) -> Result<Vec<u8>> {
    let h = &msg.header;
    let mut b = MessageBuilder::new();
    if let Some(f) = &h.from {
        b = b.from(mb_address(f));
    }
    for (name, list) in [("To", &h.to), ("Cc", &h.cc), ("Bcc", &h.bcc)] {
        if !list.is_empty() {
            b = b.header(name, MbAddress::new_list(list.iter().map(mb_address).collect()));
        }
    }
    b = b.subject(Text::new(h.subject.as_str()));
    if let Some(d) = h.date {
        b = b.date(Date::new(d.div_euclid(1000)));
    }
    if let Some(id) = h.message_id.as_deref().map(norm_id).filter(|s| !s.is_empty()) {
        b = b.message_id(MessageId::new(id));
    }
    if let Some(id) = h.in_reply_to.as_deref().map(norm_id).filter(|s| !s.is_empty()) {
        b = b.in_reply_to(MessageId::new(id));
    }
    let refs: Vec<&str> = h.references.iter().map(|r| norm_id(r)).filter(|r| !r.is_empty()).collect();
    if !refs.is_empty() {
        b = b.references(MessageId::new_list(refs.into_iter()));
    }
    if !h.labels.is_empty() {
        b = b.header("Keywords", Text::new(h.labels.join(", ")));
    }
    if let Some(class) = h.message_class.as_deref().filter(|c| !c.is_empty()) {
        b = b.header("X-MS-Message-Class", Text::new(class));
    }

    let html = msg.body.html.as_deref().filter(|s| !s.is_empty());
    let text: Option<Cow<'_, str>> = match msg.body.text.as_deref().filter(|s| !s.is_empty()) {
        Some(t) => Some(Cow::Borrowed(t)),
        None if html.is_none() => msg.body.rtf.as_deref().map(|r| Cow::Owned(mapi_props::rtf::to_text(r))),
        None => None,
    };
    let mut core = match (text, html) {
        (Some(t), Some(hm)) => {
            MimePart::new("multipart/alternative", vec![MimePart::new("text/plain", t), MimePart::new("text/html", hm)])
        }
        (Some(t), None) => MimePart::new("text/plain", t),
        (None, Some(hm)) => MimePart::new("text/html", hm),
        (None, None) => MimePart::new("text/plain", ""),
    };

    let mut related = Vec::new();
    let mut mixed = Vec::new();
    for meta in &msg.attachments {
        let payload = match fetch(meta) {
            Ok(p) => p,
            Err(_) if matches!(meta.kind, AttachmentKind::Link | AttachmentKind::Ole) => continue,
            Err(e) => return Err(e),
        };
        match payload {
            Payload::Message(bytes) => {
                let cte = if is_7bit(&bytes) { "7bit" } else { "8bit" };
                mixed.push(
                    MimePart::new("message/rfc822", BodyPart::Binary(bytes.into()))
                        .attachment(attachment_name(meta, true))
                        .transfer_encoding(cte),
                );
            }
            Payload::Bytes(bytes) => {
                let name = attachment_name(meta, false);
                let ct =
                    meta.mime_type.clone().filter(|m| m.contains('/')).unwrap_or_else(|| guess_mime(&name).to_owned());
                let part = MimePart::new(ContentType::new(ct), BodyPart::Binary(base64_lines(&bytes).into()));
                match referenced_cid(html, meta) {
                    Some(cid) => related.push(
                        part.header("Content-Disposition", ContentType::new("inline").attribute("filename", name))
                            .cid(cid.to_owned())
                            .transfer_encoding("base64"),
                    ),
                    None => {
                        let mut part = part.attachment(name);
                        if let Some(cid) = meta.content_id.as_deref().map(norm_id).filter(|c| !c.is_empty()) {
                            part = part.cid(cid.to_owned());
                        }
                        mixed.push(part.transfer_encoding("base64"));
                    }
                }
            }
        }
    }
    if !related.is_empty() {
        related.insert(0, core);
        core = MimePart::new("multipart/related", related);
    }
    if !mixed.is_empty() {
        mixed.insert(0, core);
        core = MimePart::new("multipart/mixed", mixed);
    }
    b.body(core).write_to_vec().map_err(Error::Io)
}

/// MIME type from a file name's extension; `application/octet-stream` when unknown.
pub(crate) fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "txt" | "log" => "text/plain",
        "htm" | "html" => "text/html",
        "csv" => "text/csv",
        "xml" => "text/xml",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "rtf" => "application/rtf",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "7z" => "application/x-7z-compressed",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        "dat" | "tnef" => "application/ms-tnef",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/x-icon",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// Extension (with dot) for a MIME type, for unnamed attachments.
pub(crate) fn extension_for(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().split(';').next().unwrap_or("").trim() {
        "text/plain" => ".txt",
        "text/html" => ".html",
        "text/calendar" => ".ics",
        "text/vcard" | "text/x-vcard" => ".vcf",
        "application/pdf" => ".pdf",
        "application/rtf" | "text/rtf" => ".rtf",
        "application/zip" => ".zip",
        "message/rfc822" => ".eml",
        "application/vnd.ms-outlook" => ".msg",
        "application/ms-tnef" => ".dat",
        "image/png" => ".png",
        "image/jpeg" | "image/jpg" => ".jpg",
        "image/gif" => ".gif",
        "image/bmp" => ".bmp",
        "image/webp" => ".webp",
        "image/svg+xml" => ".svg",
        "image/tiff" => ".tif",
        _ => ".bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_model::{Body, Header};
    use mail_parser::MimeHeaders;

    fn addr(name: &str, email: &str) -> Address {
        Address { name: Some(name.into()), email: Some(email.into()) }
    }

    #[test]
    fn synthesised_mime_round_trips_through_a_parser() {
        let png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0, 1, 2, 255, 13, 10, 13];
        let pdf: Vec<u8> = (0u8..=255).collect();
        let msg = Message {
            header: Header {
                subject: "Quarterly résumé ☕".into(),
                from: Some(addr("Ana Núñez", "ana@example.com")),
                to: vec![addr("Bob", "bob@example.com"), Address { name: Some("Carol".into()), email: None }],
                cc: vec![addr("Dan", "dan@example.com")],
                date: Some(1_700_000_000_000),
                message_id: Some("<abc@example.com>".into()),
                in_reply_to: Some("parent@example.com".into()),
                references: vec!["root@example.com".into(), "parent@example.com".into()],
                ..Default::default()
            },
            body: Body {
                text: Some("Hello Bob,\r\nsee attached.\r\n".into()),
                html: Some("<p>Hello <b>Bob</b></p><img src=\"cid:img1@x\">".into()),
                rtf: None,
            },
            attachments: vec![
                AttachmentMeta {
                    index: 0,
                    filename: Some("logo.png".into()),
                    mime_type: Some("image/png".into()),
                    content_id: Some("<img1@x>".into()),
                    inline: true,
                    ..Default::default()
                },
                AttachmentMeta { index: 1, filename: Some("report.pdf".into()), ..Default::default() },
                AttachmentMeta {
                    index: 2,
                    filename: Some("Fwd.msg".into()),
                    kind: AttachmentKind::EmbeddedMessage,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let inner = b"From: x@example.com\r\nSubject: inner\r\n\r\ninner body\r\n".to_vec();
        let bytes = synthesize(&msg, &mut |m| {
            Ok(match m.index {
                0 => Payload::Bytes(png.clone()),
                1 => Payload::Bytes(pdf.clone()),
                _ => Payload::Message(inner.clone()),
            })
        })
        .unwrap();

        let parsed = mail_parser::MessageParser::default().parse(&bytes).expect("parse");
        assert_eq!(parsed.subject(), Some("Quarterly résumé ☕"));
        assert_eq!(parsed.message_id(), Some("abc@example.com"));
        let from = parsed.from().and_then(|f| f.first()).unwrap();
        assert_eq!(from.name(), Some("Ana Núñez"));
        assert_eq!(from.address(), Some("ana@example.com"));
        assert_eq!(parsed.to().unwrap().iter().count(), 2);
        assert_eq!(parsed.date().unwrap().to_timestamp(), 1_700_000_000);
        assert!(parsed.body_text(0).unwrap().contains("see attached."));
        assert!(parsed.body_html(0).unwrap().contains("<b>Bob</b>"));

        let atts: Vec<_> = parsed.attachments().collect();
        assert_eq!(atts.len(), 3, "inline image, pdf, embedded message");
        let logo = atts.iter().find(|a| a.attachment_name() == Some("logo.png")).unwrap();
        assert_eq!(logo.contents(), &png[..]);
        let report = atts.iter().find(|a| a.attachment_name() == Some("report.pdf")).unwrap();
        assert_eq!(report.contents(), &pdf[..]);
        let nested = atts.iter().find_map(|a| a.message()).expect("message/rfc822 part");
        assert_eq!(nested.subject(), Some("inner"));
        let raw = String::from_utf8_lossy(&bytes);
        assert!(raw.contains("Fwd.eml"), "embedded .msg renamed to .eml");
        assert!(raw.contains("multipart/related"));
    }

    #[test]
    fn rtf_only_body_becomes_text_and_failed_links_are_skipped() {
        let msg = Message {
            header: Header { subject: "s".into(), ..Default::default() },
            body: Body { text: None, html: None, rtf: Some("{\\rtf1\\ansi Plain words\\par}".into()) },
            attachments: vec![AttachmentMeta { index: 0, kind: AttachmentKind::Link, ..Default::default() }],
            ..Default::default()
        };
        let bytes = synthesize(&msg, &mut |_| Err(Error::NotFound("link".into()))).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&bytes).unwrap();
        assert!(parsed.body_text(0).unwrap().contains("Plain words"));
        assert_eq!(parsed.attachments().count(), 0);
    }
}
