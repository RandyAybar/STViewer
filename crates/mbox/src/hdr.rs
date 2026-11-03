//! Header-block extraction (shared by list rows and `message()`) and the compact
//! per-row blob encoding kept in memory / in the sidecar cache.

use std::sync::LazyLock;

use mail_model::{Address, Header, MessageFlags};
use mail_parser::{HeaderName, HeaderValue, MessageParser, MimeHeaders};

use crate::util::{put_opt, put_str, put_varint, Reader};

pub(crate) const F_READ: u16 = 1;
pub(crate) const F_FLAGGED: u16 = 1 << 1;
pub(crate) const F_DRAFT: u16 = 1 << 2;
pub(crate) const F_DELETED: u16 = 1 << 3;
pub(crate) const F_ENCRYPTED: u16 = 1 << 4;
pub(crate) const F_SIGNED: u16 = 1 << 5;
pub(crate) const F_HAS_ATT: u16 = 1 << 6;
/// mbox message bounded by Content-Length over an unescaped `From ` line (mboxcl2): no `>From` unescape.
pub(crate) const F_NO_UNESCAPE: u16 = 1 << 7;
/// Apple Mail `.partial.emlx` (attachments stored beside the message).
pub(crate) const F_PARTIAL: u16 = 1 << 8;

/// Stored recipient / reference caps per row (full lists come from `message()`).
const MAX_ADDRS: usize = 200;
const MAX_REFS: usize = 64;

const GMAIL_LABEL_HEADERS: [&str; 2] = ["X-Gmail-Labels", "X-GM-LABELS"];

fn base_parser() -> MessageParser {
    let p = MessageParser::new()
        .with_mime_headers()
        .with_date_headers()
        .with_address_headers()
        .with_message_ids()
        .header_text(HeaderName::Subject)
        .header_text(HeaderName::Comments)
        .header_comma_separated(HeaderName::Keywords);
    GMAIL_LABEL_HEADERS.iter().fold(p, |p, h| p.header_text(HeaderName::Other((*h).into())))
}

static HEADER_PARSER: LazyLock<MessageParser> = LazyLock::new(|| base_parser().ignore_header(HeaderName::Received));
static FULL_PARSER: LazyLock<MessageParser> = LazyLock::new(base_parser);

/// Parser for list rows: `Received` skipped.
pub(crate) fn header_parser() -> &'static MessageParser {
    &HEADER_PARSER
}

/// Parser for full messages.
pub(crate) fn full_parser() -> &'static MessageParser {
    &FULL_PARSER
}

/// Owned header facts of one message.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct HeaderInfo {
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub date: Option<i64>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub conversation: Option<String>,
    pub labels: Vec<String>,
    pub flags: u16,
    /// `F_HAS_ATT` is only a guess from a multipart/mixed root; [`settle_attachments`]
    /// decides it from the part headers. Not stored.
    pub att_unsettled: bool,
}

fn addr_list(list: Option<&mail_parser::Address<'_>>, out: &mut Vec<Address>) {
    let Some(list) = list else { return };
    for mb in list.iter() {
        if out.len() >= MAX_ADDRS {
            break;
        }
        let name = mb.name().map(str::trim).filter(|s| !s.is_empty());
        let email = mb.address().map(str::trim).filter(|s| !s.is_empty());
        if name.is_none() && email.is_none() {
            continue;
        }
        out.push(Address { name: name.map(str::to_string), email: email.map(str::to_string) });
    }
}

fn raw_text<'m>(m: &'m mail_parser::Message<'_>, name: &'static str) -> Option<&'m str> {
    m.header(HeaderName::Other(name.into())).and_then(|v| v.as_text()).map(str::trim)
}

/// Message ids of an id-list header value.
pub(crate) fn id_list<'v>(v: &'v HeaderValue<'_>) -> Vec<&'v str> {
    match v {
        HeaderValue::Text(t) => vec![t.as_ref()],
        HeaderValue::TextList(l) => l.iter().map(|s| s.as_ref()).collect(),
        _ => Vec::new(),
    }
}

/// Splits a Gmail label list: comma separated, labels containing commas are double-quoted.
pub(crate) fn split_labels(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ',' if !quoted => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s.strip_prefix("0x").unwrap_or(s);
    u32::from_str_radix(s.get(..s.len().min(8))?, 16).ok()
}

/// Content-type driven flags (`has attachments`, encrypted, signed) for a root header block,
/// and whether `F_HAS_ATT` is only the multipart/mixed guess.
fn content_flags(m: &mail_parser::Message<'_>) -> (u16, bool) {
    let mut f = 0;
    let ct = m.content_type();
    let (t, st) = match ct {
        Some(ct) => (ct.ctype().to_ascii_lowercase(), ct.subtype().unwrap_or("").to_ascii_lowercase()),
        None => (String::new(), String::new()),
    };
    let mixed = (t.as_str(), st.as_str()) == ("multipart", "mixed");
    let mut att = match (t.as_str(), st.as_str()) {
        ("multipart", "mixed") => true,
        ("multipart", _) | ("text", _) | ("", _) => false,
        ("application", "pkcs7-mime" | "x-pkcs7-mime") => false,
        _ => true,
    };
    let mut guess = mixed;
    if m.content_disposition().is_some_and(|d| d.is_attachment()) {
        att = true;
        guess = false;
    }
    if let Some(v) = raw_text(m, "X-MS-Has-Attach") {
        att = v.eq_ignore_ascii_case("yes");
        guess = false;
    }
    if att {
        f |= F_HAS_ATT;
    }
    match (t.as_str(), st.as_str()) {
        ("multipart", "signed") => f |= F_SIGNED,
        ("multipart", "encrypted") => f |= F_ENCRYPTED,
        ("application", "pkcs7-mime" | "x-pkcs7-mime") => {
            let kind = ct.and_then(|c| c.attribute("smime-type")).unwrap_or("").to_ascii_lowercase();
            if kind == "signed-data" {
                f |= F_SIGNED;
            } else {
                f |= F_ENCRYPTED;
            }
        }
        _ => {}
    }
    (f, guess)
}

/// Extracts everything a list row needs from a parsed header block.
pub(crate) fn info_from_message(m: &mail_parser::Message<'_>) -> HeaderInfo {
    let mut info =
        HeaderInfo { subject: m.subject().map(|s| s.trim().to_string()).unwrap_or_default(), ..HeaderInfo::default() };
    let mut from = Vec::new();
    addr_list(m.from(), &mut from);
    if from.is_empty() {
        addr_list(m.sender(), &mut from);
    }
    info.from = from.into_iter().next();
    addr_list(m.to(), &mut info.to);
    addr_list(m.cc(), &mut info.cc);
    addr_list(m.bcc(), &mut info.bcc);
    info.date = m.date().filter(|d| d.is_valid()).map(|d| d.to_timestamp() * 1000);
    info.message_id = m.message_id().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    info.in_reply_to = id_list(m.in_reply_to()).first().map(|s| s.trim()).filter(|s| !s.is_empty()).map(str::to_string);
    let refs = id_list(m.references());
    info.references = refs
        .iter()
        .skip(refs.len().saturating_sub(MAX_REFS))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    info.conversation = raw_text(m, "X-GM-THRID").filter(|s| !s.is_empty()).map(str::to_string);
    for h in GMAIL_LABEL_HEADERS {
        if let Some(v) = raw_text(m, h) {
            info.labels = split_labels(v);
            break;
        }
    }

    // Status flags: Thunderbird, then mutt/pine Status/X-Status, then Gmail labels.
    let mut flags = F_READ;
    if let Some(st) = raw_text(m, "X-Mozilla-Status").and_then(parse_hex) {
        flags = 0;
        if st & 0x0001 != 0 {
            flags |= F_READ;
        }
        if st & 0x0004 != 0 {
            flags |= F_FLAGGED;
        }
        if st & 0x0008 != 0 {
            flags |= F_DELETED;
        }
    } else if let Some(st) = raw_text(m, "Status") {
        if !st.contains('R') {
            flags &= !F_READ;
        }
    }
    if let Some(xs) = raw_text(m, "X-Status") {
        if xs.contains('F') {
            flags |= F_FLAGGED;
        }
        if xs.contains('D') {
            flags |= F_DELETED;
        }
        if xs.contains('T') {
            flags |= F_DRAFT;
        }
    }
    for l in &info.labels {
        match l.as_str() {
            "Unread" => flags &= !F_READ,
            "Starred" => flags |= F_FLAGGED,
            "Drafts" | "Draft" => flags |= F_DRAFT,
            _ => {}
        }
    }
    if m.header(HeaderName::Other("X-Mozilla-Draft-Info".into())).is_some() {
        flags |= F_DRAFT;
    }
    let (content, unsettled) = content_flags(m);
    info.flags = flags | content;
    info.att_unsettled = unsettled;
    info
}

/// Parses a header block (or a whole message) into [`HeaderInfo`]; `None` if no header block.
pub(crate) fn info_from_bytes(bytes: &[u8]) -> Option<HeaderInfo> {
    let m = header_parser().parse_headers(bytes)?;
    Some(info_from_message(&m))
}

/// Longest part header block looked at by [`settle_attachments`].
const PART_HEADER_MAX: usize = 16 * 1024;

/// Settles the attachment flag of a multipart/mixed message from its part headers in `body`
/// (the whole body or a prefix of it): set only if some part is an attachment, so a
/// text + HTML (+ invite) message gets no paperclip. A prefix that ends before the first
/// attachment part leaves the flag unset; opening the message corrects it.
pub(crate) fn settle_attachments(info: &mut HeaderInfo, body: &[u8]) {
    if !std::mem::take(&mut info.att_unsettled) {
        return;
    }
    if !parts_have_attachment(body) {
        info.flags &= !F_HAS_ATT;
    }
}

/// True if a MIME part header block after a `--boundary` line in `body` describes an
/// attachment (the same parts `message()` lists as non-inline attachments, judged from
/// the headers alone).
fn parts_have_attachment(body: &[u8]) -> bool {
    let starts = std::iter::once(0).chain(memchr::memmem::find_iter(body, b"\n--").map(|p| p + 1));
    for s in starts {
        let line = &body[s..];
        if !line.starts_with(b"--") {
            continue;
        }
        let Some(nl) = memchr::memchr(b'\n', line) else { break };
        let part = &line[nl + 1..];
        let part = &part[..part.len().min(PART_HEADER_MAX)];
        if let Some(end) = header_end(part) {
            if part_is_attachment(&part[..end]) {
                return true;
            }
        }
    }
    false
}

fn part_is_attachment(block: &[u8]) -> bool {
    let Some(m) = header_parser().parse_headers(block) else { return false };
    let (t, st) = m.content_type().map_or(("", ""), |ct| (ct.ctype(), ct.subtype().unwrap_or("")));
    if t.eq_ignore_ascii_case("multipart") {
        return false;
    }
    if let Some(d) = m.content_disposition() {
        if d.is_attachment() {
            return true;
        }
        if d.is_inline() {
            return false;
        }
    }
    if t.eq_ignore_ascii_case("message") && (st.eq_ignore_ascii_case("rfc822") || st.eq_ignore_ascii_case("global")) {
        return true;
    }
    // A named part is a file unless it is referenced from the body by Content-ID.
    m.attachment_name().is_some() && m.content_id().is_none()
}

/// End of the header block (index just past the blank line), or `None` if there is none.
pub(crate) fn header_end(data: &[u8]) -> Option<usize> {
    if data.first() == Some(&b'\n') {
        return Some(1);
    }
    if data.starts_with(b"\r\n") {
        return Some(2);
    }
    for nl in memchr::memchr_iter(b'\n', data) {
        match data.get(nl + 1) {
            Some(b'\n') => return Some(nl + 2),
            Some(b'\r') if data.get(nl + 2) == Some(&b'\n') => return Some(nl + 3),
            _ => {}
        }
    }
    None
}

// ---- blob codec ----
// Layout: labels(varint n, varint ids) path(str) subject(str) from(opt name, opt email)
//         to/cc/bcc(varint n, pairs of opt) message_id(opt) in_reply_to(opt) conversation(opt)
//         references(varint n, str)

fn put_addrs(out: &mut Vec<u8>, a: &[Address]) {
    put_varint(out, a.len() as u64);
    for x in a {
        put_opt(out, x.name.as_deref());
        put_opt(out, x.email.as_deref());
    }
}

/// Encodes everything after the label ids.
pub(crate) fn encode_rest(out: &mut Vec<u8>, path: &str, info: &HeaderInfo) {
    put_str(out, path);
    put_str(out, &info.subject);
    put_opt(out, info.from.as_ref().and_then(|a| a.name.as_deref()));
    put_opt(out, info.from.as_ref().and_then(|a| a.email.as_deref()));
    put_addrs(out, &info.to);
    put_addrs(out, &info.cc);
    put_addrs(out, &info.bcc);
    put_opt(out, info.message_id.as_deref());
    put_opt(out, info.in_reply_to.as_deref());
    put_opt(out, info.conversation.as_deref());
    put_varint(out, info.references.len() as u64);
    for r in &info.references {
        put_str(out, r);
    }
}

#[cfg(test)]
pub(crate) fn put_label_ids(out: &mut Vec<u8>, ids: &[u32]) {
    put_varint(out, ids.len() as u64);
    for id in ids {
        put_varint(out, u64::from(*id));
    }
}

pub(crate) fn blob_labels(blob: &[u8]) -> impl Iterator<Item = u32> + '_ {
    let mut r = Reader::new(blob);
    let n = r.varint().unwrap_or(0);
    (0..n).map_while(move |_| r.varint().map(|v| v as u32))
}

fn skip_labels(r: &mut Reader<'_>) -> Option<()> {
    let n = r.varint()?;
    for _ in 0..n {
        r.varint()?;
    }
    Some(())
}

/// Relative file path stored for file-backed rows.
pub(crate) fn blob_path(blob: &[u8]) -> Option<&str> {
    let mut r = Reader::new(blob);
    skip_labels(&mut r)?;
    r.str()
}

/// `(subject, from display)` used for sorting.
pub(crate) fn blob_sort_fields(blob: &[u8]) -> Option<(&str, &str)> {
    let mut r = Reader::new(blob);
    skip_labels(&mut r)?;
    r.str()?;
    let subject = r.str()?;
    let name = r.opt()?;
    let email = r.opt()?;
    Some((subject, name.or(email).unwrap_or("")))
}

fn get_addrs(r: &mut Reader<'_>) -> Option<Vec<Address>> {
    let n = r.varint()? as usize;
    let mut v = Vec::with_capacity(n.min(MAX_ADDRS));
    for _ in 0..n {
        let name = r.opt()?.map(str::to_string);
        let email = r.opt()?.map(str::to_string);
        v.push(Address { name, email });
    }
    Some(v)
}

/// Fills `h` (subject, addresses, ids, labels) from a blob.
pub(crate) fn decode_into(blob: &[u8], labels: &[String], h: &mut Header) -> Option<()> {
    let mut r = Reader::new(blob);
    let n = r.varint()?;
    for _ in 0..n {
        let id = r.varint()? as usize;
        h.labels.push(labels.get(id)?.clone());
    }
    r.str()?;
    h.subject = r.str()?.to_string();
    let name = r.opt()?.map(str::to_string);
    let email = r.opt()?.map(str::to_string);
    h.from = (name.is_some() || email.is_some()).then_some(Address { name, email });
    h.to = get_addrs(&mut r)?;
    h.cc = get_addrs(&mut r)?;
    h.bcc = get_addrs(&mut r)?;
    h.message_id = r.opt()?.map(str::to_string);
    h.in_reply_to = r.opt()?.map(str::to_string);
    h.conversation = r.opt()?.map(str::to_string);
    let n = r.varint()? as usize;
    h.references = Vec::with_capacity(n.min(MAX_REFS));
    for _ in 0..n {
        h.references.push(r.str()?.to_string());
    }
    Some(())
}

pub(crate) fn flags_to_model(f: u16) -> MessageFlags {
    MessageFlags {
        read: f & F_READ != 0,
        flagged: f & F_FLAGGED != 0,
        draft: f & F_DRAFT != 0,
        deleted: f & F_DELETED != 0,
        encrypted: f & F_ENCRYPTED != 0,
        signed: f & F_SIGNED != 0,
    }
}

/// Fills a model header from owned info (used by the MIME helpers).
pub(crate) fn info_into_header(info: HeaderInfo, h: &mut Header) {
    h.subject = info.subject;
    h.from = info.from;
    h.to = info.to;
    h.cc = info.cc;
    h.bcc = info.bcc;
    h.date = info.date;
    h.message_id = info.message_id;
    h.in_reply_to = info.in_reply_to;
    h.references = info.references;
    h.conversation = info.conversation;
    h.labels = info.labels;
    h.has_attachments = info.flags & F_HAS_ATT != 0;
    h.flags = flags_to_model(info.flags);
}

/// Subject normalised for sorting: reply/forward prefixes stripped, lowercase.
pub(crate) fn sort_subject(s: &str) -> String {
    const PREFIXES: [&str; 9] = ["re:", "fw:", "fwd:", "aw:", "wg:", "sv:", "vs:", "re :", "fwd :"];
    let mut t = s.trim_start();
    while let Some(p) = PREFIXES.iter().find(|p| t.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p))) {
        t = t[p.len()..].trim_start();
    }
    t.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_split_quotes() {
        assert_eq!(
            split_labels(r#"Inbox,"Work, Projects",Category Personal, Opened"#),
            vec!["Inbox", "Work, Projects", "Category Personal", "Opened"]
        );
        assert!(split_labels(" , ").is_empty());
    }

    #[test]
    fn mixed_messages_get_a_paperclip_only_for_real_attachments() {
        let has_att = |parts: &str| {
            let msg = format!(
                "Subject: x\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\npreamble\r\n--b\r\n\
Content-Type: text/plain\r\n\r\nhello\r\n-- \r\nSam\r\n\r\n{parts}--b--\r\n"
            );
            let end = header_end(msg.as_bytes()).unwrap();
            let mut info = info_from_bytes(&msg.as_bytes()[..end]).unwrap();
            assert!(info.att_unsettled && info.flags & F_HAS_ATT != 0, "guess before the parts are seen");
            settle_attachments(&mut info, &msg.as_bytes()[end..]);
            info.flags & F_HAS_ATT != 0
        };
        assert!(!has_att(""), "text only (a signature dash line is not a part)");
        assert!(!has_att("--b\r\nContent-Type: text/html\r\n\r\n<p>hi</p>\r\n"));
        assert!(!has_att("--b\r\nContent-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\n"));
        assert!(!has_att("--b\r\nContent-Type: image/png; name=\"logo.png\"\r\nContent-ID: <l@x>\r\n\r\nAA==\r\n"));
        assert!(!has_att(
            "--b\r\nContent-Type: image/png\r\nContent-Disposition: inline; filename=a.png\r\n\r\nAA==\r\n"
        ));
        assert!(has_att("--b\r\nContent-Type: application/pdf; name=\"r.pdf\"\r\n\r\nJVBE\r\n"));
        assert!(has_att(
            "--b\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment\r\n\r\nAA\r\n"
        ));
        assert!(has_att("--b\r\nContent-Type: message/rfc822\r\n\r\nSubject: fwd\r\n\r\nx\r\n"));
        assert!(has_att(
            "--b\r\nContent-Type: multipart/alternative; boundary=c\r\n\r\n--c\r\nContent-Type: text/plain\r\n\r\nx\r\n\
--c--\r\n--b\r\nContent-Type: text/calendar; name=\"meeting.ics\"\r\n\r\nBEGIN:VCALENDAR\r\n"
        ));

        // Other roots keep their verdict without looking at the body.
        let mut info = info_from_bytes(b"Content-Type: multipart/alternative; boundary=b\r\n\r\n").unwrap();
        settle_attachments(&mut info, b"--b\r\nContent-Disposition: attachment\r\n\r\nx\r\n");
        assert_eq!(info.flags & F_HAS_ATT, 0);
        let mut info =
            info_from_bytes(b"Content-Type: multipart/mixed; boundary=b\r\nX-MS-Has-Attach: yes\r\n\r\n").unwrap();
        settle_attachments(&mut info, b"");
        assert_ne!(info.flags & F_HAS_ATT, 0, "the sender's own flag wins");
    }

    #[test]
    fn header_end_variants() {
        assert_eq!(header_end(b"A: b\n\nbody"), Some(6));
        assert_eq!(header_end(b"A: b\r\n\r\nbody"), Some(8));
        assert_eq!(header_end(b"\nbody"), Some(1));
        assert_eq!(header_end(b"A: b\nC: d\n"), None);
    }

    #[test]
    fn blob_roundtrip() {
        let info = HeaderInfo {
            subject: "Hi".into(),
            from: Some(Address { name: Some("Ann".into()), email: Some("a@x".into()) }),
            to: vec![Address { name: None, email: Some("b@x".into()) }],
            message_id: Some("id@x".into()),
            references: vec!["r1@x".into(), "r2@x".into()],
            conversation: Some("123".into()),
            ..Default::default()
        };
        let mut blob = Vec::new();
        put_label_ids(&mut blob, &[1, 0]);
        encode_rest(&mut blob, "a/b.eml", &info);
        let labels = vec!["L0".to_string(), "L1".to_string()];
        let mut h = Header::default();
        decode_into(&blob, &labels, &mut h).unwrap();
        assert_eq!(h.subject, "Hi");
        assert_eq!(h.labels, vec!["L1", "L0"]);
        assert_eq!(h.to[0].email.as_deref(), Some("b@x"));
        assert_eq!(h.references, vec!["r1@x", "r2@x"]);
        assert_eq!(blob_path(&blob), Some("a/b.eml"));
        assert_eq!(blob_sort_fields(&blob), Some(("Hi", "Ann")));
        assert_eq!(blob_labels(&blob).collect::<Vec<_>>(), vec![1, 0]);
        // Truncated blob decodes to None, never panics.
        for cut in 0..blob.len() {
            let mut h = Header::default();
            let _ = decode_into(&blob[..cut], &labels, &mut h);
        }
    }

    #[test]
    fn subject_normalisation() {
        assert_eq!(sort_subject("Re: Fwd: RE: Hello"), "hello");
        assert_eq!(sort_subject("Report"), "report");
        assert_eq!(sort_subject("Ré: Ünï"), "ré: ünï");
        assert_eq!(sort_subject("  AW :x"), "aw :x");
    }
}
