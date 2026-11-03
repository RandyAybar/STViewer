//! Turning a source message into an index document.

use std::collections::HashMap;

use mail_model::{
    Address, AttachmentKind, AttachmentMeta, FolderId, Header, ItemDetails, Locator, MailSource, Message, SourceId,
};
use tantivy::schema::Field;
use tantivy::TantivyDocument;

use crate::extract;
use crate::schema::Fields;
use crate::util::{normalize_subject, push_collapsed};

/// Body text indexed per message.
const MAX_BODY_TEXT: usize = 4 * 1024 * 1024;
/// Attachment text indexed per message (sum over attachments).
const MAX_ATTACHMENTS_TEXT: usize = 4 * 1024 * 1024;
/// Stored body prefix used for snippets.
pub(crate) const PREVIEW_BYTES: usize = 2048;

/// Everything extracted from a source for one document.
pub(crate) struct DocInput {
    pub header: Header,
    pub body_text: String,
    pub attachment_names: Vec<String>,
    pub attachment_text: String,
    /// The message's attachments, for the attachment listing.
    pub attachments: Vec<AttachmentMeta>,
    /// Subjects of embedded messages without a file name, by attachment index.
    pub embedded_subjects: HashMap<u32, String>,
}

impl DocInput {
    /// A document input carrying only the listing header (message unreadable).
    pub(crate) fn from_header(header: Header) -> Self {
        DocInput {
            header,
            body_text: String::new(),
            attachment_names: Vec::new(),
            attachment_text: String::new(),
            attachments: Vec::new(),
            embedded_subjects: HashMap::new(),
        }
    }
}

/// `"{source}:{locator json}"`. The source prefix keeps keys unique across sources
/// (two PSTs both have nid 0x200024) so deletes by key are exact.
pub(crate) fn make_key(source: SourceId, loc: &Locator) -> String {
    let json = serde_json::to_string(loc).expect("Locator serialises");
    format!("{}:{json}", source.0)
}

/// Loads the message body and extractable attachments for `header`.
/// Returns `false` in the second slot when the message itself could not be read; the
/// document then carries the listing header only (still findable by subject/sender/date).
pub(crate) fn gather(src: &dyn MailSource, header: Header) -> (DocInput, bool) {
    let mut input = DocInput::from_header(header);
    let Some(loc) = input.header.locator.clone() else {
        return (input, false);
    };
    let mut msg = match src.message(&loc) {
        Ok(m) => m,
        Err(_) => return (input, false),
    };
    merge_header(&mut input.header, &msg);
    input.body_text = extract::body_text(std::mem::take(&mut msg.body), MAX_BODY_TEXT);

    for att in &msg.attachments {
        if let Some(name) = att.filename.as_deref().filter(|n| !n.is_empty()) {
            input.attachment_names.push(name.to_string());
        }
        if input.attachment_text.len() >= MAX_ATTACHMENTS_TEXT {
            continue;
        }
        match att.kind {
            AttachmentKind::EmbeddedMessage | AttachmentKind::Tnef => {
                // One level of native embedding; cross-format embeddings the source cannot
                // resolve simply contribute their file name.
                let eloc = Locator::Embedded { parent: Box::new(loc.clone()), attachment: att.index };
                if let Ok(inner) = src.message(&eloc) {
                    if att.filename.is_none() && !inner.header.subject.is_empty() {
                        input.attachment_names.push(inner.header.subject.clone());
                        input.embedded_subjects.insert(att.index, inner.header.subject.clone());
                    }
                    input.attachment_names.extend(inner.attachments.iter().filter_map(|a| a.filename.clone()));
                    let t = &mut input.attachment_text;
                    t.push_str(&inner.header.subject);
                    t.push('\n');
                    if let Some(from) = &inner.header.from {
                        push_address(t, from);
                        t.push('\n');
                    }
                    t.push_str(&extract::body_text(inner.body, extract::MAX_ATTACHMENT_TEXT));
                    t.push('\n');
                }
            }
            AttachmentKind::File => {
                if att.size > extract::MAX_ATTACHMENT_BYTES || extract::attachment_kind(att).is_none() {
                    continue;
                }
                let Ok(bytes) = src.attachment(&loc, att.index) else { continue };
                if let Some(text) = extract::attachment_text(att, &bytes) {
                    input.attachment_text.push_str(&text);
                    input.attachment_text.push('\n');
                }
            }
            AttachmentKind::Link | AttachmentKind::Ole => {}
        }
    }
    crate::util::truncate_at_char(&mut input.attachment_text, MAX_ATTACHMENTS_TEXT);
    input.attachments = msg.attachments;
    (input, true)
}

/// The listing header is authoritative (it is what `list()` shows and carries the
/// locator/folder); the full message fills in whatever the listing left out.
fn merge_header(h: &mut Header, msg: &Message) {
    let m = &msg.header;
    if h.subject.is_empty() {
        h.subject.clone_from(&m.subject);
    }
    if h.from.is_none() {
        h.from.clone_from(&m.from);
    }
    for (dst, src) in [(&mut h.to, &m.to), (&mut h.cc, &m.cc), (&mut h.bcc, &m.bcc)] {
        if dst.is_empty() {
            dst.clone_from(src);
        }
    }
    if h.date.is_none() {
        h.date = m.date;
    }
    if h.size == 0 {
        h.size = m.size;
    }
    if h.message_id.is_none() {
        h.message_id.clone_from(&m.message_id);
    }
    if h.in_reply_to.is_none() {
        h.in_reply_to.clone_from(&m.in_reply_to);
    }
    if h.references.is_empty() {
        h.references.clone_from(&m.references);
    }
    if h.conversation.is_none() {
        h.conversation.clone_from(&m.conversation);
    }
    if h.labels.is_empty() {
        h.labels.clone_from(&m.labels);
    }
    if h.message_class.is_none() {
        h.message_class.clone_from(&m.message_class);
    }
    // List rows carry only what the container's table holds (no attendees); the message's
    // details are complete.
    if m.item.is_some() {
        h.item.clone_from(&m.item);
    }
    h.has_attachments |= m.has_attachments || msg.attachments.iter().any(|a| !a.inline);
}

fn push_address(out: &mut String, a: &Address) {
    if let Some(n) = &a.name {
        out.push_str(n);
        out.push(' ');
    }
    if let Some(e) = &a.email {
        out.push_str(e);
    }
}

fn address_text(a: &Address) -> String {
    let mut s = String::new();
    push_address(&mut s, a);
    s
}

fn lower_email(a: &Address) -> Option<String> {
    a.email.as_deref().map(str::trim).filter(|e| !e.is_empty()).map(str::to_lowercase)
}

fn add_address(d: &mut TantivyDocument, text_f: Field, addr_f: Field, a: &Address) {
    d.add_text(text_f, address_text(a));
    if let Some(e) = lower_email(a) {
        d.add_text(addr_f, e);
    }
}

/// Plain searchable text of an item's details (one value per line).
fn details_text(item: &ItemDetails) -> String {
    let mut out = String::new();
    let mut push = |s: &str| {
        if !s.trim().is_empty() {
            out.push_str(s);
            out.push('\n');
        }
    };
    match item {
        ItemDetails::Appointment(a) => {
            for s in [&a.location, &a.recurrence].into_iter().flatten() {
                push(s);
            }
        }
        ItemDetails::Contact(c) => {
            for s in [&c.display_name, &c.given_name, &c.surname, &c.company, &c.job_title, &c.department, &c.web_page]
                .into_iter()
                .flatten()
            {
                push(s);
            }
            for p in &c.phones {
                push(&p.value);
                // Also as one run of digits, so `7135550100` finds `(713) 555-0100`.
                let digits: String = p.value.chars().filter(char::is_ascii_digit).collect();
                if digits.len() != p.value.trim().len() {
                    push(&digits);
                }
            }
            for a in &c.addresses {
                push(&a.value);
            }
        }
        ItemDetails::Task(t) => {
            if let Some(owner) = &t.owner {
                push(owner);
            }
        }
    }
    out
}

/// `is:` words for the item kind. Also from the message class, so items whose details could
/// not be decoded (or a meeting message decoded without its kind) still filter by kind.
fn kind_words(h: &Header) -> &'static [&'static str] {
    let class = h.message_class.as_deref().unwrap_or("").to_ascii_lowercase();
    let class_is = |c: &str| class == c || class.strip_prefix(c).is_some_and(|rest| rest.starts_with('.'));
    let meeting = class_is("ipm.schedule.meeting");
    match &h.item {
        Some(ItemDetails::Appointment(a)) if meeting || a.meeting.is_some() => &["appointment", "meeting"],
        Some(ItemDetails::Appointment(_)) => &["appointment"],
        Some(ItemDetails::Contact(_)) => &["contact"],
        Some(ItemDetails::Task(_)) => &["task"],
        None if meeting => &["appointment", "meeting"],
        None if class_is("ipm.appointment") => &["appointment"],
        None if class_is("ipm.contact") => &["contact"],
        None if class_is("ipm.task") => &["task"],
        None => &[],
    }
}

pub(crate) fn to_document(
    f: &Fields,
    source: SourceId,
    key: &str,
    input: &DocInput,
    folder_names: &HashMap<FolderId, Vec<String>>,
) -> TantivyDocument {
    let h = &input.header;
    let mut d = TantivyDocument::new();
    d.add_u64(f.source, u64::from(source.0));
    d.add_text(f.key, key);
    if let Some(folder) = &h.folder {
        d.add_text(f.folder, &folder.0);
        for name in folder_names.get(folder).into_iter().flatten() {
            d.add_text(f.folder_name, name);
        }
    }
    d.add_text(f.subject, &h.subject);
    d.add_text(f.subject_sort, normalize_subject(&h.subject));
    if let Some(from) = &h.from {
        d.add_text(f.from, address_text(from));
        // Facet key: the address, or the display name when the container has no SMTP
        // address (Exchange/Notes-era PSTs), so such senders still appear in the facet.
        match lower_email(from) {
            Some(e) => d.add_text(f.from_addr, e),
            None => {
                if let Some(n) = from.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                    d.add_text(f.from_addr, n);
                }
            }
        }
        let sort = from.name.as_deref().filter(|n| !n.trim().is_empty()).or(from.email.as_deref());
        d.add_text(f.from_sort, sort.unwrap_or("").trim().to_lowercase());
    }
    for (list, text_f, addr_f) in [(&h.to, f.to, f.to_addr), (&h.cc, f.cc, f.cc_addr), (&h.bcc, f.bcc, f.bcc_addr)] {
        for a in list {
            add_address(&mut d, text_f, addr_f, a);
        }
    }
    // Item participants: the organizer as sender (text only: the sender facet and sort stay
    // the message's own sender), every attendee (required, optional, resource) and a
    // contact's addresses as recipients, so `from:`/`to:` and bare words find them.
    match &h.item {
        Some(ItemDetails::Appointment(a)) => {
            if let Some(o) = &a.organizer {
                d.add_text(f.from, address_text(o));
            }
            for p in a.required.iter().chain(&a.optional).chain(&a.resources) {
                add_address(&mut d, f.to, f.to_addr, p);
            }
        }
        Some(ItemDetails::Contact(c)) => {
            for e in &c.emails {
                add_address(&mut d, f.to, f.to_addr, e);
            }
        }
        Some(ItemDetails::Task(_)) | None => {}
    }
    if let Some(item) = &h.item {
        let text = details_text(item);
        if !text.is_empty() {
            d.add_text(f.details, text);
        }
    }
    d.add_text(f.body, &input.body_text);
    let mut preview = String::with_capacity(PREVIEW_BYTES + 4);
    push_collapsed(&mut preview, &input.body_text, PREVIEW_BYTES);
    d.add_text(f.preview, preview);
    for name in &input.attachment_names {
        d.add_text(f.attachment_names, name);
    }
    if !input.attachment_text.is_empty() {
        d.add_text(f.attachment_text, &input.attachment_text);
    }
    for label in &h.labels {
        d.add_text(f.labels, label.trim().to_lowercase());
    }
    let fl = &h.flags;
    d.add_text(f.is, if fl.read { "read" } else { "unread" });
    for (on, word) in [
        (fl.flagged, "flagged"),
        (fl.draft, "draft"),
        (fl.deleted, "deleted"),
        (fl.encrypted, "encrypted"),
        (fl.signed, "signed"),
    ] {
        if on {
            d.add_text(f.is, word);
        }
    }
    for word in kind_words(h) {
        d.add_text(f.is, word);
    }
    if let Some(id) = h.message_id.as_deref().filter(|s| !s.is_empty()) {
        d.add_text(f.message_id, id);
    }
    if let Some(id) = h.in_reply_to.as_deref().filter(|s| !s.is_empty()) {
        d.add_text(f.in_reply_to, id);
    }
    for r in h.references.iter().filter(|s| !s.is_empty()) {
        d.add_text(f.references, r);
    }
    if let Some(c) = h.conversation.as_deref().filter(|s| !s.is_empty()) {
        d.add_text(f.conversation, c);
    }
    if let Some(date) = h.date {
        d.add_i64(f.date, date);
    }
    // Calendar time for appointments and tasks: `after:`/`before:`, the year facet and the
    // start sort place them where they happen, not when they were created or sent.
    if let Some(start) = h.start_ms() {
        d.add_i64(f.start, start);
        d.add_u64(f.year, crate::util::year_of_ms(start).max(0) as u64);
    }
    d.add_u64(f.size, h.size);
    d.add_bool(f.has_attachment, h.has_attachments);
    d.add_bytes(f.header, &serde_json::to_vec(h).expect("Header serialises"));
    d
}

/// `FolderId` → lowercased `[name, "parent/…/name"]` for `in:` queries.
pub(crate) fn folder_names(src: &dyn MailSource) -> HashMap<FolderId, Vec<String>> {
    let Ok(folders) = src.folders() else { return HashMap::new() };
    let by_id: HashMap<&FolderId, &mail_model::Folder> = folders.iter().map(|f| (&f.id, f)).collect();
    let mut out = HashMap::with_capacity(folders.len());
    for f in &folders {
        let mut path = vec![f.name.as_str()];
        let mut cur = f.parent.as_ref();
        // Bounded walk: a corrupt parent cycle must not hang indexing.
        for _ in 0..64 {
            let Some(p) = cur.and_then(|id| by_id.get(id)) else { break };
            path.push(p.name.as_str());
            cur = p.parent.as_ref();
        }
        path.reverse();
        let name = f.name.trim().to_lowercase();
        let full = path.join("/").to_lowercase();
        let mut names = vec![name];
        if full != names[0] {
            names.push(full);
        }
        out.insert(f.id.clone(), names);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_model::{Appointment, MeetingKind};

    #[test]
    fn counter_proposal_is_a_meeting() {
        // A MIME mail carrying METHOD:COUNTER has no message class: the item kind decides.
        let a = Appointment { meeting: Some(MeetingKind::Counter), ..Appointment::default() };
        let h = Header { item: Some(ItemDetails::Appointment(a)), ..Header::default() };
        assert_eq!(kind_words(&h), ["appointment", "meeting"]);
    }
}
