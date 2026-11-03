//! RFC 5322 / MIME → `mail_model::Message` via `mail-parser`.

use std::collections::HashSet;

use mail_model::{
    AttachmentKind, AttachmentMeta, Body, Error, FolderId, Header, Locator, Message, PropertyView, Result,
};
use mail_parser::decoders::base64::base64_decode;
use mail_parser::decoders::html::{html_to_text, text_to_html};
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{Encoding, HeaderValue, MessagePart, MimeHeaders, PartType};

use crate::hdr::{self, F_ENCRYPTED};
use crate::smime;

type Parsed<'x> = mail_parser::Message<'x>;

const ENCRYPTED_NOTICE: &str = "This message is encrypted (S/MIME or PGP). STViewer cannot decrypt it; \
the encrypted payload is available as an attachment.";

fn parse(bytes: &[u8]) -> Result<Parsed<'_>> {
    hdr::full_parser().parse(bytes).ok_or_else(|| Error::Format("not an RFC 5322 message (no header block)".into()))
}

fn is_opaque_signed(m: &Parsed<'_>) -> bool {
    let Some(ct) = m.content_type() else { return false };
    let st = ct.subtype().unwrap_or("");
    ct.ctype().eq_ignore_ascii_case("application")
        && (st.eq_ignore_ascii_case("pkcs7-mime") || st.eq_ignore_ascii_case("x-pkcs7-mime"))
        && ct.attribute("smime-type").is_some_and(|t| t.eq_ignore_ascii_case("signed-data"))
}

/// Runs `f(outer, content)`. `content` is the signed entity of an opaque S/MIME
/// signed-data message, else the outer message itself.
fn with_content<R>(bytes: &[u8], f: impl FnOnce(&Parsed<'_>, &Parsed<'_>) -> R) -> Result<R> {
    let outer = parse(bytes)?;
    if is_opaque_signed(&outer) {
        if let Some(inner) = smime::signed_content(outer.root_part().contents()) {
            if let Some(inner_msg) = hdr::full_parser().parse(&inner) {
                return Ok(f(&outer, &inner_msg));
            }
        }
    }
    Ok(f(&outer, &outer))
}

/// One attachment part: its id in `parts`, IMAP section number, and inline hints.
struct AttPart {
    id: usize,
    section: String,
    in_body: bool,
    in_related: bool,
}

fn child_section(parent: &str, i: usize) -> String {
    if parent.is_empty() {
        i.to_string()
    } else {
        format!("{parent}.{i}")
    }
}

fn collect(
    m: &Parsed<'_>,
    id: usize,
    section: &str,
    in_related: bool,
    body: &HashSet<usize>,
    depth: u32,
    out: &mut Vec<AttPart>,
) {
    let Some(part) = m.parts.get(id) else { return };
    if let PartType::Multipart(children) = &part.body {
        if depth > 64 {
            return;
        }
        let related = part.is_content_type("multipart", "related");
        for (i, c) in children.iter().enumerate() {
            collect(m, *c as usize, &child_section(section, i + 1), related, body, depth + 1, out);
        }
        return;
    }
    let in_body = body.contains(&id);
    let named = part.attachment_name().is_some();
    let include = match &part.body {
        PartType::Text(t) | PartType::Html(t) => {
            if in_body {
                id != 0 && named
            } else {
                named || !t.trim().is_empty()
            }
        }
        _ => true,
    };
    if include {
        let section = if section.is_empty() { "1".to_string() } else { section.to_string() };
        out.push(AttPart { id, section, in_body, in_related });
    }
}

/// Attachment parts in document (depth-first) order; the position is the attachment index.
fn attachment_parts(m: &Parsed<'_>) -> Vec<AttPart> {
    let body: HashSet<usize> = m.text_body.iter().chain(&m.html_body).map(|i| *i as usize).collect();
    let mut out = Vec::new();
    if !m.parts.is_empty() {
        collect(m, 0, "", false, &body, 0, &mut out);
    }
    out
}

fn mime_type(p: &MessagePart<'_>) -> Option<String> {
    let ct = p.content_type()?;
    Some(match ct.subtype() {
        Some(st) => format!("{}/{}", ct.ctype(), st).to_ascii_lowercase(),
        None => ct.ctype().to_ascii_lowercase(),
    })
}

fn is_calendar(p: &MessagePart<'_>) -> bool {
    matches!(mime_type(p).as_deref(), Some("text/calendar" | "application/ics"))
        || p.attachment_name().is_some_and(|n| n.to_ascii_lowercase().ends_with(".ics"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Embedded {
    No,
    /// MIME message we resolve ourselves.
    Mime,
    /// Outlook .msg — resolved by stv-library via stv-msg.
    OutlookMsg,
    Tnef,
}

fn classify(p: &MessagePart<'_>) -> Embedded {
    let mime = mime_type(p);
    let lname = p.attachment_name().map(str::to_ascii_lowercase).unwrap_or_default();
    if matches!(p.body, PartType::Message(_))
        || matches!(mime.as_deref(), Some("message/rfc822" | "message/global"))
        || lname.ends_with(".eml")
    {
        return Embedded::Mime;
    }
    if matches!(mime.as_deref(), Some("application/ms-tnef" | "application/vnd.ms-tnef")) || lname == "winmail.dat" {
        return Embedded::Tnef;
    }
    if matches!(mime.as_deref(), Some("application/vnd.ms-outlook")) || lname.ends_with(".msg") {
        return Embedded::OutlookMsg;
    }
    Embedded::No
}

/// Transfer-decoded bytes of a part (text parts keep their original charset bytes).
fn part_bytes(m: &Parsed<'_>, p: &MessagePart<'_>) -> Vec<u8> {
    match &p.body {
        PartType::Text(_) | PartType::Html(_) => {
            let raw = m.raw_message.get(p.offset_body as usize..p.offset_end as usize).unwrap_or_default();
            let decoded = match p.encoding {
                Encoding::None => Some(raw.to_vec()),
                Encoding::Base64 => base64_decode(raw),
                Encoding::QuotedPrintable => quoted_printable_decode(raw),
            };
            decoded.unwrap_or_else(|| p.contents().to_vec())
        }
        _ => p.contents().to_vec(),
    }
}

fn attachment_meta(index: u32, m: &Parsed<'_>, a: &AttPart) -> AttachmentMeta {
    let p = &m.parts[a.id];
    let mut filename = p.attachment_name().map(str::to_string);
    if filename.is_none() {
        if let PartType::Message(nested) = &p.body {
            let subj = nested.subject().unwrap_or("message").trim();
            let subj = if subj.is_empty() { "message" } else { subj };
            filename = Some(format!("{subj}.eml"));
        }
    }
    // An unnamed `text/calendar` part is the invite's machine-readable body (shown as the
    // item card), not a file the sender attached.
    let calendar_body = filename.is_none() && is_calendar(p);
    if calendar_body {
        filename = Some("invite.ics".into());
    } else if filename.is_none()
        && matches!(mime_type(p).as_deref(), Some("text/vcard" | "text/x-vcard" | "text/directory"))
    {
        filename = Some("contact.vcf".into());
    }
    let content_id = p
        .content_id()
        .map(|c| c.trim().trim_start_matches('<').trim_end_matches('>').to_string())
        .filter(|c| !c.is_empty());
    let disp_inline = p.content_disposition().is_some_and(|d| d.is_inline());
    let disp_attach = p.content_disposition().is_some_and(|d| d.is_attachment());
    AttachmentMeta {
        index,
        filename,
        mime_type: mime_type(p),
        // What `attachment()` returns: text parts keep their charset, so the decoded
        // (UTF-8) length would misreport non-UTF-8 text attachments.
        size: match p.body {
            PartType::Text(_) | PartType::Html(_) => part_bytes(m, p).len(),
            _ => p.len(),
        } as u64,
        inline: !disp_attach && (disp_inline || a.in_body || calendar_body || (a.in_related && content_id.is_some())),
        content_id,
        kind: match classify(p) {
            Embedded::No => AttachmentKind::File,
            Embedded::Mime | Embedded::OutlookMsg => AttachmentKind::EmbeddedMessage,
            Embedded::Tnef => AttachmentKind::Tnef,
        },
    }
}

fn body_of(m: &Parsed<'_>) -> Body {
    let mut body = Body::default();
    let html_ids: Vec<usize> = m.html_body.iter().map(|i| *i as usize).collect();
    let text_ids: Vec<usize> = m.text_body.iter().map(|i| *i as usize).collect();
    let part = |i: usize| m.parts.get(i);
    if html_ids.iter().any(|&i| part(i).is_some_and(|p| matches!(p.body, PartType::Html(_)))) {
        let mut s = String::new();
        for p in html_ids.iter().filter_map(|&i| part(i)) {
            match &p.body {
                PartType::Html(h) => s.push_str(h),
                PartType::Text(t) => s.push_str(&text_to_html(t)),
                _ => {}
            }
        }
        body.html = Some(s);
    }
    if text_ids.iter().any(|&i| part(i).is_some_and(|p| matches!(p.body, PartType::Text(_)))) {
        let mut s = String::new();
        for p in text_ids.iter().filter_map(|&i| part(i)) {
            let piece = match &p.body {
                PartType::Text(t) => t.to_string(),
                PartType::Html(h) => html_to_text(h),
                _ => continue,
            };
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&piece);
        }
        body.text = Some(s);
    }
    body
}

fn unfold(raw: &[u8]) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(s.len());
    for line in s.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

fn properties(m: &Parsed<'_>) -> Vec<PropertyView> {
    let raw = &m.raw_message;
    m.root_part()
        .headers
        .iter()
        .map(|h| {
            let name = raw
                .get(h.offset_field as usize..h.offset_start as usize)
                .map(|n| String::from_utf8_lossy(n).trim_end_matches(':').trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| h.name.as_str().to_string());
            let value = match &h.value {
                HeaderValue::Text(t) => t.to_string(),
                _ => unfold(raw.get(h.offset_start as usize..h.offset_end as usize).unwrap_or_default()),
            };
            PropertyView { tag: name, name: String::new(), type_name: "MIME header".into(), value }
        })
        .collect()
}

fn build_message(
    outer: &Parsed<'_>,
    content: &Parsed<'_>,
    size: u64,
    locator: Option<Locator>,
    folder: Option<FolderId>,
) -> Message {
    let info = hdr::info_from_message(outer);
    let encrypted = info.flags & F_ENCRYPTED != 0;
    let mut header = Header { locator, folder, size, ..Header::default() };
    hdr::info_into_header(info, &mut header);
    let attachments: Vec<AttachmentMeta> =
        attachment_parts(content).iter().enumerate().map(|(i, a)| attachment_meta(i as u32, content, a)).collect();
    header.has_attachments = attachments.iter().any(|a| !a.inline);
    if !encrypted {
        // An invite (Outlook, Gmail, …) carries its event as a `text/calendar` part or `.ics` file;
        // the first one holding an event or task wins (a VFREEBUSY or oversized part is skipped).
        header.item =
            content.parts.iter().filter(|p| is_calendar(p)).find_map(|p| crate::ical::item_from_ical(p.contents()));
    }
    let body =
        if encrypted { Body { text: Some(ENCRYPTED_NOTICE.to_string()), ..Body::default() } } else { body_of(content) };
    let root = outer.root_part();
    let raw = outer.raw_message.get(root.offset_header as usize..root.offset_body as usize).unwrap_or_default();
    Message { header, body, attachments, raw_headers: Some(String::from_utf8_lossy(raw).trim_end().to_string()) }
}

/// Full message from RFC 5322 bytes.
pub fn message_from_mime(bytes: &[u8], locator: Option<Locator>, folder: Option<FolderId>) -> Result<Message> {
    with_content(bytes, |outer, content| build_message(outer, content, bytes.len() as u64, locator, folder))
}

/// Header fields of the outer message for the property inspector, in file order.
pub fn properties_from_mime(bytes: &[u8]) -> Result<Vec<PropertyView>> {
    Ok(properties(&parse(bytes)?))
}

/// Header only (cheap: parses the header block, and for multipart/mixed the part headers).
pub fn header_from_mime(bytes: &[u8], locator: Option<Locator>, folder: Option<FolderId>) -> Result<Header> {
    let end = hdr::header_end(bytes).unwrap_or(bytes.len());
    let mut info = hdr::info_from_bytes(&bytes[..end])
        .ok_or_else(|| Error::Format("not an RFC 5322 message (no header block)".into()))?;
    hdr::settle_attachments(&mut info, &bytes[end..]);
    let mut h = Header { locator, folder, size: bytes.len() as u64, ..Header::default() };
    hdr::info_into_header(info, &mut h);
    Ok(h)
}

/// Decoded bytes of attachment `index` (as numbered by [`message_from_mime`]).
pub fn attachment_from_mime(bytes: &[u8], index: u32) -> Result<Vec<u8>> {
    with_content(bytes, |_, content| {
        let parts = attachment_parts(content);
        parts
            .get(index as usize)
            .map(|a| part_bytes(content, &content.parts[a.id]))
            .ok_or_else(|| Error::NotFound(format!("attachment {index}")))
    })?
}

/// Bytes of an attached MIME message (for `Locator::Embedded`).
pub(crate) fn embedded_message_bytes(bytes: &[u8], index: u32) -> Result<Vec<u8>> {
    with_content(bytes, |_, content| {
        let parts = attachment_parts(content);
        let a = parts.get(index as usize).ok_or_else(|| Error::NotFound(format!("attachment {index}")))?;
        let p = &content.parts[a.id];
        match classify(p) {
            Embedded::Mime => Ok(part_bytes(content, p)),
            Embedded::OutlookMsg => {
                Err(Error::Unsupported("embedded Outlook .msg: resolve with stv-msg (message_from_msg_bytes)".into()))
            }
            Embedded::Tnef => {
                Err(Error::Unsupported("embedded TNEF: resolve with stv-msg (message_from_tnef_bytes)".into()))
            }
            Embedded::No => Err(Error::Unsupported(format!(
                "attachment {index} is not a MIME message; sniff its bytes for another format"
            ))),
        }
    })?
}

/// IMAP section number of each attachment (`"2"`, `"1.3"`), used for Apple Mail partial emlx.
pub(crate) fn attachment_sections(bytes: &[u8]) -> Result<Vec<String>> {
    with_content(bytes, |_, content| attachment_parts(content).into_iter().map(|a| a.section).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_attachment_size_matches_saved_bytes() {
        let msg = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n\
Content-Type: text/plain\r\n\r\nbody\r\n--b\r\n\
Content-Type: text/plain; charset=iso-8859-1\r\n\
Content-Disposition: attachment; filename=l1.txt\r\n\
Content-Transfer-Encoding: base64\r\n\r\nY2Fm6Q==\r\n--b--\r\n";
        let m = message_from_mime(msg, None, None).unwrap();
        let saved = attachment_from_mime(msg, 0).unwrap();
        assert_eq!(saved, b"caf\xe9");
        assert_eq!(m.attachments[0].size, saved.len() as u64);
    }
}

#[cfg(test)]
mod calendar_tests {
    use super::*;
    use mail_model::{ItemDetails, MeetingKind, When};

    const INVITE: &str = "From: Organizer <organizer@example.org>\r\nTo: Guest <guest@example.org>\r\n\
Subject: Board meeting\r\nMIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=\"b1\"\r\n\r\n--b1\r\n\
Content-Type: text/plain\r\n\r\nSee you there.\r\n--b1\r\nContent-Type: text/calendar; method=REQUEST; charset=UTF-8\r\n\r\n";

    #[test]
    fn invite_part_becomes_appointment_details() {
        let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nDTSTART:20010226T140000Z\r\n\
DTEND:20010226T150000Z\r\nLOCATION:Room 101\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let mime = format!("{INVITE}{ics}\r\n--b1--\r\n");
        let m = message_from_mime(mime.as_bytes(), None, None).unwrap();
        let Some(ItemDetails::Appointment(a)) = m.header.item else { panic!("no item: {:?}", m.header.item) };
        assert_eq!(a.start, Some(When::Time(983_196_000_000)));
        assert_eq!(a.location.as_deref(), Some("Room 101"));
        assert_eq!(a.meeting, Some(MeetingKind::Request));
        assert_eq!(m.body.text.as_deref().map(str::trim), Some("See you there."), "body unchanged");
        // The calendar alternative is the card's source, not a file the sender attached.
        let names: Vec<_> = m.attachments.iter().map(|a| (a.filename.as_deref(), a.inline)).collect();
        assert_eq!(names, [(Some("invite.ics"), true)]);
        assert!(!m.header.has_attachments);

        let plain = "From: a@b.example\r\nSubject: hi\r\n\r\nno invite\r\n";
        assert_eq!(message_from_mime(plain.as_bytes(), None, None).unwrap().header.item, None);
    }

    #[test]
    fn calendar_attachment_honours_transfer_encoding_and_charset() {
        let mime = "From: organizer@example.org\r\nSubject: Offsite\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b2\"\r\n\r\n--b2\r\nContent-Type: text/plain\r\n\r\nAttached.\r\n--b2\r\n\
Content-Type: application/octet-stream; name=\"invite.ics\"\r\nContent-Disposition: attachment; filename=\"invite.ics\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
QkVHSU46VkNBTEVOREFSDQpCRUdJTjpWRVZFTlQNCkRUU1RBUlQ7VkFMVUU9REFURToyMDI2MDMwMQ0KRU5EOlZFVkVOVA0KRU5EOlZDQUxFTkRBUg0K\r\n\
--b2\r\nContent-Type: text/calendar; charset=windows-1252\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n\
BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nLOCATION:Caf=E9\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n--b2--\r\n";
        let m = message_from_mime(mime.as_bytes(), None, None).unwrap();
        let Some(ItemDetails::Appointment(a)) = m.header.item else { panic!("no item: {:?}", m.header.item) };
        // The first calendar part (the base64 `.ics` file) wins.
        assert_eq!(a.start, Some(When::Date("2026-03-01".into())));
        assert!(a.all_day);

        let second = mime.replace("invite.ics", "notes.bin");
        let m = message_from_mime(second.as_bytes(), None, None).unwrap();
        let Some(ItemDetails::Appointment(a)) = m.header.item else { panic!("no item: {:?}", m.header.item) };
        assert_eq!(a.location.as_deref(), Some("Café"), "charset decoded");
    }

    #[test]
    fn unnamed_vcard_part_is_saved_as_contact_vcf() {
        let mime = "From: a@example.org\r\nSubject: card\r\nContent-Type: multipart/mixed; boundary=\"v\"\r\n\r\n\
--v\r\nContent-Type: text/plain\r\n\r\nMy card.\r\n--v\r\nContent-Type: text/vcard\r\n\r\nBEGIN:VCARD\r\nVERSION:3.0\r\n\
FN:Alex Example\r\nEND:VCARD\r\n--v--\r\n";
        let m = message_from_mime(mime.as_bytes(), None, None).unwrap();
        let names: Vec<_> = m.attachments.iter().map(|a| (a.filename.as_deref(), a.inline)).collect();
        assert_eq!(names, [(Some("contact.vcf"), false)]);
        assert!(m.header.has_attachments);
    }
}
