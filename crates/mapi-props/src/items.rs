//! Outlook appointments, meeting messages, contacts and tasks → [`ItemDetails`].
//! [MS-OXOCAL], [MS-OXOCNTC], [MS-OXOTASK].
//!
//! Works on full messages and on contents-table rows alike: rows hold no recipients, so
//! attendee lists stay empty there; [`ROW_FIXED_IDS`] and [`ROW_NAMED`] list the columns a
//! container should read so list rows carry the rest.

use mail_model::time::{civil_from_days, MS_PER_DAY};
use mail_model::{
    Address, Appointment, BusyStatus, Contact, ItemDetails, Labeled, MeetingKind, Task, TaskStatus, When,
};

use crate::{get, names, tags as t, MapiMessage, NamedId, PropKey, PropMap, PropValue};

const LID_BUSY_STATUS: u32 = 0x8205;
const LID_APPOINTMENT_SUB_TYPE: u32 = 0x8215;
const LID_RECURRING: u32 = 0x8223;
const LID_RECURRENCE_PATTERN: u32 = 0x8232;
const LID_PROPOSED_START_WHOLE: u32 = 0x8250;
const LID_PROPOSED_END_WHOLE: u32 = 0x8251;
const LID_COUNTER_PROPOSAL: u32 = 0x8257;
const LID_HOME_ADDRESS: u32 = 0x801A;
const LID_WORK_ADDRESS: u32 = 0x801B;
const LID_OTHER_ADDRESS: u32 = 0x801C;
const LID_HTML: u32 = 0x802B;
const LID_BIRTHDAY_LOCAL: u32 = 0x80DE;
/// Email1..3: (display name, address, address type, original display name = SMTP address).
const LID_EMAILS: [(u32, u32, u32, u32); 3] =
    [(0x8080, 0x8083, 0x8082, 0x8084), (0x8090, 0x8093, 0x8092, 0x8094), (0x80A0, 0x80A3, 0x80A2, 0x80A4)];
const LID_TASK_STATUS: u32 = 0x8101;
const LID_PERCENT_COMPLETE: u32 = 0x8102;
const LID_TASK_START: u32 = 0x8104;
const LID_TASK_DUE: u32 = 0x8105;
const LID_TASK_COMPLETED: u32 = 0x810F;
const LID_TASK_OWNER: u32 = 0x811F;
/// `PidLidTaskGlobalId` (PSETID_Common): a GUID shared by a task and its task requests.
const LID_TASK_GLOBAL_ID: u32 = 0x8519;

const BIRTHDAY: u16 = 0x3A42;
const PERSONAL_HOME_PAGE: u16 = 0x3A50;
const BUSINESS_HOME_PAGE: u16 = 0x3A51;
/// Contact phone numbers in display order.
const PHONES: [(u16, &str); 14] = [
    (t::MOBILE_TELEPHONE_NUMBER, "mobile"),
    (t::BUSINESS_TELEPHONE_NUMBER, "business"),
    (0x3A1B, "business 2"),
    (t::HOME_TELEPHONE_NUMBER, "home"),
    (0x3A2F, "home 2"),
    (0x3A1A, "primary"),
    (0x3A57, "company"),
    (0x3A1E, "car"),
    (0x3A2E, "assistant"),
    (0x3A1F, "other"),
    (0x3A24, "business fax"),
    (0x3A25, "home fax"),
    (0x3A23, "primary fax"),
    (0x3A21, "pager"),
];
/// `PidTagRecipientFlags` bit marking the meeting organizer's own recipient row.
const RECIP_ORGANIZER: i64 = 0x0002;
/// MAPI's "no date" values: 4501-01-01 (unset task dates, [MS-OXOTASK] 2.2.2.2.4) and the
/// 4500-08-31 recurrence end. Item times from 4500 on are treated as absent.
const NO_DATE_MS: i64 = 79_839_129_600_000;

/// Fixed-id contents-table columns list rows need for [`item_details`].
pub const ROW_FIXED_IDS: &[u16] = &[
    t::DISPLAY_NAME,
    t::COMPANY_NAME,
    t::MOBILE_TELEPHONE_NUMBER,
    t::BUSINESS_TELEPHONE_NUMBER,
    t::HOME_TELEPHONE_NUMBER,
];

/// Named contents-table columns list rows need for [`item_details`].
pub const ROW_NAMED: &[([u8; 16], u32)] = &[
    (names::PSETID_APPOINTMENT, names::LID_APPOINTMENT_START_WHOLE),
    (names::PSETID_APPOINTMENT, names::LID_APPOINTMENT_END_WHOLE),
    (names::PSETID_APPOINTMENT, names::LID_LOCATION),
    (names::PSETID_APPOINTMENT, LID_APPOINTMENT_SUB_TYPE),
    (names::PSETID_APPOINTMENT, LID_BUSY_STATUS),
    (names::PSETID_APPOINTMENT, LID_RECURRING),
    (names::PSETID_APPOINTMENT, LID_COUNTER_PROPOSAL),
    (names::PSETID_APPOINTMENT, LID_PROPOSED_START_WHOLE),
    (names::PSETID_APPOINTMENT, LID_PROPOSED_END_WHOLE),
    (names::PSETID_ADDRESS, names::LID_EMAIL1_EMAIL_ADDRESS),
    (names::PSETID_ADDRESS, 0x8080),
    (names::PSETID_TASK, LID_TASK_STATUS),
    (names::PSETID_TASK, LID_PERCENT_COMPLETE),
    (names::PSETID_TASK, LID_TASK_DUE),
];

/// Kind of non-mail item a message class denotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// Appointment or meeting message.
    Appointment,
    Contact,
    /// Task or task request.
    Task,
}

impl ItemKind {
    /// Item kind of a message class; `None` for mail and anything else.
    pub fn of_class(class: &str) -> Option<ItemKind> {
        classify(class).map(|(k, _)| k)
    }

    /// The named property ([`ROW_NAMED`]) whose presence as a contents-table column means the
    /// table carries this kind's details. A container whose table lacks it should read rows of
    /// this kind from the message itself.
    pub fn key_column(self) -> ([u8; 16], u32) {
        match self {
            ItemKind::Appointment => (names::PSETID_APPOINTMENT, names::LID_APPOINTMENT_START_WHOLE),
            ItemKind::Contact => (names::PSETID_ADDRESS, names::LID_EMAIL1_EMAIL_ADDRESS),
            ItemKind::Task => (names::PSETID_TASK, LID_TASK_DUE),
        }
    }
}

/// Class-based dispatch, ASCII case-insensitive; subclasses (`IPM.Appointment.Custom`) count.
fn classify(class: &str) -> Option<(ItemKind, Option<MeetingKind>)> {
    let class = class.trim_end_matches('\0').trim();
    let is = |p: &str| {
        class.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p))
            && matches!(class.as_bytes().get(p.len()), None | Some(b'.'))
    };
    if is("IPM.Appointment") {
        return Some((ItemKind::Appointment, None));
    }
    const MEETINGS: [(&str, MeetingKind); 5] = [
        ("IPM.Schedule.Meeting.Request", MeetingKind::Request),
        ("IPM.Schedule.Meeting.Canceled", MeetingKind::Cancellation),
        ("IPM.Schedule.Meeting.Resp.Pos", MeetingKind::Accepted),
        ("IPM.Schedule.Meeting.Resp.Tent", MeetingKind::Tentative),
        ("IPM.Schedule.Meeting.Resp.Neg", MeetingKind::Declined),
    ];
    if let Some(&(_, kind)) = MEETINGS.iter().find(|(p, _)| is(p)) {
        return Some((ItemKind::Appointment, Some(kind)));
    }
    if is("IPM.Contact") {
        return Some((ItemKind::Contact, None));
    }
    if is("IPM.Task") || is("IPM.TaskRequest") {
        return Some((ItemKind::Task, None));
    }
    None
}

fn named(p: &PropMap, guid: [u8; 16], lid: u32) -> Option<&PropValue> {
    p.get(&PropKey::Named { guid, id: NamedId::Lid(lid) }).map(|p| &p.value)
}

fn text(v: Option<&PropValue>) -> Option<String> {
    v.and_then(PropValue::as_str).map(|s| s.trim_end_matches('\0').trim()).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Integer value; error-valued properties (`PtypErrorCode`) are absent.
fn int(v: Option<&PropValue>) -> Option<i64> {
    v.filter(|v| !matches!(v, PropValue::ErrorCode(_))).and_then(PropValue::as_i64)
}

fn flag(v: Option<&PropValue>) -> bool {
    int(v).is_some_and(|i| i != 0)
}

/// A real point in time: MAPI "no date" placeholders are absent.
fn time(v: Option<&PropValue>) -> Option<i64> {
    v.and_then(PropValue::as_time).filter(|&ms| ms < NO_DATE_MS)
}

fn day(days: i64) -> When {
    let (y, m, d) = civil_from_days(days);
    When::Date(format!("{y:04}-{m:02}-{d:02}"))
}

/// Outlook stores an all-day boundary, a birthday or a task completion date as the creator's
/// local midnight converted to UTC, without the zone. Local midnight of a zone at UTC+o lies
/// at UTC midnight − o, so the day is recovered exactly for every offset in (−11 h, +13 h]
/// (all inhabited zones but UTC−11 and UTC+13:45/+14; New Zealand summer time included).
fn local_midnight_day(ms: i64) -> When {
    day((ms + 13 * 3_600_000).div_euclid(MS_PER_DAY))
}

/// Task start/due dates and `PidLidBirthdayLocal` hold the local date at 00:00 *as if* UTC:
/// the UTC date is the date.
fn utc_day(ms: i64) -> When {
    day(ms.div_euclid(MS_PER_DAY))
}

/// Class-based dispatch; `None` for mail and anything else.
pub fn item_details(m: &MapiMessage) -> Option<ItemDetails> {
    let (kind, meeting) = classify(get(&m.props, t::MESSAGE_CLASS)?.as_str()?)?;
    Some(match kind {
        ItemKind::Appointment => ItemDetails::Appointment(appointment(m, meeting)),
        ItemKind::Contact => ItemDetails::Contact(contact(&m.props)),
        ItemKind::Task => ItemDetails::Task(task(&m.props)),
    })
}

/// Upper-case hex of a global object id, or the iCalendar UID Outlook keeps inside one when
/// the item came from iCalendar ([MS-OXCICAL] 2.1.3.1.1.20.26).
fn global_object_uid(b: &[u8]) -> String {
    // [MS-OXOCAL] 2.2.1.27: 16-byte class id, YH YL M D, creation time (8), reserved (8),
    // Size (4), Data (Size).
    const VCAL_UID: &[u8] = b"vCal-Uid\x01\x00\x00\x00";
    let data = b.get(36..40).zip(b.get(40..)).map(|(size, data)| {
        let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]) as usize;
        &data[..size.min(data.len())]
    });
    let vcal = data
        .and_then(|d| d.strip_prefix(VCAL_UID))
        .and_then(|uid| std::str::from_utf8(uid.split(|&c| c == 0).next().unwrap_or_default()).ok())
        .map(str::trim)
        .filter(|uid| !uid.is_empty());
    if let Some(uid) = vcal {
        return uid.to_owned();
    }
    hex_upper(b)
}

fn hex_upper(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = write!(s, "{x:02X}");
        s
    })
}

fn appointment(m: &MapiMessage, meeting: Option<MeetingKind>) -> Appointment {
    let p = &m.props;
    let ap = |lid| named(p, names::PSETID_APPOINTMENT, lid);
    let all_day = flag(ap(LID_APPOINTMENT_SUB_TYPE));
    let at = |lid| time(ap(lid)).map(|ms| if all_day { local_midnight_day(ms) } else { When::Time(ms) });
    let response = matches!(meeting, Some(MeetingKind::Accepted | MeetingKind::Tentative | MeetingKind::Declined));
    // A response proposing a new time ([MS-OXOCAL] counter proposal) carries the proposed times.
    let counter = response && flag(ap(LID_COUNTER_PROPOSAL));
    let proposed = |proposed_lid, lid| if counter { at(proposed_lid).or_else(|| at(lid)) } else { at(lid) };
    let mut a = Appointment {
        start: proposed(LID_PROPOSED_START_WHOLE, names::LID_APPOINTMENT_START_WHOLE),
        end: proposed(LID_PROPOSED_END_WHOLE, names::LID_APPOINTMENT_END_WHOLE),
        all_day,
        location: text(ap(names::LID_LOCATION)),
        busy_status: int(ap(LID_BUSY_STATUS)).and_then(|v| match v {
            0 => Some(BusyStatus::Free),
            1 => Some(BusyStatus::Tentative),
            2 => Some(BusyStatus::Busy),
            3 => Some(BusyStatus::OutOfOffice),
            4 => Some(BusyStatus::WorkingElsewhere),
            _ => None,
        }),
        recurring: flag(ap(LID_RECURRING)),
        recurrence: text(ap(LID_RECURRENCE_PATTERN)),
        meeting: if counter { Some(MeetingKind::Counter) } else { meeting },
        uid: named(p, names::PSETID_MEETING, names::LID_CLEAN_GLOBAL_OBJECT_ID)
            .or_else(|| named(p, names::PSETID_MEETING, names::LID_GLOBAL_OBJECT_ID))
            .and_then(PropValue::as_bytes)
            .filter(|b| !b.is_empty())
            .map(global_object_uid),
        ..Appointment::default()
    };
    if response {
        // A response comes from an attendee and is addressed to the organizer (list rows,
        // without recipients, name them from `PidTagDisplayTo`).
        a.organizer = m.recipients.iter().find_map(crate::convert::recipient_address).or_else(|| {
            let name = text(get(p, t::DISPLAY_TO))?.split(';').next()?.trim().to_owned();
            (!name.is_empty()).then_some(Address { name: Some(name), email: None })
        });
        return a;
    }
    for r in &m.recipients {
        let Some(addr) = crate::convert::recipient_address(r) else { continue };
        if int(get(r, t::RECIPIENT_FLAGS)).is_some_and(|f| f & RECIP_ORGANIZER != 0) {
            // The organizer's own row; it also names the organizer when a delegate sent the item.
            a.organizer.get_or_insert(addr);
            continue;
        }
        match int(get(r, t::RECIPIENT_TYPE)).unwrap_or(1) & 0x0F {
            2 => a.optional.push(addr),
            3 => a.resources.push(addr),
            _ => a.required.push(addr),
        }
    }
    if a.organizer.is_none() {
        a.organizer = crate::convert::sender(p);
    }
    a
}

fn contact(p: &PropMap) -> Contact {
    let ad = |lid| named(p, names::PSETID_ADDRESS, lid);
    let emails = LID_EMAILS
        .iter()
        .filter_map(|&(name_lid, addr_lid, type_lid, smtp_lid)| {
            let addr = text(ad(addr_lid))?;
            // Exchange-internal (EX) addresses are distinguished names; the original display
            // name holds the matching SMTP address when the client knew it.
            let ex = text(ad(type_lid)).is_some_and(|t| t.eq_ignore_ascii_case("EX")) || addr.starts_with('/');
            let email = if ex {
                text(ad(smtp_lid))
                    .filter(|s| s.contains('@') && !s.starts_with('/') && !s.contains(char::is_whitespace))
            } else {
                Some(addr)
            };
            let name = text(ad(name_lid));
            (email.is_some() || name.is_some()).then_some(Address { name, email })
        })
        .collect();
    let labeled = |label: &str, value: Option<String>| value.map(|value| Labeled { label: label.to_owned(), value });
    Contact {
        display_name: text(get(p, t::DISPLAY_NAME)),
        given_name: text(get(p, t::GIVEN_NAME)),
        surname: text(get(p, t::SURNAME)),
        company: text(get(p, t::COMPANY_NAME)),
        job_title: text(get(p, t::TITLE)),
        department: text(get(p, t::DEPARTMENT_NAME)),
        emails,
        phones: PHONES.iter().filter_map(|&(id, label)| labeled(label, text(get(p, id)))).collect(),
        addresses: [("business", LID_WORK_ADDRESS), ("home", LID_HOME_ADDRESS), ("other", LID_OTHER_ADDRESS)]
            .into_iter()
            .filter_map(|(label, lid)| labeled(label, text(ad(lid))))
            .collect(),
        birthday: time(ad(LID_BIRTHDAY_LOCAL)).map(utc_day).or_else(|| time(get(p, BIRTHDAY)).map(local_midnight_day)),
        web_page: text(get(p, BUSINESS_HOME_PAGE))
            .or_else(|| text(get(p, PERSONAL_HOME_PAGE)))
            .or_else(|| text(ad(LID_HTML))),
    }
}

fn task(p: &PropMap) -> Task {
    let tk = |lid| named(p, names::PSETID_TASK, lid);
    Task {
        status: int(tk(LID_TASK_STATUS)).and_then(|v| match v {
            0 => Some(TaskStatus::NotStarted),
            1 => Some(TaskStatus::InProgress),
            2 => Some(TaskStatus::Complete),
            3 => Some(TaskStatus::Waiting),
            4 => Some(TaskStatus::Deferred),
            _ => None,
        }),
        percent_complete: match tk(LID_PERCENT_COMPLETE) {
            Some(PropValue::Float64(f)) if f.is_finite() => Some((f * 100.0).round().clamp(0.0, 100.0) as u8),
            _ => None,
        },
        start: time(tk(LID_TASK_START)).map(utc_day),
        due: time(tk(LID_TASK_DUE)).map(utc_day),
        // Unlike start/due, the completion date is local midnight converted to UTC
        // ([MS-OXOTASK] 2.2.2.2.9).
        completed: time(tk(LID_TASK_COMPLETED)).map(local_midnight_day),
        owner: text(tk(LID_TASK_OWNER)),
        uid: named(p, names::PSETID_COMMON, LID_TASK_GLOBAL_ID)
            .and_then(PropValue::as_bytes)
            .filter(|b| b.iter().any(|&x| x != 0))
            .map(hex_upper),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Prop, PropType};

    fn put(m: &mut PropMap, key: PropKey, ty: PropType, value: PropValue) {
        m.insert(key, Prop { ty, value });
    }
    fn s(m: &mut PropMap, id: u16, v: &str) {
        put(m, PropKey::Id(id), PropType::Unicode, PropValue::String(v.into()));
    }
    fn n(m: &mut PropMap, guid: [u8; 16], lid: u32, ty: PropType, v: PropValue) {
        put(m, PropKey::Named { guid, id: NamedId::Lid(lid) }, ty, v);
    }
    fn time_prop(m: &mut PropMap, guid: [u8; 16], lid: u32, ms: i64) {
        n(m, guid, lid, PropType::SysTime, PropValue::SysTime(ms));
    }
    fn ms(y: i64, mo: u32, d: u32, h: i64) -> i64 {
        (mail_model::time::days_from_civil(y, mo, d) * 24 + h) * 3_600_000
    }
    fn recip(name: &str, email: &str, ty: i32, flags: i32) -> PropMap {
        let mut r = PropMap::new();
        s(&mut r, t::DISPLAY_NAME, name);
        s(&mut r, t::SMTP_ADDRESS, email);
        put(&mut r, PropKey::Id(t::RECIPIENT_TYPE), PropType::Int32, PropValue::Int32(ty));
        put(&mut r, PropKey::Id(t::RECIPIENT_FLAGS), PropType::Int32, PropValue::Int32(flags));
        r
    }
    fn with_class(class: &str) -> MapiMessage {
        let mut m = MapiMessage::default();
        s(&mut m.props, t::MESSAGE_CLASS, class);
        m
    }
    fn appt(m: &MapiMessage) -> Appointment {
        match item_details(m) {
            Some(ItemDetails::Appointment(a)) => a,
            other => panic!("no appointment: {other:?}"),
        }
    }
    fn names_of(v: &[Address]) -> Vec<String> {
        v.iter().filter_map(|a| a.name.clone()).collect()
    }

    #[test]
    fn meeting_request_with_attendees_split_by_type() {
        let mut m = with_class("IPM.Schedule.Meeting.Request");
        // Sent by a delegate: the organizer is the flagged recipient row, not the sender.
        s(&mut m.props, t::SENDER_NAME, "Assistant");
        s(&mut m.props, t::SENDER_SMTP_ADDRESS, "assistant@example.org");
        let ap = names::PSETID_APPOINTMENT;
        time_prop(&mut m.props, ap, names::LID_APPOINTMENT_START_WHOLE, ms(2001, 6, 12, 16));
        time_prop(&mut m.props, ap, names::LID_APPOINTMENT_END_WHOLE, ms(2001, 6, 12, 17));
        n(&mut m.props, ap, names::LID_LOCATION, PropType::Unicode, PropValue::String("Room 101".into()));
        n(&mut m.props, ap, LID_BUSY_STATUS, PropType::Int32, PropValue::Int32(3));
        n(&mut m.props, ap, LID_RECURRING, PropType::Boolean, PropValue::Boolean(true));
        n(&mut m.props, ap, LID_RECURRENCE_PATTERN, PropType::Unicode, PropValue::String("every Tuesday".into()));
        m.recipients = vec![
            recip("Olivia Organizer", "olivia@example.org", 1, 0x3),
            recip("Rita Required", "rita@example.org", 1, 0x1),
            recip("Oscar Optional", "oscar@example.org", 2, 0x1),
            recip("Room 101", "room101@example.org", 3, 0x1),
        ];
        let a = appt(&m);
        assert_eq!(a.meeting, Some(MeetingKind::Request));
        assert_eq!(a.start, Some(When::Time(ms(2001, 6, 12, 16))));
        assert_eq!(a.location.as_deref(), Some("Room 101"));
        assert_eq!(a.busy_status, Some(BusyStatus::OutOfOffice));
        assert!(a.recurring);
        assert_eq!(a.recurrence.as_deref(), Some("every Tuesday"));
        assert_eq!(a.organizer.as_ref().and_then(|o| o.email.as_deref()), Some("olivia@example.org"));
        assert_eq!(names_of(&a.required), ["Rita Required"], "organizer row excluded");
        assert_eq!(names_of(&a.optional), ["Oscar Optional"]);
        assert_eq!(names_of(&a.resources), ["Room 101"]);

        m.recipients.remove(0);
        assert_eq!(appt(&m).organizer.and_then(|o| o.email).as_deref(), Some("assistant@example.org"), "sender");
    }

    #[test]
    fn meeting_classes_map_case_insensitively() {
        for (class, kind) in [
            ("ipm.schedule.meeting.request", Some(MeetingKind::Request)),
            ("IPM.Schedule.Meeting.Canceled", Some(MeetingKind::Cancellation)),
            ("IPM.Schedule.Meeting.Resp.Pos", Some(MeetingKind::Accepted)),
            ("IPM.Schedule.Meeting.Resp.Tent", Some(MeetingKind::Tentative)),
            ("IPM.Schedule.Meeting.Resp.Neg", Some(MeetingKind::Declined)),
            ("IPM.Appointment.MyForm", None),
        ] {
            assert_eq!(appt(&with_class(class)).meeting, kind, "{class}");
        }
        assert_eq!(ItemKind::of_class("IPM.TaskRequest.Accept"), Some(ItemKind::Task));
        assert_eq!(ItemKind::of_class("IPM.Contact\0"), Some(ItemKind::Contact));
    }

    #[test]
    fn response_is_addressed_to_the_organizer() {
        let mut m = with_class("IPM.Schedule.Meeting.Resp.Neg");
        s(&mut m.props, t::SENDER_NAME, "Rita Required");
        m.recipients = vec![recip("Olivia Organizer", "olivia@example.org", 1, 0x1)];
        let a = appt(&m);
        assert_eq!(a.organizer.and_then(|o| o.name).as_deref(), Some("Olivia Organizer"));
        assert!(a.required.is_empty(), "the organizer is no attendee");

        // A list row: no recipient table, only the display list.
        m.recipients.clear();
        s(&mut m.props, t::DISPLAY_TO, "Olivia Organizer; ");
        assert_eq!(appt(&m).organizer.and_then(|o| o.name).as_deref(), Some("Olivia Organizer"));
    }

    #[test]
    fn counter_proposal_carries_the_proposed_time() {
        let ap = names::PSETID_APPOINTMENT;
        let mut m = with_class("IPM.Schedule.Meeting.Resp.Tent");
        time_prop(&mut m.props, ap, names::LID_APPOINTMENT_START_WHOLE, ms(2026, 5, 4, 9));
        time_prop(&mut m.props, ap, names::LID_APPOINTMENT_END_WHOLE, ms(2026, 5, 4, 10));
        time_prop(&mut m.props, ap, LID_PROPOSED_START_WHOLE, ms(2026, 5, 5, 14));
        time_prop(&mut m.props, ap, LID_PROPOSED_END_WHOLE, ms(2026, 5, 5, 15));
        assert_eq!(appt(&m).meeting, Some(MeetingKind::Tentative), "no counter flag: a plain response");
        assert_eq!(appt(&m).start, Some(When::Time(ms(2026, 5, 4, 9))));

        n(&mut m.props, ap, LID_COUNTER_PROPOSAL, PropType::Boolean, PropValue::Boolean(true));
        m.recipients = vec![recip("Olivia Organizer", "olivia@example.org", 1, 0x1)];
        let a = appt(&m);
        assert_eq!(a.meeting, Some(MeetingKind::Counter));
        assert_eq!((a.start, a.end), (Some(When::Time(ms(2026, 5, 5, 14))), Some(When::Time(ms(2026, 5, 5, 15)))));
        assert_eq!(a.organizer.and_then(|o| o.name).as_deref(), Some("Olivia Organizer"), "still a response");
        assert!(a.required.is_empty());

        // The flag on a request is not a counter proposal.
        s(&mut m.props, t::MESSAGE_CLASS, "IPM.Schedule.Meeting.Request");
        assert_eq!(appt(&m).meeting, Some(MeetingKind::Request));
    }

    #[test]
    fn all_day_boundaries_become_calendar_dates_in_any_zone() {
        // Pacific/Honolulu, US Eastern, UTC, India, Japan, New Zealand summer time.
        for offset_min in [-600, -300, 0, 330, 540, 780] {
            let mut m = with_class("IPM.Appointment");
            let ap = names::PSETID_APPOINTMENT;
            n(&mut m.props, ap, LID_APPOINTMENT_SUB_TYPE, PropType::Boolean, PropValue::Boolean(true));
            // Local midnight of Dec 24 / Dec 26 for a creator at UTC+offset.
            let start = ms(2026, 12, 24, 0) - offset_min * 60_000;
            let end = ms(2026, 12, 26, 0) - offset_min * 60_000;
            time_prop(&mut m.props, ap, names::LID_APPOINTMENT_START_WHOLE, start);
            time_prop(&mut m.props, ap, names::LID_APPOINTMENT_END_WHOLE, end);
            let a = appt(&m);
            assert!(a.all_day);
            assert_eq!(a.start, Some(When::Date("2026-12-24".into())), "offset {offset_min}");
            assert_eq!(a.end, Some(When::Date("2026-12-26".into())), "offset {offset_min}");
        }
    }

    #[test]
    fn error_valued_flags_are_absent() {
        let mut m = with_class("IPM.Appointment");
        let ap = names::PSETID_APPOINTMENT;
        n(&mut m.props, ap, LID_APPOINTMENT_SUB_TYPE, PropType::ErrorCode, PropValue::ErrorCode(0x8004_010F_u32 as _));
        time_prop(&mut m.props, ap, names::LID_APPOINTMENT_START_WHOLE, ms(2026, 3, 1, 9));
        let a = appt(&m);
        assert!(!a.all_day);
        assert_eq!(a.start, Some(When::Time(ms(2026, 3, 1, 9))));
    }

    #[test]
    fn global_object_id_yields_ical_uid_or_hex() {
        let mut goid =
            vec![0x04, 0x00, 0x00, 0x00, 0x82, 0x00, 0xE0, 0x00, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82, 0xE0, 0x08];
        goid.extend_from_slice(&[0; 20]);
        let data = b"vCal-Uid\x01\x00\x00\x00event-1@example.org\x00";
        let with_size = |size: u32| {
            let mut b = goid.clone();
            b.extend_from_slice(&size.to_le_bytes());
            b.extend_from_slice(data);
            b
        };
        assert_eq!(global_object_uid(&with_size(data.len() as u32)), "event-1@example.org");
        // A hostile size larger than the blob is clamped to it.
        assert_eq!(global_object_uid(&with_size(u32::MAX)), "event-1@example.org");
        // Truncated Data: not a vCal UID, so the id is shown as hex.
        assert!(global_object_uid(&with_size(4)).starts_with("040000008200E00074C5B7101A82E008"));
        assert_eq!(global_object_uid(&[0xAB, 0x01]), "AB01");
    }

    #[test]
    fn contact_fields_and_exchange_addresses() {
        let mut m = with_class("IPM.Contact");
        s(&mut m.props, t::DISPLAY_NAME, "Casey Contact");
        s(&mut m.props, t::COMPANY_NAME, "Example Corp");
        s(&mut m.props, t::MOBILE_TELEPHONE_NUMBER, "+1 555 0100");
        s(&mut m.props, t::BUSINESS_TELEPHONE_NUMBER, "+1 555 0199");
        s(&mut m.props, 0x3A24, "+1 555 0198");
        let ad = names::PSETID_ADDRESS;
        let str_ = |v: &str| PropValue::String(v.into());
        n(&mut m.props, ad, 0x8083, PropType::Unicode, str_("casey@example.org"));
        n(&mut m.props, ad, 0x8080, PropType::Unicode, str_("Casey (work)"));
        // EX entry with its SMTP address in the original display name.
        n(&mut m.props, ad, 0x8093, PropType::Unicode, str_("/o=Example/ou=Admin/cn=Recipients/cn=casey"));
        n(&mut m.props, ad, 0x8092, PropType::Unicode, str_("EX"));
        n(&mut m.props, ad, 0x8094, PropType::Unicode, str_("casey.c@corp.example.org"));
        // EX entry without one: name only.
        n(&mut m.props, ad, 0x80A3, PropType::Unicode, str_("/o=Example/ou=Admin/cn=Recipients/cn=casey2"));
        n(&mut m.props, ad, 0x80A0, PropType::Unicode, str_("Casey (old)"));
        n(&mut m.props, ad, LID_WORK_ADDRESS, PropType::Unicode, str_("1 Example Way\nSpringfield"));
        // Born 1963-05-07, saved by Outlook at UTC-7 local midnight.
        put(&mut m.props, PropKey::Id(BIRTHDAY), PropType::SysTime, PropValue::SysTime(ms(1963, 5, 7, 7)));
        let Some(ItemDetails::Contact(c)) = item_details(&m) else { panic!() };
        assert_eq!(c.display_name.as_deref(), Some("Casey Contact"));
        let emails: Vec<_> = c.emails.iter().map(|a| (a.name.as_deref(), a.email.as_deref())).collect();
        assert_eq!(
            emails,
            [
                (Some("Casey (work)"), Some("casey@example.org")),
                (None, Some("casey.c@corp.example.org")),
                (Some("Casey (old)"), None),
            ]
        );
        let phones: Vec<_> = c.phones.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(phones, ["mobile", "business", "business fax"]);
        assert_eq!(c.phones[0].value, "+1 555 0100");
        assert_eq!(c.addresses[0], Labeled { label: "business".into(), value: "1 Example Way\nSpringfield".into() });
        assert_eq!(c.birthday, Some(When::Date("1963-05-07".into())));

        // PidLidBirthdayLocal (local midnight, unconverted) wins: no zone guess needed.
        time_prop(&mut m.props, ad, LID_BIRTHDAY_LOCAL, ms(1963, 5, 8, 0));
        let Some(ItemDetails::Contact(c)) = item_details(&m) else { panic!() };
        assert_eq!(c.birthday, Some(When::Date("1963-05-08".into())));
    }

    #[test]
    fn task_status_percent_and_dates() {
        let mut m = with_class("IPM.Task");
        let tk = names::PSETID_TASK;
        n(&mut m.props, tk, LID_TASK_STATUS, PropType::Int32, PropValue::Int32(1));
        n(&mut m.props, tk, LID_PERCENT_COMPLETE, PropType::Float64, PropValue::Float64(0.25));
        time_prop(&mut m.props, tk, LID_TASK_DUE, ms(2001, 7, 2, 0));
        // "No start date" placeholder, 4501-01-01.
        time_prop(&mut m.props, tk, LID_TASK_START, ms(4501, 1, 1, 0));
        // Completed on Jul 3 by a user at UTC+2: local midnight is Jul 2 22:00 UTC.
        time_prop(&mut m.props, tk, LID_TASK_COMPLETED, ms(2001, 7, 2, 22));
        n(&mut m.props, tk, LID_TASK_OWNER, PropType::Unicode, PropValue::String("Terry Task".into()));
        let Some(ItemDetails::Task(t)) = item_details(&m) else { panic!() };
        assert_eq!(t.status, Some(TaskStatus::InProgress));
        assert_eq!(t.percent_complete, Some(25));
        assert_eq!(t.due, Some(When::Date("2001-07-02".into())));
        assert_eq!(t.start, None);
        assert_eq!(t.completed, Some(When::Date("2001-07-03".into())));
        assert_eq!(t.owner.as_deref(), Some("Terry Task"));
        assert_eq!(t.uid, None);

        let id = PropValue::Binary(vec![0xAB, 0x01, 0x00, 0xFF]);
        n(&mut m.props, names::PSETID_COMMON, LID_TASK_GLOBAL_ID, PropType::Binary, id);
        let Some(ItemDetails::Task(t)) = item_details(&m) else { panic!() };
        assert_eq!(t.uid.as_deref(), Some("AB0100FF"));
        // An all-zero id identifies nothing.
        let zero = PropValue::Binary(vec![0; 16]);
        n(&mut m.props, names::PSETID_COMMON, LID_TASK_GLOBAL_ID, PropType::Binary, zero);
        let Some(ItemDetails::Task(t)) = item_details(&m) else { panic!() };
        assert_eq!(t.uid, None);
    }

    #[test]
    fn no_date_constant_is_4500_01_01() {
        assert_eq!(NO_DATE_MS, ms(4500, 1, 1, 0));
    }

    #[test]
    fn mail_and_unknown_classes_have_no_item() {
        for class in ["IPM.Note", "IPM.Schedule.Meeting.Notification.Forward", "IPM.DistList", "IPM.Appointmentx"] {
            assert_eq!(item_details(&with_class(class)), None, "{class}");
        }
    }
}
