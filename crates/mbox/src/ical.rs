//! Minimal iCalendar (RFC 5545) reader for invites carried in MIME mail: the master VEVENT
//! (else the first VEVENT, else the first VTODO) of a `text/calendar` part becomes
//! [`ItemDetails`].
//!
//! Local times with a `TZID` are resolved with the `VTIMEZONE` in the same object (Outlook,
//! Exchange and Google always include one): each STANDARD/DAYLIGHT observance's onsets are
//! computed from its `DTSTART`, `RDATE`s and yearly `RRULE` (`BYMONTH` + `BYDAY`, e.g. `2SU`,
//! `-1SU`, bounded by `UNTIL`). A `TZID` without a usable `VTIMEZONE` (some generators send
//! the name only) is looked up as an IANA name, Windows zone id or Outlook display name (see
//! [`crate::tzid`]). Floating times (no `TZID`, no `Z`) and unknown zones are taken as UTC:
//! the viewer has no reliable "local" zone for the archive's owner.

use std::borrow::Cow;

use mail_model::time::{civil_from_days, days_from_civil, MS_PER_DAY};
use mail_model::{Address, Appointment, BusyStatus, ItemDetails, MeetingKind, Task, TaskStatus, When};

/// Larger calendar parts are ignored (an invite is a few KB; this bounds work on hostile input).
pub const MAX_ICAL_BYTES: usize = 1 << 20;

/// Deepest component nesting kept (root > VCALENDAR > VTIMEZONE > STANDARD needs 4); deeper
/// components are skipped, which bounds the (recursive) drop of the tree on hostile input.
const MAX_DEPTH: usize = 8;

/// One content line: `NAME;PARAM=V;PARAM="V":value`, borrowed from the unfolded text.
#[derive(Debug, Clone, Copy)]
struct Line<'a> {
    name: &'a str,
    /// Everything between the name and the value separator, without the leading `;`.
    params: &'a str,
    value: &'a str,
}

impl<'a> Line<'a> {
    /// First value of parameter `key`, unquoted (RFC 5545 3.2; quoted values may contain `:;,`).
    fn param(&self, key: &str) -> Option<&'a str> {
        let mut rest = self.params;
        while !rest.is_empty() {
            let end = unquoted_find(rest, b';').unwrap_or(rest.len());
            let (seg, next) = (&rest[..end], rest.get(end + 1..).unwrap_or(""));
            rest = next;
            let Some((k, v)) = seg.split_once('=') else { continue };
            if !k.trim().eq_ignore_ascii_case(key) {
                continue;
            }
            return Some(match v.strip_prefix('"') {
                Some(q) => &q[..q.find('"').unwrap_or(q.len())],
                None => &v[..unquoted_find(v, b',').unwrap_or(v.len())],
            });
        }
        None
    }
}

/// Byte offset of the first ASCII `needle` outside double quotes.
fn unquoted_find(s: &str, needle: u8) -> Option<usize> {
    let mut in_quotes = false;
    s.bytes().position(|b| {
        if b == b'"' {
            in_quotes = !in_quotes;
        }
        b == needle && !in_quotes
    })
}

/// A component with its own properties and children, e.g. VEVENT or VTIMEZONE.
#[derive(Debug, Default)]
struct Component<'a> {
    name: &'a str,
    lines: Vec<Line<'a>>,
    children: Vec<Component<'a>>,
}

impl<'a> Component<'a> {
    fn get(&self, name: &str) -> Option<&Line<'a>> {
        self.lines.iter().find(|l| l.name.eq_ignore_ascii_case(name))
    }

    fn all<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s Line<'a>> + 's {
        self.lines.iter().filter(move |l| l.name.eq_ignore_ascii_case(name))
    }

    fn children<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s Component<'a>> + 's {
        self.children.iter().filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(|l| unescape(l.value)).filter(|s| !s.trim().is_empty())
    }
}

/// RFC 5545 3.1: drops CRs before LFs and removes every line break followed by a space or tab.
/// Works on bytes, so a fold inside a multi-byte UTF-8 sequence (which some writers produce
/// when cutting at 75 octets) rejoins before decoding.
fn unfold(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while let Some(&b) = src.get(i) {
        match b {
            b'\r' if src.get(i + 1) == Some(&b'\n') => i += 1,
            b'\n' if matches!(src.get(i + 1), Some(b' ' | b'\t')) => i += 2,
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Splits a content line at the first `:` outside a quoted parameter value.
fn parse_line(s: &str) -> Option<Line<'_>> {
    let colon = unquoted_find(s, b':')?;
    let head = &s[..colon];
    let (name, params) = head.split_once(';').unwrap_or((head, ""));
    let name = name.trim();
    (!name.is_empty()).then_some(Line { name, params, value: &s[colon + 1..] })
}

/// Builds the component tree from unfolded text. `END:X` closes the innermost open `X`
/// (and anything left open inside it); an `END` matching nothing open is ignored.
fn parse(text: &str) -> Component<'_> {
    let mut stack = vec![Component { name: "ROOT", ..Component::default() }];
    // Open components below MAX_DEPTH whose content is being skipped.
    let mut skipped = 0usize;
    for line in text.split('\n').filter_map(parse_line) {
        let is = |n: &str| line.name.eq_ignore_ascii_case(n);
        if is("BEGIN") {
            if skipped > 0 || stack.len() >= MAX_DEPTH {
                skipped += 1;
            } else {
                stack.push(Component { name: line.value.trim(), ..Component::default() });
            }
        } else if is("END") {
            if skipped > 0 {
                skipped -= 1;
                continue;
            }
            let name = line.value.trim();
            if let Some(pos) = stack.iter().skip(1).rposition(|c| c.name.eq_ignore_ascii_case(name)) {
                close_to(&mut stack, pos + 1);
            }
        } else if skipped == 0 {
            stack.last_mut().expect("root").lines.push(line);
        }
    }
    close_to(&mut stack, 1);
    stack.pop().expect("root")
}

/// Pops components until `len` remain, attaching each to its parent.
fn close_to<'a>(stack: &mut Vec<Component<'a>>, len: usize) {
    while stack.len() > len.max(1) {
        let done = stack.pop().expect("len > 1");
        stack.last_mut().expect("root").children.push(done);
    }
}

/// RFC 5545 3.3.11 TEXT escapes.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// RFC 6868 parameter value escapes: `^n` newline, `^'` double quote, `^^` caret.
fn unescape_param(s: &str) -> Cow<'_, str> {
    if !s.contains('^') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        let decoded = match (c, it.peek()) {
            ('^', Some('n' | 'N')) => '\n',
            ('^', Some('\'')) => '"',
            ('^', Some('^')) => '^',
            _ => {
                out.push(c);
                continue;
            }
        };
        it.next();
        out.push(decoded);
    }
    Cow::Owned(out)
}

/// Wall-clock date and time, before any zone is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Local {
    /// Days since 1970-01-01.
    days: i64,
    /// Seconds into the day.
    secs: i64,
}

impl Local {
    fn ms_as_utc(self) -> i64 {
        self.days * MS_PER_DAY + self.secs * 1000
    }

    fn minus_secs(self, secs: i64) -> Local {
        let t = self.days * 86_400 + self.secs - secs;
        Local { days: t.div_euclid(86_400), secs: t.rem_euclid(86_400) }
    }
}

enum Stamp {
    Date(i64),
    Utc(Local),
    Local(Local),
}

impl Stamp {
    /// The wall-clock (or UTC) value; a date is its midnight.
    fn local(&self) -> Local {
        match *self {
            Stamp::Date(days) => Local { days, secs: 0 },
            Stamp::Utc(l) | Stamp::Local(l) => l,
        }
    }
}

/// Splits `s` into fixed-width runs of ASCII digits (`None` unless every byte is a digit and
/// the widths add up to its length).
fn fields<const N: usize>(s: &str, widths: [usize; N]) -> Option<[i64; N]> {
    let b = s.as_bytes();
    if b.len() != widths.iter().sum::<usize>() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut out = [0i64; N];
    let mut at = 0;
    for (o, w) in out.iter_mut().zip(widths) {
        *o = b[at..at + w].iter().fold(0, |n, d| n * 10 + i64::from(d - b'0'));
        at += w;
    }
    Some(out)
}

/// `YYYYMMDD`, `YYYYMMDDTHHMMSS` or `YYYYMMDDTHHMMSSZ`.
fn parse_stamp(value: &str) -> Option<Stamp> {
    let v = value.trim();
    let (date, time) = v.split_once(['T', 't']).map_or((v, None), |(d, t)| (d, Some(t)));
    let [y, m, d] = fields(date, [4, 2, 2])?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m as u32, d as u32);
    let Some(time) = time else { return Some(Stamp::Date(days)) };
    let (time, utc) = time.strip_suffix(['Z', 'z']).map_or((time, false), |t| (t, true));
    let [h, mi, s] = fields(time, [2, 2, 2])?;
    if h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let local = Local { days, secs: h * 3600 + mi * 60 + s };
    Some(if utc { Stamp::Utc(local) } else { Stamp::Local(local) })
}

/// `+HHMM` / `-HHMMSS` → seconds east of UTC.
fn parse_offset(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, rest) = match v.as_bytes().first()? {
        b'+' => (1, &v[1..]),
        b'-' => (-1, &v[1..]),
        _ => (1, v),
    };
    let [h, m, s] = fields(rest, [2, 2, 2]).or_else(|| fields(rest, [2, 2]).map(|[h, m]| [h, m, 0]))?;
    (h < 24 && m < 60 && s < 60).then_some(sign * (h * 3600 + m * 60 + s))
}

/// Day of week, 0 = Sunday.
fn weekday(days: i64) -> i64 {
    (days + 4).rem_euclid(7)
}

/// The `n`th (`-1` = last; `n` in ±1..=5) weekday `dow` of month `m` in year `y`, as days
/// since epoch.
fn nth_weekday(y: i64, m: u32, n: i64, dow: i64) -> Option<i64> {
    let first = days_from_civil(y, m, 1);
    let next = if m == 12 { days_from_civil(y + 1, 1, 1) } else { days_from_civil(y, m + 1, 1) };
    if n > 0 {
        let d = first + (dow - weekday(first)).rem_euclid(7) + (n - 1) * 7;
        (d < next).then_some(d)
    } else {
        let last = next - 1;
        let d = last - (weekday(last) - dow).rem_euclid(7) + (n + 1) * 7;
        (d >= first).then_some(d)
    }
}

fn rrule_part<'a>(rrule: &'a str, key: &str) -> Option<&'a str> {
    rrule.split(';').filter_map(|p| p.split_once('=')).find(|(k, _)| k.trim().eq_ignore_ascii_case(key)).map(|(_, v)| v)
}

/// A yearly transition rule: `FREQ=YEARLY;BYMONTH=m;BYDAY=nDD[;UNTIL=…]`.
struct Yearly {
    month: u32,
    /// `-1` = last, else 1–5.
    nth: i64,
    /// 0 = Sunday.
    dow: i64,
    until: Option<Local>,
}

impl Yearly {
    fn parse(r: &str) -> Option<Yearly> {
        if !rrule_part(r, "FREQ")?.trim().eq_ignore_ascii_case("YEARLY") {
            return None;
        }
        let month: u32 = rrule_part(r, "BYMONTH")?.trim().parse().ok().filter(|m| (1..=12).contains(m))?;
        let byday = rrule_part(r, "BYDAY")?.trim();
        let split = byday.find(|c: char| c.is_ascii_alphabetic())?;
        let nth: i64 = if split == 0 { 1 } else { byday[..split].parse().ok()? };
        if nth == 0 || !(-5..=5).contains(&nth) {
            return None;
        }
        let dow = ["SU", "MO", "TU", "WE", "TH", "FR", "SA"]
            .iter()
            .position(|d| byday[split..].eq_ignore_ascii_case(d))? as i64;
        let until = rrule_part(r, "UNTIL").and_then(parse_stamp).map(|s| s.local());
        Some(Yearly { month, nth, dow, until })
    }
}

/// One STANDARD or DAYLIGHT block of a VTIMEZONE.
struct Observance<'a> {
    start: Local,
    offset_to: i64,
    /// How far clocks spring forward at this onset (0 when they fall back).
    gap: i64,
    yearly: Option<Yearly>,
    rdates: Vec<&'a str>,
}

impl<'a> Observance<'a> {
    fn from(c: &Component<'a>) -> Option<Observance<'a>> {
        let offset_to = parse_offset(c.get("TZOFFSETTO")?.value)?;
        let offset_from = c.get("TZOFFSETFROM").and_then(|l| parse_offset(l.value)).unwrap_or(offset_to);
        Some(Observance {
            start: parse_stamp(c.get("DTSTART")?.value)?.local(),
            offset_to,
            gap: (offset_to - offset_from).max(0),
            yearly: c.get("RRULE").and_then(|r| Yearly::parse(r.value)),
            rdates: c.all("RDATE").map(|l| l.value).collect(),
        })
    }

    /// Latest onset at or before wall-clock time `t` (onsets are in the wall-clock time
    /// before the change).
    fn last_onset(&self, t: Local) -> Option<Local> {
        let rdates = self.rdates.iter().flat_map(|v| v.split(',')).filter_map(parse_stamp).map(|s| s.local());
        let fixed = std::iter::once(self.start).chain(rdates).filter(|on| *on <= t).max();
        let Some(y) = &self.yearly else { return fixed };
        let (year, _, _) = civil_from_days(t.days);
        let ruled = [year, year - 1]
            .into_iter()
            .filter_map(|yr| nth_weekday(yr, y.month, y.nth, y.dow).map(|days| Local { days, secs: self.start.secs }))
            .find(|on| *on <= t && *on >= self.start && y.until.is_none_or(|u| *on <= u));
        fixed.max(ruled)
    }
}

/// UTC offset (seconds) of wall-clock time `t` in the VTIMEZONE of `cal` whose TZID is `tzid`.
/// A time skipped when clocks spring forward takes the offset before the change, a repeated
/// one its first occurrence (RFC 5545 3.3.5).
fn zone_offset(cal: &Component<'_>, tzid: &str, t: Local) -> Option<i64> {
    let tz = cal.children("VTIMEZONE").find(|c| c.get("TZID").is_some_and(|l| l.value.trim() == tzid.trim()))?;
    tz.children
        .iter()
        .filter(|c| c.name.eq_ignore_ascii_case("STANDARD") || c.name.eq_ignore_ascii_case("DAYLIGHT"))
        .filter_map(Observance::from)
        .filter_map(|o| o.last_onset(t.minus_secs(o.gap)).map(|on| (on, o.offset_to)))
        .max_by_key(|(on, _)| *on)
        .map(|(_, off)| off)
}

/// DTSTART/DTEND/… → [`When`], plus whether it was a DATE value.
fn when(cal: &Component<'_>, line: &Line<'_>) -> Option<(When, bool)> {
    let tzid = line.param("TZID").map(Cow::Borrowed);
    let (stamp, tzid) = match parse_stamp(line.value) {
        Some(s) => (s, tzid),
        // An unquoted TZID with a colon (`TZID=(UTC+01:00) Amsterdam:2026…`, which RFC 5545
        // forbids but some writers emit) split the line early: a DATE-TIME never contains
        // one, so the value is what follows the last colon.
        None => {
            let (extra, value) = line.value.rsplit_once(':')?;
            let tail = extra.split(';').next().unwrap_or(extra);
            (parse_stamp(value)?, tzid.map(|t| Cow::Owned(format!("{t}:{tail}"))))
        }
    };
    Some(match stamp {
        Stamp::Date(days) => (When::Date(date_string(days)), true),
        Stamp::Utc(l) => (When::Time(l.ms_as_utc()), false),
        Stamp::Local(l) => {
            let off = tzid
                .and_then(|tz| zone_offset(cal, &tz, l).or_else(|| crate::tzid::offset(&tz, l.days, l.secs)))
                .unwrap_or(0);
            (When::Time(l.ms_as_utc() - off * 1000), false)
        }
    })
}

fn date_string(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `P1D`, `PT1H30M`, `P2W`, `-PT15M` → milliseconds (`None` when malformed or overflowing).
fn duration_ms(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, v) = match v.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix(['P', 'p'])?;
    let (mut total, mut num, mut in_time) = (0i64, None::<i64>, false);
    for c in v.chars() {
        match c.to_ascii_uppercase() {
            'T' if num.is_none() => in_time = true,
            d @ '0'..='9' => num = Some(num.unwrap_or(0).checked_mul(10)?.checked_add(i64::from(d as u8 - b'0'))?),
            unit => {
                let secs = match (unit, in_time) {
                    ('W', false) => 604_800,
                    ('D', false) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.checked_add(num.take()?.checked_mul(secs)?)?;
            }
        }
    }
    if num.is_some() {
        return None;
    }
    total.checked_mul(sign * 1000)
}

/// `CN=Name:mailto:a@b` → address.
fn address(line: &Line<'_>) -> Option<Address> {
    let v = line.value.trim();
    let email = v.get(..7).filter(|p| p.eq_ignore_ascii_case("mailto:")).map_or(v, |_| &v[7..]).trim();
    let name = line.param("CN").map(|n| unescape_param(n.trim()).into_owned()).filter(|n| !n.is_empty());
    let email = (!email.is_empty() && email.contains('@')).then(|| email.to_owned());
    (name.is_some() || email.is_some()).then_some(Address { name, email })
}

fn same_person(a: &Address, b: &Address) -> bool {
    matches!((&a.email, &b.email), (Some(x), Some(y)) if x.eq_ignore_ascii_case(y))
}

fn upper(line: Option<&Line<'_>>) -> Option<String> {
    line.map(|l| l.value.trim().to_ascii_uppercase())
}

fn appointment(cal: &Component<'_>, ev: &Component<'_>, method: Option<&str>) -> Appointment {
    let start = ev.get("DTSTART").and_then(|l| when(cal, l));
    let all_day = start.as_ref().is_some_and(|(_, date)| *date);
    let start = start.map(|(w, _)| w);
    let end = ev
        .get("DTEND")
        .and_then(|l| when(cal, l))
        .map(|(w, _)| w)
        .or_else(|| {
            // RFC 5545 3.6.1: a DATE start without DTEND/DURATION lasts one day.
            let dur = match ev.get("DURATION") {
                Some(l) => duration_ms(l.value)?,
                None if all_day => MS_PER_DAY,
                None => return None,
            };
            Some(match start.as_ref()? {
                When::Time(ms) => When::Time(ms.checked_add(dur)?),
                date @ When::Date(_) => When::Date(date_string(date.to_ms()? / MS_PER_DAY + dur / MS_PER_DAY)),
            })
        })
        // An end before the start (bad DTEND, negative DURATION) is noise, not a time range.
        .filter(|e| !matches!((e.to_ms(), start.as_ref().and_then(When::to_ms)), (Some(e), Some(s)) if e < s));
    let organizer = ev.get("ORGANIZER").and_then(address);
    let mut a = Appointment {
        start,
        end,
        all_day,
        location: ev.text("LOCATION"),
        organizer: organizer.clone(),
        recurrence: ev.get("RRULE").map(|l| l.value.trim().to_owned()).filter(|s| !s.is_empty()),
        uid: ev.text("UID"),
        ..Appointment::default()
    };
    a.recurring = a.recurrence.is_some();
    let mut replier = None;
    for line in ev.all("ATTENDEE") {
        let Some(addr) = address(line) else { continue };
        if organizer.as_ref().is_some_and(|o| same_person(o, &addr)) {
            continue;
        }
        if replier.is_none() {
            replier = line.param("PARTSTAT").map(str::to_ascii_uppercase);
        }
        let cutype = line.param("CUTYPE").unwrap_or("");
        let role = line.param("ROLE").unwrap_or("REQ-PARTICIPANT");
        let is = |v: &str, n: &str| v.trim().eq_ignore_ascii_case(n);
        if is(cutype, "RESOURCE") || is(cutype, "ROOM") || is(role, "NON-PARTICIPANT") {
            a.resources.push(addr);
        } else if is(role, "OPT-PARTICIPANT") {
            a.optional.push(addr);
        } else {
            a.required.push(addr);
        }
    }
    a.busy_status = upper(ev.get("X-MICROSOFT-CDO-BUSYSTATUS"))
        .and_then(|v| match v.as_str() {
            "FREE" => Some(BusyStatus::Free),
            "TENTATIVE" => Some(BusyStatus::Tentative),
            "BUSY" => Some(BusyStatus::Busy),
            "OOF" => Some(BusyStatus::OutOfOffice),
            "WORKINGELSEWHERE" => Some(BusyStatus::WorkingElsewhere),
            _ => None,
        })
        .or_else(|| match upper(ev.get("TRANSP"))?.as_str() {
            "TRANSPARENT" => Some(BusyStatus::Free),
            "OPAQUE" => Some(BusyStatus::Busy),
            _ => None,
        });
    a.meeting = match method.map(str::to_ascii_uppercase).as_deref() {
        Some("REQUEST") => Some(MeetingKind::Request),
        Some("CANCEL") => Some(MeetingKind::Cancellation),
        Some("COUNTER") => Some(MeetingKind::Counter),
        // A reply carries the responding attendee's PARTSTAT.
        Some("REPLY") => match replier.as_deref() {
            Some("ACCEPTED") => Some(MeetingKind::Accepted),
            Some("TENTATIVE") => Some(MeetingKind::Tentative),
            Some("DECLINED") => Some(MeetingKind::Declined),
            _ => None,
        },
        _ => None,
    };
    a
}

fn task(todo: &Component<'_>) -> Task {
    // Task dates are calendar days in every client UI: keep the written (wall-clock) day of a
    // local time, the UTC day of a UTC time.
    let day =
        |name: &str| todo.get(name).and_then(|l| parse_stamp(l.value)).map(|s| When::Date(date_string(s.local().days)));
    Task {
        status: upper(todo.get("STATUS")).and_then(|v| match v.as_str() {
            "NEEDS-ACTION" => Some(TaskStatus::NotStarted),
            "IN-PROCESS" => Some(TaskStatus::InProgress),
            "COMPLETED" => Some(TaskStatus::Complete),
            "CANCELLED" => Some(TaskStatus::Deferred),
            _ => None,
        }),
        percent_complete: todo
            .get("PERCENT-COMPLETE")
            .and_then(|l| l.value.trim().parse::<u8>().ok())
            .map(|p| p.min(100)),
        start: day("DTSTART"),
        due: day("DUE"),
        completed: day("COMPLETED"),
        owner: todo.get("ORGANIZER").and_then(address).and_then(|a| a.name.or(a.email)),
        uid: todo.text("UID"),
    }
}

/// Item details from an iCalendar object: its master VEVENT (the one without a
/// `RECURRENCE-ID`), else its first VEVENT, else its first VTODO.
pub fn item_from_ical(bytes: &[u8]) -> Option<ItemDetails> {
    if bytes.len() > MAX_ICAL_BYTES {
        return None;
    }
    let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
    let unfolded = unfold(bytes);
    let text = String::from_utf8_lossy(&unfolded);
    let root = parse(&text);
    let cal = root.children("VCALENDAR").next().unwrap_or(&root);
    let method = cal.get("METHOD").map(|l| l.value.trim());
    let master = cal.children("VEVENT").find(|e| e.get("RECURRENCE-ID").is_none());
    if let Some(ev) = master.or_else(|| cal.children("VEVENT").next()) {
        return Some(ItemDetails::Appointment(appointment(cal, ev, method)));
    }
    let todo = cal.children("VTODO").next()?;
    Some(ItemDetails::Task(task(todo)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appt(src: &str) -> Appointment {
        match item_from_ical(src.as_bytes()) {
            Some(ItemDetails::Appointment(a)) => a,
            other => panic!("not an appointment: {other:?}"),
        }
    }

    fn ms(y: i64, m: u32, d: u32, h: i64, mi: i64) -> i64 {
        (days_from_civil(y, m, d) * 86_400 + h * 3600 + mi * 60) * 1000
    }

    /// Outlook's form: one STANDARD and one DAYLIGHT rule from 1601.
    const PACIFIC: &str =
        "BEGIN:VTIMEZONE\r\nTZID:Pacific Standard Time\r\nBEGIN:STANDARD\r\nDTSTART:16010101T020000\r\n\
TZOFFSETFROM:-0700\r\nTZOFFSETTO:-0800\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\nEND:STANDARD\r\n\
BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0800\r\nTZOFFSETTO:-0700\r\n\
RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";

    #[test]
    fn outlook_request_resolves_vtimezone_in_summer_and_winter() {
        let src = format!(
            "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\n{PACIFIC}BEGIN:VEVENT\r\n\
ORGANIZER;CN=\"Doe, Alex\":mailto:alex@example.org\r\n\
ATTENDEE;ROLE=REQ-PARTICIPANT;CN=Sam:mailto:sam@example.org\r\n\
ATTENDEE;ROLE=OPT-PARTICIPANT;CN=Kai:mailto:kai@example.org\r\n\
ATTENDEE;CUTYPE=RESOURCE;ROLE=NON-PARTICIPANT;CN=Room 4C:mailto:room4c@example.org\r\n\
DTSTART;TZID=Pacific Standard Time:20010612T090000\r\nDTEND;TZID=\"Pacific Standard Time\":20010612T103000\r\n\
LOCATION:Room 101\\, Building A\r\nUID:040000008200E00074C5B7101A82E008\r\nX-MICROSOFT-CDO-BUSYSTATUS:BUSY\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let a = appt(&src);
        // June: daylight time, UTC-7.
        assert_eq!(a.start, Some(When::Time(ms(2001, 6, 12, 16, 0))));
        assert_eq!(a.end, Some(When::Time(ms(2001, 6, 12, 17, 30))));
        assert_eq!(a.location.as_deref(), Some("Room 101, Building A"));
        assert_eq!(a.organizer.as_ref().and_then(|o| o.name.as_deref()), Some("Doe, Alex"));
        assert_eq!((a.required.len(), a.optional.len(), a.resources.len()), (1, 1, 1));
        assert_eq!(a.meeting, Some(MeetingKind::Request));
        assert_eq!(a.busy_status, Some(BusyStatus::Busy));
        assert!(!a.all_day);

        // January: standard time, UTC-8.
        let winter = src.replace("20010612T090000", "20010115T090000");
        assert_eq!(appt(&winter).start, Some(When::Time(ms(2001, 1, 15, 17, 0))));
    }

    #[test]
    fn skipped_and_repeated_local_times_follow_rfc_5545() {
        let at = |local: &str| {
            appt(&format!(
                "BEGIN:VCALENDAR\n{PACIFIC}BEGIN:VEVENT\nDTSTART;TZID=Pacific Standard Time:{local}\nEND:VEVENT\n\
END:VCALENDAR\n"
            ))
            .start
        };
        // 02:30 does not exist on 2026-03-08: the offset before the gap (-8) applies.
        assert_eq!(at("20260308T023000"), Some(When::Time(ms(2026, 3, 8, 10, 30))));
        assert_eq!(at("20260308T030000"), Some(When::Time(ms(2026, 3, 8, 10, 0))));
        // 01:30 happens twice on 2026-11-01: the first (daylight, -7) is meant.
        assert_eq!(at("20261101T013000"), Some(When::Time(ms(2026, 11, 1, 8, 30))));
        assert_eq!(at("20261101T020000"), Some(When::Time(ms(2026, 11, 1, 10, 0))));
    }

    #[test]
    fn an_end_before_the_start_is_dropped() {
        let a = appt("BEGIN:VEVENT\nDTSTART:20260601T100000Z\nDTEND:20260601T090000Z\nEND:VEVENT\n");
        assert_eq!((a.start.is_some(), a.end), (true, None));
        let a = appt("BEGIN:VEVENT\nDTSTART:20260601T100000Z\nDURATION:-PT1H\nEND:VEVENT\n");
        assert_eq!(a.end, None);
        let a = appt("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20260601\nDTEND;VALUE=DATE:20260531\nEND:VEVENT\n");
        assert_eq!(a.end, None);
    }

    /// The IANA form Google/Apple write: one observance per historical rule, ended by `UNTIL`.
    const LOS_ANGELES: &str = "BEGIN:VTIMEZONE\nTZID:America/Los_Angeles\n\
BEGIN:DAYLIGHT\nDTSTART:19870405T020000\nTZOFFSETFROM:-0800\nTZOFFSETTO:-0700\n\
RRULE:FREQ=YEARLY;UNTIL=20060402T100000Z;BYMONTH=4;BYDAY=1SU\nEND:DAYLIGHT\n\
BEGIN:STANDARD\nDTSTART:19671029T020000\nTZOFFSETFROM:-0700\nTZOFFSETTO:-0800\n\
RRULE:FREQ=YEARLY;UNTIL=20061029T090000Z;BYMONTH=10;BYDAY=-1SU\nEND:STANDARD\n\
BEGIN:DAYLIGHT\nDTSTART:20070311T020000\nTZOFFSETFROM:-0800\nTZOFFSETTO:-0700\n\
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=2SU\nEND:DAYLIGHT\n\
BEGIN:STANDARD\nDTSTART:20071104T020000\nTZOFFSETFROM:-0700\nTZOFFSETTO:-0800\n\
RRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\nEND:STANDARD\nEND:VTIMEZONE\n";

    #[test]
    fn ended_observance_rules_stop_at_until() {
        let at = |local: &str| {
            appt(&format!(
                "BEGIN:VCALENDAR\n{LOS_ANGELES}BEGIN:VEVENT\nDTSTART;TZID=America/Los_Angeles:{local}\n\
END:VEVENT\nEND:VCALENDAR\n"
            ))
            .start
        };
        // Between the old rule's last Sunday of October and the current first Sunday of November.
        assert_eq!(at("20241101T100000"), Some(When::Time(ms(2024, 11, 1, 17, 0))), "still daylight");
        assert_eq!(at("20241105T100000"), Some(When::Time(ms(2024, 11, 5, 18, 0))));
        // Under the old rules.
        assert_eq!(at("20051031T090000"), Some(When::Time(ms(2005, 10, 31, 17, 0))));
        assert_eq!(at("20050404T090000"), Some(When::Time(ms(2005, 4, 4, 16, 0))));
        assert_eq!(at("20050320T090000"), Some(When::Time(ms(2005, 3, 20, 17, 0))));
    }

    #[test]
    fn rdate_observances_apply_from_each_listed_onset() {
        let a = appt(
            "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:Europe/Example\n\
BEGIN:DAYLIGHT\nDTSTART:20230326T020000\nRDATE:20240331T020000,20250330T020000\nTZOFFSETFROM:+0100\n\
TZOFFSETTO:+0200\nEND:DAYLIGHT\nBEGIN:STANDARD\nDTSTART:20231029T030000\nRDATE:20241027T030000\n\
TZOFFSETFROM:+0200\nTZOFFSETTO:+0100\nEND:STANDARD\nEND:VTIMEZONE\n\
BEGIN:VEVENT\nDTSTART;TZID=Europe/Example:20240601T120000\nEND:VEVENT\nEND:VCALENDAR\n",
        );
        assert_eq!(a.start, Some(When::Time(ms(2024, 6, 1, 10, 0))));
    }

    #[test]
    fn tzid_without_vtimezone_uses_the_time_zone_database() {
        let at =
            |tzid: &str, local: &str| appt(&format!("BEGIN:VEVENT\nDTSTART;TZID={tzid}:{local}\nEND:VEVENT\n")).start;
        // IANA name only (Zoom and many web generators): EDT in July, EST in January.
        assert_eq!(at("America/New_York", "20240701T090000"), Some(When::Time(ms(2024, 7, 1, 13, 0))));
        assert_eq!(at("America/New_York", "20240115T090000"), Some(When::Time(ms(2024, 1, 15, 14, 0))));
        // Windows id and Outlook display name.
        assert_eq!(at("\"W. Europe Standard Time\"", "20240701T090000"), Some(When::Time(ms(2024, 7, 1, 7, 0))));
        let display = "\"(UTC-08:00) Pacific Time (US & Canada)\"";
        assert_eq!(at(display, "20240115T090000"), Some(When::Time(ms(2024, 1, 15, 17, 0))));
        // Unquoted, as some writers emit it despite the colon.
        assert_eq!(at(display.trim_matches('"'), "20240115T090000"), Some(When::Time(ms(2024, 1, 15, 17, 0))));
        // An unknown zone falls back to UTC.
        assert_eq!(at("Nowhere/Example", "20240701T090000"), Some(When::Time(ms(2024, 7, 1, 9, 0))));

        // A VTIMEZONE in the object wins over the database, even for an IANA-looking TZID.
        let custom = appt(
            "BEGIN:VCALENDAR\nBEGIN:VTIMEZONE\nTZID:America/New_York\nBEGIN:STANDARD\nDTSTART:19700101T000000\n\
TZOFFSETTO:+0300\nEND:STANDARD\nEND:VTIMEZONE\nBEGIN:VEVENT\nDTSTART;TZID=America/New_York:20240701T090000\n\
END:VEVENT\nEND:VCALENDAR\n",
        );
        assert_eq!(custom.start, Some(When::Time(ms(2024, 7, 1, 6, 0))));
    }

    #[test]
    fn floating_times_are_taken_as_utc() {
        let a = appt("BEGIN:VEVENT\nDTSTART:20240301T090000\nEND:VEVENT\n");
        assert_eq!(a.start, Some(When::Time(ms(2024, 3, 1, 9, 0))));
    }

    #[test]
    fn google_invite_with_utc_times_and_folded_lines() {
        let a = appt(
            "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\nDTSTART:20240301T150000Z\nDURATION:PT45M\n\
LOCATION:https://meet.example/abc-def\n  -ghi\nRRULE:FREQ=WEEKLY;BYDAY=FR\nEND:VEVENT\nEND:VCALENDAR\n",
        );
        assert_eq!(a.start, Some(When::Time(ms(2024, 3, 1, 15, 0))));
        assert_eq!(a.end, Some(When::Time(ms(2024, 3, 1, 15, 45))));
        assert_eq!(a.location.as_deref(), Some("https://meet.example/abc-def -ghi"));
        assert!(a.recurring);
        assert_eq!(a.recurrence.as_deref(), Some("FREQ=WEEKLY;BYDAY=FR"));
    }

    #[test]
    fn fold_inside_a_utf8_sequence_rejoins_and_text_escapes_decode() {
        let mut src = b"BEGIN:VEVENT\r\nLOCATION:Caf\xC3\r\n\t\xA9 \\; Bar\\nFloor 2\\\\3\r\nEND:VEVENT\r\n".to_vec();
        let Some(ItemDetails::Appointment(a)) = item_from_ical(&src) else { panic!("no appointment") };
        assert_eq!(a.location.as_deref(), Some("Café ; Bar\nFloor 2\\3"));
        src.splice(0..0, "\u{feff}".bytes());
        assert!(item_from_ical(&src).is_some(), "BOM ignored");
    }

    #[test]
    fn quoted_and_multi_valued_parameters() {
        let a = appt(
            "BEGIN:VEVENT\nATTENDEE;DELEGATED-FROM=\"mailto:a@example.org\",\"mailto:b@example.org\";\
CN=\"Jo ^'JJ^' Doe: Ops; QA\";ROLE=OPT-PARTICIPANT:MAILTO:jo@example.org\n\
ATTENDEE;CUTYPE=ROOM;CN=Lab:mailto:lab@example.org\nEND:VEVENT\n",
        );
        let jo = Address { name: Some("Jo \"JJ\" Doe: Ops; QA".into()), email: Some("jo@example.org".into()) };
        assert_eq!(a.optional, vec![jo]);
        assert_eq!(a.resources.len(), 1);
        assert!(a.required.is_empty());
    }

    #[test]
    fn all_day_event_keeps_calendar_dates() {
        let a = appt("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261224\nDTEND;VALUE=DATE:20261226\nEND:VEVENT\n");
        assert!(a.all_day);
        assert_eq!(a.start, Some(When::Date("2026-12-24".into())));
        assert_eq!(a.end, Some(When::Date("2026-12-26".into())));
        assert_eq!(a.meeting, None, "no METHOD: a published event, not a meeting message");

        let one_day = appt("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261231\nEND:VEVENT\n");
        assert_eq!(one_day.end, Some(When::Date("2027-01-01".into())), "RFC 5545: lasts one day");
        let week = appt("BEGIN:VEVENT\nDTSTART;VALUE=DATE:20261231\nDURATION:P1W\nEND:VEVENT\n");
        assert_eq!(week.end, Some(When::Date("2027-01-07".into())));
    }

    #[test]
    fn replies_and_cancellations() {
        let reply = |stat: &str| {
            appt(&format!(
                "BEGIN:VCALENDAR\nMETHOD:REPLY\nBEGIN:VEVENT\nORGANIZER:mailto:org@example.org\n\
ATTENDEE;PARTSTAT={stat}:mailto:guest@example.org\nDTSTART:20010101T100000Z\nEND:VEVENT\nEND:VCALENDAR\n"
            ))
            .meeting
        };
        assert_eq!(reply("DECLINED"), Some(MeetingKind::Declined));
        assert_eq!(reply("accepted"), Some(MeetingKind::Accepted));
        assert_eq!(reply("TENTATIVE"), Some(MeetingKind::Tentative));
        assert_eq!(reply("DELEGATED"), None);
        let cancel = appt("BEGIN:VCALENDAR\nMETHOD:CANCEL\nBEGIN:VEVENT\nEND:VEVENT\nEND:VCALENDAR\n");
        assert_eq!(cancel.meeting, Some(MeetingKind::Cancellation));
        let counter = appt("BEGIN:VCALENDAR\nMETHOD:COUNTER\nBEGIN:VEVENT\nEND:VEVENT\nEND:VCALENDAR\n");
        assert_eq!(counter.meeting, Some(MeetingKind::Counter));
    }

    #[test]
    fn master_event_wins_over_exceptions() {
        let a = appt(
            "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\nUID:u1\nRECURRENCE-ID:20240308T150000Z\n\
DTSTART:20240308T160000Z\nEND:VEVENT\nBEGIN:VEVENT\nUID:u1\nDTSTART:20240301T150000Z\n\
RRULE:FREQ=WEEKLY\nEND:VEVENT\nEND:VCALENDAR\n",
        );
        assert_eq!(a.start, Some(When::Time(ms(2024, 3, 1, 15, 0))));
        assert!(a.recurring);
        let only_exception = appt(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nRECURRENCE-ID:20240308T150000Z\nDTSTART:20240308T160000Z\n\
END:VEVENT\nEND:VCALENDAR\n",
        );
        assert_eq!(only_exception.start, Some(When::Time(ms(2024, 3, 8, 16, 0))));
    }

    #[test]
    fn stray_end_does_not_close_the_event() {
        let a = appt(
            "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\nEND:VALARM\nDTSTART:20240301T150000Z\n\
BEGIN:VALARM\nTRIGGER:-PT15M\nEND:VEVENT\nEND:VCALENDAR\n",
        );
        assert_eq!(a.start, Some(When::Time(ms(2024, 3, 1, 15, 0))));
        assert_eq!(a.meeting, Some(MeetingKind::Request));
    }

    #[test]
    fn vtodo_becomes_a_task_with_calendar_dates() {
        let Some(ItemDetails::Task(t)) = item_from_ical(
            format!(
                "BEGIN:VCALENDAR\n{PACIFIC}BEGIN:VTODO\nSTATUS:IN-PROCESS\nPERCENT-COMPLETE:40\n\
DTSTART;VALUE=DATE:20260110\nDUE;TZID=Pacific Standard Time:20260115T200000\n\
COMPLETED:20260120T101500Z\nEND:VTODO\nEND:VCALENDAR\n"
            )
            .as_bytes(),
        ) else {
            panic!("no task")
        };
        assert_eq!(t.status, Some(TaskStatus::InProgress));
        assert_eq!(t.percent_complete, Some(40));
        assert_eq!(t.start, Some(When::Date("2026-01-10".into())));
        assert_eq!(t.due, Some(When::Date("2026-01-15".into())), "the written day, not the UTC day");
        assert_eq!(t.completed, Some(When::Date("2026-01-20".into())));
    }

    #[test]
    fn garbage_and_truncated_input_never_panic() {
        for src in [
            "",
            "BEGIN:VEVENT",
            "BEGIN:VEVENT\nDTSTART:2024\nDTEND;TZID=\"unterminated:20240101T000000\nEND:VEVENT",
            "END:VEVENT\nEND:VCALENDAR",
            "BEGIN:VEVENT\nDURATION:P\nDTSTART:20241399T999999\nATTENDEE\nEND:VEVENT",
            "\u{feff}BEGIN:VCALENDAR\n\tcontinued\n:\n;;;\n",
            // Multi-byte characters where fixed-width digits are expected.
            "BEGIN:VEVENT\nDTSTART:abcé123\nDUE:20240101T1é000\nEND:VEVENT",
            "BEGIN:VEVENT\nDTSTART:20240101T1é000\nEND:VEVENT",
            "BEGIN:VTIMEZONE\nTZID:x\nBEGIN:STANDARD\nDTSTART:20000101T000000\nTZOFFSETTO:+0é00\n\
RRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=9223372036854775807SU\nEND:STANDARD\nEND:VTIMEZONE\n\
BEGIN:VEVENT\nDTSTART;TZID=x:20240101T000000\nEND:VEVENT",
            "BEGIN:VEVENT\nDTSTART:20240101T000000Z\nDURATION:P9999999999999999W\nEND:VEVENT",
            "BEGIN:VEVENT\nDTSTART;VALUE=DATE:20240101\nDURATION:-P99999999999999999999D\nEND:VEVENT",
        ] {
            let _ = item_from_ical(src.as_bytes());
        }
        let overflow = appt("BEGIN:VEVENT\nDTSTART:20240101T000000Z\nDURATION:P999999999999999W\nEND:VEVENT\n");
        assert_eq!(overflow.end, None);
        assert!(item_from_ical(&vec![b'A'; MAX_ICAL_BYTES + 1]).is_none());
    }

    #[test]
    fn deep_nesting_and_huge_inputs_stay_bounded() {
        let mut deep = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20240301T150000Z\n".to_owned();
        while deep.len() < MAX_ICAL_BYTES - 64 {
            deep.push_str("BEGIN:X\n");
        }
        deep.push_str("LOCATION:lost\n");
        assert_eq!(appt(&deep).start, Some(When::Time(ms(2024, 3, 1, 15, 0))));

        // Nested beyond MAX_DEPTH is skipped as a whole; the event's later properties survive.
        let nested = format!(
            "BEGIN:VEVENT\n{}LOCATION:inner\n{}LOCATION:outer\nEND:VEVENT\n",
            "BEGIN:X\n".repeat(20),
            "END:X\n".repeat(20)
        );
        assert_eq!(appt(&nested).location.as_deref(), Some("outer"));

        let mut wide = "BEGIN:VEVENT\nDTSTART:20240301T150000Z\n".to_owned();
        while wide.len() < MAX_ICAL_BYTES - 64 {
            wide.push_str("ATTENDEE:mailto:a@example.org\nBEGIN:VALARM\nEND:VALARM\n");
        }
        assert!(!appt(&wide).required.is_empty());
        let long_line = format!("BEGIN:VEVENT\nLOCATION:{}\nEND:VEVENT\n", "x".repeat(MAX_ICAL_BYTES - 64));
        assert_eq!(appt(&long_line).location.map(|l| l.len()), Some(MAX_ICAL_BYTES - 64));
    }
}
