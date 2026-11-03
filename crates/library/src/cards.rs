//! vCard 3.0 (RFC 2426, with RFC 6350 escaping) and iCalendar (RFC 5545) writers for
//! exporting contacts, appointments and tasks.
//!
//! Output uses CRLF line ends and folds content lines at 75 octets without splitting a UTF-8
//! character. TEXT values are escaped; values whose grammar has no escapes (URIs, `RRULE`,
//! dates) are validated and left out when the archive holds something malformed, so hostile
//! input cannot inject lines or produce a file importers reject.

use std::collections::HashSet;
use std::io::{self, Write};

use mail_model::time::{civil_from_days, days_from_civil, MS_PER_DAY};
use mail_model::{
    Address, Appointment, BusyStatus, Contact, ItemDetails, MeetingKind, Message, SourceId, Task, TaskStatus, When,
};

use crate::text::body_text;

/// TEXT value escaping (RFC 5545 3.3.11, RFC 6350 3.4): `\`, `,`, `;` and line breaks;
/// other control characters, which content lines may not hold, are dropped.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => out.push_str("\\n"),
            '\t' => out.push('\t'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Parameter value: quoted when it contains `:;,` (RFC 5545 3.2); double quotes and control
/// characters, which a parameter value cannot hold, are dropped.
fn param(s: &str) -> String {
    let s: String = s.chars().filter(|&c| c != '"' && !c.is_control()).collect();
    if s.contains([':', ';', ',']) {
        format!("\"{s}\"")
    } else {
        s
    }
}

/// A URI-like value (e-mail address, web page) written verbatim: `None` when empty or when
/// it holds whitespace, control characters or quotes.
fn verbatim(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"')).then_some(s)
}

/// Writes one content line folded at 75 octets without splitting a UTF-8 character.
fn line(w: &mut impl Write, s: &str) -> io::Result<()> {
    let mut start = 0;
    let mut limit = 75;
    while s.len() - start > limit {
        let mut end = start + limit;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        w.write_all(&s.as_bytes()[start..end])?;
        w.write_all(b"\r\n ")?;
        start = end;
        // Continuation lines start with the space, which counts toward their 75 octets.
        limit = 74;
    }
    w.write_all(&s.as_bytes()[start..])?;
    w.write_all(b"\r\n")
}

/// `YYYY-MM-DD` that names a real day in years 0000–9999.
fn parse_day(d: &str) -> Option<(i64, u32, u32)> {
    let b = d.as_bytes();
    let shape = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    if !shape {
        return None;
    }
    let (y, m, day) = (d[..4].parse().ok()?, d[5..7].parse().ok()?, d[8..].parse().ok()?);
    ((1..=12).contains(&m) && civil_from_days(days_from_civil(y, m, day)) == (y, m, day)).then_some((y, m, day))
}

/// A validated iCalendar DATE (`20240301`) or UTC DATE-TIME (`20240301T150000Z`).
struct CalTime {
    date: bool,
    value: String,
    /// UTC ms (midnight UTC for a date): orders a start before its end.
    ms: i64,
}

impl CalTime {
    fn new(w: &When) -> Option<CalTime> {
        match w {
            When::Time(ms) => {
                let secs = ms.div_euclid(1000);
                let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
                let s = secs.rem_euclid(86_400);
                (0..=9999).contains(&y).then(|| CalTime {
                    date: false,
                    value: format!("{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z", s / 3600, s % 3600 / 60, s % 60),
                    ms: *ms,
                })
            }
            When::Date(d) => {
                let (y, m, day) = parse_day(d)?;
                Some(CalTime {
                    date: true,
                    value: format!("{y:04}{m:02}{day:02}"),
                    ms: days_from_civil(y, m, day) * MS_PER_DAY,
                })
            }
        }
    }

    /// As a UTC DATE-TIME (midnight UTC for a date), for properties that require one.
    fn to_time(&self) -> Option<CalTime> {
        CalTime::new(&When::Time(self.ms))
    }

    /// `DTSTART:20240301T150000Z` or `DTSTART;VALUE=DATE:20240301`.
    fn prop(&self, name: &str) -> String {
        if self.date {
            format!("{name};VALUE=DATE:{}", self.value)
        } else {
            format!("{name}:{}", self.value)
        }
    }
}

fn person(a: &Address) -> Option<(String, String)> {
    let email = a.email.as_deref().and_then(verbatim).filter(|e| e.contains('@'))?;
    let cn = a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| format!(";CN={}", param(n)));
    Some((cn.unwrap_or_default(), format!("mailto:{email}")))
}

fn description(msg: &Message) -> Option<String> {
    let text = body_text(&msg.body);
    // Control characters are dropped on output: a body of only those is no description.
    let text = text.trim_matches(|c: char| c.is_whitespace() || c.is_control());
    (!text.is_empty()).then(|| text.to_owned())
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// One vCard; `false` (nothing written) when the message is not a contact.
pub(crate) fn write_vcard(w: &mut impl Write, msg: &Message) -> io::Result<bool> {
    let Some(ItemDetails::Contact(c)) = &msg.header.item else { return Ok(false) };
    line(w, "BEGIN:VCARD")?;
    line(w, "VERSION:3.0")?;
    // FN is required and must not be empty.
    let given_sur =
        [c.given_name.as_deref(), c.surname.as_deref()].into_iter().filter_map(non_empty).collect::<Vec<_>>().join(" ");
    let fn_ = non_empty(c.display_name.as_deref())
        .or(non_empty(Some(given_sur.as_str())))
        .or(non_empty(Some(msg.header.subject.as_str())))
        .or_else(|| c.emails.iter().find_map(|e| non_empty(e.email.as_deref())))
        .unwrap_or("(no name)");
    line(w, &format!("FN:{}", escape(fn_)))?;
    line(
        w,
        &format!(
            "N:{};{};;;",
            escape(c.surname.as_deref().unwrap_or("")),
            escape(c.given_name.as_deref().unwrap_or(""))
        ),
    )?;
    write_contact_fields(w, c)?;
    if let Some(note) = description(msg) {
        line(w, &format!("NOTE:{}", escape(&note)))?;
    }
    line(w, "END:VCARD")?;
    Ok(true)
}

/// vCard 3.0 `TEL` types for the contact phone labels the readers produce.
fn phone_type(label: &str) -> &'static str {
    match label {
        "mobile" => "CELL,VOICE",
        "business" | "business 2" | "company" => "WORK,VOICE",
        "home" | "home 2" => "HOME,VOICE",
        "primary" => "PREF,VOICE",
        "car" => "CAR,VOICE",
        "business fax" | "fax" => "WORK,FAX",
        "home fax" => "HOME,FAX",
        "primary fax" => "PREF,FAX",
        "pager" => "PAGER",
        _ => "VOICE",
    }
}

fn write_contact_fields(w: &mut impl Write, c: &Contact) -> io::Result<()> {
    let company = non_empty(c.company.as_deref());
    match (company, non_empty(c.department.as_deref())) {
        (None, None) => {}
        (company, None) => line(w, &format!("ORG:{}", escape(company.unwrap_or(""))))?,
        (company, Some(dept)) => line(w, &format!("ORG:{};{}", escape(company.unwrap_or("")), escape(dept)))?,
    }
    if let Some(t) = non_empty(c.job_title.as_deref()) {
        line(w, &format!("TITLE:{}", escape(t)))?;
    }
    for e in c.emails.iter().filter_map(|a| a.email.as_deref().and_then(verbatim)) {
        line(w, &format!("EMAIL;TYPE=INTERNET:{}", escape(e)))?;
    }
    for p in c.phones.iter().filter(|p| !p.value.trim().is_empty()) {
        line(w, &format!("TEL;TYPE={}:{}", phone_type(&p.label), escape(p.value.trim())))?;
    }
    for a in c.addresses.iter().filter(|a| !a.value.trim().is_empty()) {
        let ty = match a.label.as_str() {
            "business" => "WORK",
            "home" => "HOME",
            _ => "POSTAL",
        };
        // The source holds one formatted block, not structured parts: put it in the street
        // field and give the same text as the label.
        let v = escape(a.value.trim());
        line(w, &format!("ADR;TYPE={ty}:;;{v};;;;"))?;
        line(w, &format!("LABEL;TYPE={ty}:{v}"))?;
    }
    let birthday = c.birthday.as_ref().and_then(CalTime::new).map(|t| t.value);
    if let Some(d) = birthday.filter(|d| d.len() >= 8) {
        line(w, &format!("BDAY:{}-{}-{}", &d[..4], &d[4..6], &d[6..8]))?;
    }
    if let Some(u) = c.web_page.as_deref().and_then(verbatim) {
        line(w, &format!("URL:{u}"))?;
    }
    Ok(())
}

/// What [`Calendar::add`] did with a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Added {
    Written,
    /// Neither an appointment nor a task, or an appointment with no time to place it at.
    NotApplicable,
    /// Same UID as a component already written (a meeting request, its replies and its
    /// calendar entry; a message selected twice): a UID may appear once per calendar.
    Duplicate,
}

/// iCalendar export: one VCALENDAR holding a VEVENT per appointment / meeting message and a
/// VTODO per task. Nothing at all is written until the first component (an empty
/// VCALENDAR is not valid iCalendar). The calendar has no `METHOD`, so it states events
/// rather than scheduling messages: a counter proposal ([`MeetingKind::Counter`], RFC 5546
/// `COUNTER`) only suggests a time for an event it shares the UID with, so it is skipped
/// rather than written as (or instead of) that event.
pub(crate) struct Calendar<W: Write> {
    w: W,
    /// `DTSTAMP` value: the export time.
    stamp: String,
    /// UIDs written so far.
    uids: HashSet<String>,
}

impl<W: Write> Calendar<W> {
    /// `stamp_ms` is the export time (`DTSTAMP` is required).
    pub(crate) fn new(w: W, stamp_ms: i64) -> Self {
        let stamp = CalTime::new(&When::Time(stamp_ms)).map_or_else(|| "19700101T000000Z".to_owned(), |t| t.value);
        Calendar { w, stamp, uids: HashSet::new() }
    }

    /// One VEVENT or VTODO for `msg` of `source`.
    pub(crate) fn add(&mut self, source: SourceId, msg: &Message) -> io::Result<Added> {
        enum Component<'a> {
            Event(&'a Appointment, CalTime),
            Todo(&'a Task),
        }
        let (component, uid) = match &msg.header.item {
            Some(ItemDetails::Appointment(a)) if a.meeting == Some(MeetingKind::Counter) => {
                return Ok(Added::NotApplicable)
            }
            Some(ItemDetails::Appointment(a)) => match event_start(msg, a) {
                Some(start) => (Component::Event(a, start), a.uid.as_deref()),
                None => return Ok(Added::NotApplicable),
            },
            Some(ItemDetails::Task(t)) => (Component::Todo(t), t.uid.as_deref()),
            _ => return Ok(Added::NotApplicable),
        };
        let uid = non_empty(uid).map_or_else(|| fallback_uid(source, msg), str::to_owned);
        if self.uids.contains(&uid) {
            return Ok(Added::Duplicate);
        }
        if self.uids.is_empty() {
            line(&mut self.w, "BEGIN:VCALENDAR")?;
            line(&mut self.w, "VERSION:2.0")?;
            line(&mut self.w, "PRODID:-//STViewer//STViewer//EN")?;
            line(&mut self.w, "CALSCALE:GREGORIAN")?;
        }
        match component {
            Component::Event(a, start) => write_event(&mut self.w, msg, a, &start, &uid, &self.stamp)?,
            Component::Todo(t) => write_todo(&mut self.w, msg, t, &uid, &self.stamp)?,
        }
        self.uids.insert(uid);
        Ok(Added::Written)
    }

    /// Ends the calendar, if anything was written, and returns the writer (not flushed).
    pub(crate) fn finish(mut self) -> io::Result<W> {
        if !self.uids.is_empty() {
            line(&mut self.w, "END:VCALENDAR")?;
        }
        Ok(self.w)
    }
}

/// Stable per message, so re-exports do not create duplicates on import.
fn fallback_uid(source: SourceId, msg: &Message) -> String {
    let key = match non_empty(msg.header.message_id.as_deref()) {
        Some(id) => id.to_owned(),
        None => format!("{}:{}", source.0, serde_json::to_string(&msg.header.locator).unwrap_or_default()),
    };
    format!("{:016x}@stviewer", fnv(key.as_bytes()))
}

fn common(w: &mut impl Write, msg: &Message, uid: &str, stamp: &str) -> io::Result<()> {
    line(w, &format!("UID:{}", escape(uid)))?;
    line(w, &format!("DTSTAMP:{stamp}"))?;
    line(w, &format!("SUMMARY:{}", escape(&msg.header.subject)))?;
    if let Some(d) = description(msg) {
        line(w, &format!("DESCRIPTION:{}", escape(&d)))?;
    }
    Ok(())
}

/// An iCalendar `RRULE` value as the source stored it, when it looks like one (Outlook stores
/// a prose description instead).
fn rrule(r: &str) -> Option<&str> {
    let r = r.trim();
    let r = r.get(..6).filter(|p| p.eq_ignore_ascii_case("RRULE:")).map_or(r, |_| &r[6..]);
    let charset = r.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '=' | ';' | ',' | '-' | '+'));
    (charset && r.to_ascii_uppercase().contains("FREQ=")).then_some(r)
}

/// DTSTART (required without a METHOD): the start, else the message date as in lists.
fn event_start(msg: &Message, a: &Appointment) -> Option<CalTime> {
    a.start.as_ref().and_then(CalTime::new).or_else(|| msg.header.date.and_then(|d| CalTime::new(&When::Time(d))))
}

fn write_event(
    w: &mut impl Write,
    msg: &Message,
    a: &Appointment,
    start: &CalTime,
    uid: &str,
    stamp: &str,
) -> io::Result<()> {
    // DTEND must have DTSTART's value type and come after it; otherwise the default duration
    // (one day for a date, none for a time) applies.
    let end = a.end.as_ref().and_then(CalTime::new).filter(|e| e.date == start.date && e.ms > start.ms);
    line(w, "BEGIN:VEVENT")?;
    common(w, msg, uid, stamp)?;
    line(w, &start.prop("DTSTART"))?;
    if let Some(end) = end {
        line(w, &end.prop("DTEND"))?;
    }
    if let Some(l) = non_empty(a.location.as_deref()) {
        line(w, &format!("LOCATION:{}", escape(l)))?;
    }
    if a.meeting == Some(MeetingKind::Cancellation) {
        line(w, "STATUS:CANCELLED")?;
    }
    if let Some(b) = a.busy_status {
        line(w, if b == BusyStatus::Free { "TRANSP:TRANSPARENT" } else { "TRANSP:OPAQUE" })?;
    }
    if let Some((cn, uri)) = a.organizer.as_ref().and_then(person) {
        line(w, &format!("ORGANIZER{cn}:{uri}"))?;
    }
    for (list, role) in [(&a.required, "REQ-PARTICIPANT"), (&a.optional, "OPT-PARTICIPANT")] {
        for (cn, uri) in list.iter().filter_map(person) {
            line(w, &format!("ATTENDEE;ROLE={role}{cn}:{uri}"))?;
        }
    }
    for (cn, uri) in a.resources.iter().filter_map(person) {
        line(w, &format!("ATTENDEE;CUTYPE=RESOURCE;ROLE=NON-PARTICIPANT{cn}:{uri}"))?;
    }
    if let Some(r) = non_empty(a.recurrence.as_deref()) {
        match rrule(r) {
            Some(rule) => line(w, &format!("RRULE:{rule}"))?,
            None => line(w, &format!("COMMENT:{}", escape(&format!("Recurrence: {r}"))))?,
        }
    }
    line(w, "END:VEVENT")
}

fn write_todo(w: &mut impl Write, msg: &Message, t: &Task, uid: &str, stamp: &str) -> io::Result<()> {
    let due = t.due.as_ref().and_then(CalTime::new);
    // DTSTART must have DUE's value type and not come after it.
    let start = t
        .start
        .as_ref()
        .and_then(CalTime::new)
        .filter(|s| due.as_ref().is_none_or(|d| d.date == s.date && d.ms >= s.ms));
    // COMPLETED must be a UTC date-time.
    let completed = t.completed.as_ref().and_then(CalTime::new).and_then(|c| c.to_time());
    line(w, "BEGIN:VTODO")?;
    common(w, msg, uid, stamp)?;
    for (name, v) in [("DTSTART", &start), ("DUE", &due), ("COMPLETED", &completed)] {
        if let Some(v) = v {
            line(w, &v.prop(name))?;
        }
    }
    if let Some(s) = t.status {
        let s = match s {
            TaskStatus::NotStarted | TaskStatus::Waiting => "NEEDS-ACTION",
            TaskStatus::InProgress => "IN-PROCESS",
            TaskStatus::Complete => "COMPLETED",
            TaskStatus::Deferred => "CANCELLED",
        };
        line(w, &format!("STATUS:{s}"))?;
    }
    if let Some(p) = t.percent_complete {
        line(w, &format!("PERCENT-COMPLETE:{}", p.min(100)))?;
    }
    line(w, "END:VTODO")
}

fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF2_9CE4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_model::{Body, Header, Labeled};

    fn msg(subject: &str, item: ItemDetails, body: &str) -> Message {
        Message {
            header: Header {
                subject: subject.into(),
                item: Some(item),
                message_id: Some(format!("{subject}@example.org")),
                ..Header::default()
            },
            body: Body { text: Some(body.into()), ..Body::default() },
            ..Message::default()
        }
    }

    /// Checks the physical form (CRLF only, folded at 75 octets, valid UTF-8, no control
    /// characters but HTAB, balanced BEGIN/END) and returns the unfolded content lines, each
    /// checked against `name *(";" param) ":" value`.
    fn lines(out: &[u8]) -> Vec<String> {
        let s = std::str::from_utf8(out).expect("UTF-8");
        assert!(s.ends_with("\r\n"), "ends with CRLF");
        let physical: Vec<&str> = s.strip_suffix("\r\n").unwrap().split("\r\n").collect();
        for l in &physical {
            assert!(l.len() <= 75, "folded at 75 octets: {l:?}");
            assert!(!l.chars().any(|c| c.is_control() && c != '\t'), "control character in {l:?}");
        }
        let unfolded: Vec<String> = s.replace("\r\n ", "").lines().map(str::to_owned).collect();
        let mut open = Vec::new();
        for l in &unfolded {
            let (head, _) = l.split_once(':').unwrap_or_else(|| panic!("no `:` in {l:?}"));
            let name = head.split(';').next().unwrap();
            assert!(!name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '-'), "name in {l:?}");
            if let Some(c) = l.strip_prefix("BEGIN:") {
                open.push(c.to_owned());
            } else if let Some(c) = l.strip_prefix("END:") {
                assert_eq!(open.pop().as_deref(), Some(c), "balanced");
            }
        }
        assert!(open.is_empty(), "unclosed {open:?}");
        unfolded
    }

    fn has(l: &[String], want: &[&str]) {
        for want in want {
            assert!(l.iter().any(|x| x == want), "missing {want:?} in {l:#?}");
        }
    }

    fn calendar(msgs: &[Message]) -> (Vec<String>, Vec<Added>) {
        let mut cal = Calendar::new(Vec::new(), 0);
        let added = msgs.iter().map(|m| cal.add(SourceId(1), m).unwrap()).collect();
        (lines(&cal.finish().unwrap()), added)
    }

    #[test]
    fn vcard_escapes_and_maps_fields() {
        let c = Contact {
            display_name: Some("Doe, Jane".into()),
            given_name: Some("Jane".into()),
            surname: Some("Doe".into()),
            company: Some("Example; Widgets\\Gadgets".into()),
            emails: vec![
                Address { name: None, email: Some("jane@example.org".into()) },
                // Exchange-only address and an injection attempt: left out.
                Address { name: None, email: Some("bad\r\nX-EVIL:1@example.org".into()) },
            ],
            phones: vec![Labeled { label: "mobile".into(), value: "+1 555 0100".into() }],
            addresses: vec![Labeled { label: "business".into(), value: "1 Example Way\r\nSpringfield".into() }],
            birthday: Some(When::Date("1970-05-07".into())),
            web_page: Some("https://example.org/a,b".into()),
            ..Contact::default()
        };
        let mut out = Vec::new();
        assert!(write_vcard(&mut out, &msg("Jane", ItemDetails::Contact(c), "Met at the fair")).unwrap());
        let l = lines(&out);
        has(
            &l,
            &[
                "BEGIN:VCARD",
                "VERSION:3.0",
                "FN:Doe\\, Jane",
                "N:Doe;Jane;;;",
                "ORG:Example\\; Widgets\\\\Gadgets",
                "EMAIL;TYPE=INTERNET:jane@example.org",
                "TEL;TYPE=CELL,VOICE:+1 555 0100",
                "ADR;TYPE=WORK:;;1 Example Way\\nSpringfield;;;;",
                "BDAY:1970-05-07",
                "URL:https://example.org/a,b",
                "NOTE:Met at the fair",
            ],
        );
        assert_eq!(l[1], "VERSION:3.0", "VERSION follows BEGIN");
        assert!(!l.iter().any(|x| x.contains("EVIL")), "{l:#?}");

        let mut none = Vec::new();
        assert!(!write_vcard(&mut none, &Message::default()).unwrap(), "mail is skipped");
        assert!(none.is_empty());
    }

    #[test]
    fn nothing_to_write_writes_nothing() {
        let mut cal = Calendar::new(Vec::new(), 0);
        assert_eq!(cal.add(SourceId(1), &Message::default()).unwrap(), Added::NotApplicable);
        assert!(cal.finish().unwrap().is_empty(), "no empty VCALENDAR");
        // A body of only control characters gives no (empty) NOTE.
        let mut out = Vec::new();
        write_vcard(&mut out, &msg("X", ItemDetails::Contact(Contact::default()), "\n\0 \u{1}")).unwrap();
        assert!(!lines(&out).iter().any(|l| l.starts_with("NOTE")), "{out:?}");
    }

    #[test]
    fn vcard_without_names_still_has_fn_and_department_only_org() {
        let c = Contact {
            department: Some("Research;".into()),
            emails: vec![Address { name: None, email: Some("x@example.org".into()) }],
            birthday: Some(When::Date("2001-02-30".into())),
            ..Contact::default()
        };
        let mut out = Vec::new();
        assert!(write_vcard(&mut out, &msg("", ItemDetails::Contact(c), "")).unwrap());
        let l = lines(&out);
        has(&l, &["FN:x@example.org", "N:;;;;", "ORG:;Research\\;"]);
        assert!(!l.iter().any(|x| x.starts_with("BDAY")), "invalid day left out");
    }

    #[test]
    fn ics_event_all_day_and_timed_with_attendees() {
        let timed = Appointment {
            start: Some(When::Time(983_196_000_000)), // 2001-02-26 14:00:00Z
            end: Some(When::Time(983_199_600_000)),
            location: Some("Room 3321, Springfield".into()),
            organizer: Some(Address { name: Some("Doe, Jane".into()), email: Some("jane@example.org".into()) }),
            required: vec![
                Address { name: Some("Sam".into()), email: Some("sam@example.org".into()) },
                Address { name: Some("No SMTP".into()), email: Some("/o=Example/cn=sam".into()) },
            ],
            resources: vec![Address { name: Some("Room 4C".into()), email: Some("room@example.org".into()) }],
            recurrence: Some("every Tuesday from 9:00 AM to 10:00 AM".into()),
            busy_status: Some(BusyStatus::Busy),
            ..Appointment::default()
        };
        let all_day = Appointment {
            start: Some(When::Date("2026-12-24".into())),
            end: Some(When::Date("2026-12-26".into())),
            all_day: true,
            recurrence: Some("FREQ=YEARLY;BYMONTH=12;BYMONTHDAY=24".into()),
            uid: Some("holiday-1@example.org".into()),
            busy_status: Some(BusyStatus::Free),
            ..Appointment::default()
        };
        let (l, added) = calendar(&[
            msg("Board", ItemDetails::Appointment(timed), &"long ".repeat(40)),
            msg("Holiday", ItemDetails::Appointment(all_day), ""),
            Message::default(),
        ]);
        assert_eq!(added, [Added::Written, Added::Written, Added::NotApplicable]);
        has(
            &l,
            &[
                "BEGIN:VCALENDAR",
                "VERSION:2.0",
                "PRODID:-//STViewer//STViewer//EN",
                "DTSTART:20010226T140000Z",
                "DTEND:20010226T150000Z",
                "LOCATION:Room 3321\\, Springfield",
                "TRANSP:OPAQUE",
                "ORGANIZER;CN=\"Doe, Jane\":mailto:jane@example.org",
                "ATTENDEE;ROLE=REQ-PARTICIPANT;CN=Sam:mailto:sam@example.org",
                "ATTENDEE;CUTYPE=RESOURCE;ROLE=NON-PARTICIPANT;CN=Room 4C:mailto:room@example.org",
                "COMMENT:Recurrence: every Tuesday from 9:00 AM to 10:00 AM",
                "UID:holiday-1@example.org",
                "DTSTART;VALUE=DATE:20261224",
                "DTEND;VALUE=DATE:20261226",
                "TRANSP:TRANSPARENT",
                "RRULE:FREQ=YEARLY;BYMONTH=12;BYMONTHDAY=24",
                "DTSTAMP:19700101T000000Z",
                "END:VCALENDAR",
            ],
        );
        assert_eq!(l.iter().filter(|x| *x == "BEGIN:VEVENT").count(), 2);
        assert_eq!(l.iter().filter(|x| x.starts_with("UID:")).count(), 2);
        assert_eq!(l.iter().filter(|x| x.starts_with("ATTENDEE")).count(), 2, "no mailto without an SMTP address");
        assert_eq!(l.iter().filter(|x| x.starts_with("RRULE")).count(), 1, "prose recurrence is not a rule");
    }

    #[test]
    fn ics_event_fixes_inconsistent_times_and_dedupes_uids() {
        let base = Appointment { uid: Some("u1@example.org".into()), ..Appointment::default() };
        let msgs = [
            // Date start with a time end, and a hostile RRULE: no DTEND, no RRULE.
            msg(
                "Mixed",
                ItemDetails::Appointment(Appointment {
                    start: Some(When::Date("2024-03-01".into())),
                    end: Some(When::Time(1_709_300_000_000)),
                    recurrence: Some("FREQ=DAILY\r\nATTACH:http://x.example".into()),
                    meeting: Some(MeetingKind::Cancellation),
                    ..base.clone()
                }),
                "",
            ),
            // Same UID (e.g. the meeting request of that entry): a duplicate.
            msg(
                "Mixed again",
                ItemDetails::Appointment(Appointment { start: Some(When::Time(0)), ..base.clone() }),
                "",
            ),
            // No start at all and no message date: skipped.
            msg("Nowhere", ItemDetails::Appointment(Appointment::default()), ""),
            // End before start: dropped; start from the message date.
            {
                let mut m = msg(
                    "Backwards",
                    ItemDetails::Appointment(Appointment { end: Some(When::Time(0)), ..Appointment::default() }),
                    "",
                );
                m.header.date = Some(86_400_000);
                m
            },
        ];
        let (l, added) = calendar(&msgs);
        assert_eq!(added, [Added::Written, Added::Duplicate, Added::NotApplicable, Added::Written]);
        let counter = Appointment { start: Some(When::Time(0)), meeting: Some(MeetingKind::Counter), ..base };
        let mut cal = Calendar::new(Vec::new(), 0);
        let proposal = msg("Proposal", ItemDetails::Appointment(counter), "");
        assert_eq!(cal.add(SourceId(1), &proposal).unwrap(), Added::NotApplicable, "a counter proposal is no event");
        assert!(cal.finish().unwrap().is_empty(), "nothing written");
        has(&l, &["DTSTART;VALUE=DATE:20240301", "STATUS:CANCELLED", "DTSTART:19700102T000000Z"]);
        assert!(
            !l.iter().any(|x| x.starts_with("DTEND") || x.starts_with("RRULE") || x.starts_with("ATTACH")),
            "{l:#?}"
        );
        let comment = l.iter().find(|x| x.starts_with("COMMENT:")).unwrap();
        assert_eq!(comment, "COMMENT:Recurrence: FREQ=DAILY\\nATTACH:http://x.example");
    }

    #[test]
    fn ics_todo_status_and_dates() {
        let t = Task {
            status: Some(TaskStatus::Complete),
            percent_complete: Some(100),
            start: Some(When::Date("2001-07-05".into())),
            due: Some(When::Date("2001-07-02".into())),
            completed: Some(When::Date("2001-07-03".into())),
            ..Task::default()
        };
        let (l, added) = calendar(&[msg("File brief", ItemDetails::Task(t), "")]);
        assert_eq!(added, [Added::Written]);
        has(
            &l,
            &[
                "BEGIN:VTODO",
                "DUE;VALUE=DATE:20010702",
                "COMPLETED:20010703T000000Z",
                "STATUS:COMPLETED",
                "PERCENT-COMPLETE:100",
            ],
        );
        assert!(!l.iter().any(|x| x.starts_with("DTSTART")), "start after due is left out");
    }

    #[test]
    fn folding_keeps_multibyte_characters_whole() {
        let mut out = Vec::new();
        line(&mut out, &format!("SUMMARY:{}", "é".repeat(100))).unwrap();
        lines(&out);
        let s = String::from_utf8(out).expect("valid UTF-8 after folding");
        assert_eq!(s.replace("\r\n ", "").trim_end(), format!("SUMMARY:{}", "é".repeat(100)));
    }
}
