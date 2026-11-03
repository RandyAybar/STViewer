//! Well-known named-property sets and named properties. [MS-OXPROPS], [MS-OXCDATA] 2.6.1
//!
//! GUIDs are in on-disk byte order (Data1/Data2/Data3 little-endian), which is how
//! PST, MSG and TNEF store them and how [`crate::PropKey::Named`] carries them.

use crate::NamedId;

/// Build an on-disk GUID from its textual components.
pub const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}

const OLE: [u8; 8] = [0xC0, 0, 0, 0, 0, 0, 0, 0x46];

pub const PS_MAPI: [u8; 16] = guid(0x0002_0328, 0, 0, OLE);
pub const PS_PUBLIC_STRINGS: [u8; 16] = guid(0x0002_0329, 0, 0, OLE);
pub const PS_INTERNET_HEADERS: [u8; 16] = guid(0x0002_0386, 0, 0, OLE);
pub const PSETID_APPOINTMENT: [u8; 16] = guid(0x0006_2002, 0, 0, OLE);
pub const PSETID_TASK: [u8; 16] = guid(0x0006_2003, 0, 0, OLE);
pub const PSETID_ADDRESS: [u8; 16] = guid(0x0006_2004, 0, 0, OLE);
pub const PSETID_COMMON: [u8; 16] = guid(0x0006_2008, 0, 0, OLE);
pub const PSETID_LOG: [u8; 16] = guid(0x0006_200A, 0, 0, OLE);
pub const PSETID_NOTE: [u8; 16] = guid(0x0006_200E, 0, 0, OLE);
pub const PSETID_SHARING: [u8; 16] = guid(0x0006_2040, 0, 0, OLE);
pub const PSETID_POST_RSS: [u8; 16] = guid(0x0006_2041, 0, 0, OLE);
pub const PSETID_MEETING: [u8; 16] =
    guid(0x6ED8_DA90, 0x450B, 0x101B, [0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05]);
pub const PSETID_MESSAGING: [u8; 16] =
    guid(0x41F2_8F13, 0x83F4, 0x4114, [0xA5, 0x84, 0xEE, 0xDB, 0x5A, 0x6B, 0x0B, 0xFF]);
pub const PSETID_UNIFIED_MESSAGING: [u8; 16] =
    guid(0x4442_858E, 0xA9E3, 0x4E80, [0xB9, 0x00, 0x31, 0x7A, 0x21, 0x0C, 0xC1, 0x5B]);
pub const PSETID_AIR_SYNC: [u8; 16] =
    guid(0x7103_5549, 0x0739, 0x4DCB, [0x91, 0x63, 0x00, 0xF0, 0x58, 0x0D, 0xBB, 0xDF]);
pub const PSETID_CALENDAR_ASSISTANT: [u8; 16] =
    guid(0x11000E07, 0xB51B, 0x40D6, [0xAF, 0x21, 0xCA, 0xA8, 0x5E, 0xDA, 0xB1, 0xD0]);
pub const PSETID_ATTACHMENT: [u8; 16] =
    guid(0x9635_7F7F, 0x59E1, 0x47D0, [0x99, 0xA7, 0x46, 0x51, 0x5C, 0x18, 0x3B, 0x54]);

// ---- Common named properties (LIDs within their property set) ----
pub const LID_REMINDER_DELTA: u32 = 0x8501;
pub const LID_REMINDER_TIME: u32 = 0x8502;
pub const LID_REMINDER_SET: u32 = 0x8503;
pub const LID_PRIVATE: u32 = 0x8506;
pub const LID_SMART_NO_ATTACH: u32 = 0x8514;
pub const LID_COMMON_START: u32 = 0x8516;
pub const LID_COMMON_END: u32 = 0x8517;
pub const LID_FLAG_REQUEST: u32 = 0x8530;
pub const LID_REMINDER_SIGNAL_TIME: u32 = 0x8560;
pub const LID_INTERNET_ACCOUNT_NAME: u32 = 0x8580;
pub const LID_INTERNET_ACCOUNT_STAMP: u32 = 0x8581;
pub const LID_USE_TNEF: u32 = 0x8582;
pub const LID_LOCATION: u32 = 0x8208;
pub const LID_APPOINTMENT_START_WHOLE: u32 = 0x820D;
pub const LID_APPOINTMENT_END_WHOLE: u32 = 0x820E;
pub const LID_EMAIL1_EMAIL_ADDRESS: u32 = 0x8083;
pub const LID_GLOBAL_OBJECT_ID: u32 = 0x0003;
pub const LID_CLEAN_GLOBAL_OBJECT_ID: u32 = 0x0023;

/// `PidNameKeywords` (Outlook categories) lives in [`PS_PUBLIC_STRINGS`] with this string name.
pub const NAME_KEYWORDS: &str = "Keywords";

/// The `PidNameKeywords` key as found in a [`crate::PropMap`].
pub fn keywords_key() -> crate::PropKey {
    crate::PropKey::Named { guid: PS_PUBLIC_STRINGS, id: NamedId::Name(NAME_KEYWORDS.to_owned()) }
}

static LID_NAMES: &[([u8; 16], u32, &str)] = &[
    (PSETID_COMMON, 0x8501, "PidLidReminderDelta"),
    (PSETID_COMMON, 0x8502, "PidLidReminderTime"),
    (PSETID_COMMON, 0x8503, "PidLidReminderSet"),
    (PSETID_COMMON, 0x8506, "PidLidPrivate"),
    (PSETID_COMMON, 0x8510, "PidLidSideEffects"),
    (PSETID_COMMON, 0x8514, "PidLidSmartNoAttach"),
    (PSETID_COMMON, 0x8516, "PidLidCommonStart"),
    (PSETID_COMMON, 0x8517, "PidLidCommonEnd"),
    (PSETID_COMMON, 0x8518, "PidLidTaskMode"),
    (PSETID_COMMON, 0x8530, "PidLidFlagRequest"),
    (PSETID_COMMON, 0x8552, "PidLidCurrentVersion"),
    (PSETID_COMMON, 0x8554, "PidLidCurrentVersionName"),
    (PSETID_COMMON, 0x8560, "PidLidReminderSignalTime"),
    (PSETID_COMMON, 0x8580, "PidLidInternetAccountName"),
    (PSETID_COMMON, 0x8581, "PidLidInternetAccountStamp"),
    (PSETID_COMMON, 0x8582, "PidLidUseTnef"),
    (PSETID_COMMON, 0x85A0, "PidLidToDoOrdinalDate"),
    (PSETID_COMMON, 0x85A1, "PidLidToDoSubOrdinal"),
    (PSETID_COMMON, 0x85A4, "PidLidToDoTitle"),
    (PSETID_COMMON, 0x85BF, "PidLidValidFlagStringProof"),
    (PSETID_COMMON, 0x85C0, "PidLidFlagString"),
    (PSETID_APPOINTMENT, 0x8205, "PidLidBusyStatus"),
    (PSETID_APPOINTMENT, 0x8208, "PidLidLocation"),
    (PSETID_APPOINTMENT, 0x820D, "PidLidAppointmentStartWhole"),
    (PSETID_APPOINTMENT, 0x820E, "PidLidAppointmentEndWhole"),
    (PSETID_APPOINTMENT, 0x8213, "PidLidAppointmentDuration"),
    (PSETID_APPOINTMENT, 0x8215, "PidLidAppointmentSubType"),
    (PSETID_APPOINTMENT, 0x8216, "PidLidAppointmentRecur"),
    (PSETID_APPOINTMENT, 0x8217, "PidLidAppointmentStateFlags"),
    (PSETID_APPOINTMENT, 0x8218, "PidLidResponseStatus"),
    (PSETID_APPOINTMENT, 0x8223, "PidLidRecurring"),
    (PSETID_APPOINTMENT, 0x8231, "PidLidRecurrenceType"),
    (PSETID_APPOINTMENT, 0x8232, "PidLidRecurrencePattern"),
    (PSETID_APPOINTMENT, 0x8234, "PidLidTimeZoneDescription"),
    (PSETID_APPOINTMENT, 0x8235, "PidLidClipStart"),
    (PSETID_APPOINTMENT, 0x8236, "PidLidClipEnd"),
    (PSETID_APPOINTMENT, 0x8238, "PidLidAllAttendeesString"),
    (PSETID_APPOINTMENT, 0x823B, "PidLidToAttendeesString"),
    (PSETID_APPOINTMENT, 0x823C, "PidLidCcAttendeesString"),
    (PSETID_ADDRESS, 0x8005, "PidLidFileUnder"),
    (PSETID_ADDRESS, 0x801B, "PidLidWorkAddress"),
    (PSETID_ADDRESS, 0x802B, "PidLidHtml"),
    (PSETID_ADDRESS, 0x8062, "PidLidInstantMessagingAddress"),
    (PSETID_ADDRESS, 0x8080, "PidLidEmail1DisplayName"),
    (PSETID_ADDRESS, 0x8082, "PidLidEmail1AddressType"),
    (PSETID_ADDRESS, 0x8083, "PidLidEmail1EmailAddress"),
    (PSETID_ADDRESS, 0x8084, "PidLidEmail1OriginalDisplayName"),
    (PSETID_ADDRESS, 0x8090, "PidLidEmail2DisplayName"),
    (PSETID_ADDRESS, 0x8093, "PidLidEmail2EmailAddress"),
    (PSETID_ADDRESS, 0x80A0, "PidLidEmail3DisplayName"),
    (PSETID_ADDRESS, 0x80A3, "PidLidEmail3EmailAddress"),
    (PSETID_TASK, 0x8101, "PidLidTaskStatus"),
    (PSETID_TASK, 0x8102, "PidLidPercentComplete"),
    (PSETID_TASK, 0x8104, "PidLidTaskStartDate"),
    (PSETID_TASK, 0x8105, "PidLidTaskDueDate"),
    (PSETID_TASK, 0x811C, "PidLidTaskComplete"),
    (PSETID_NOTE, 0x8B00, "PidLidNoteColor"),
    (PSETID_NOTE, 0x8B02, "PidLidNoteWidth"),
    (PSETID_NOTE, 0x8B03, "PidLidNoteHeight"),
    (PSETID_LOG, 0x8700, "PidLidLogType"),
    (PSETID_LOG, 0x8706, "PidLidLogStart"),
    (PSETID_LOG, 0x8708, "PidLidLogEnd"),
    (PSETID_MEETING, 0x0003, "PidLidGlobalObjectId"),
    (PSETID_MEETING, 0x0023, "PidLidCleanGlobalObjectId"),
];

static STRING_NAMES: &[([u8; 16], &str, &str)] = &[
    (PS_PUBLIC_STRINGS, "Keywords", "PidNameKeywords"),
    (PS_PUBLIC_STRINGS, "urn:schemas-microsoft-com:office:office#Keywords", "PidNameKeywords"),
    (PS_INTERNET_HEADERS, "content-type", "PidNameContentType"),
    (PS_INTERNET_HEADERS, "content-class", "PidNameContentClass"),
    (PS_INTERNET_HEADERS, "x-mailer", "PidNameXMailer"),
    (PS_INTERNET_HEADERS, "Accept-Language", "PidNameAcceptLanguage"),
];

/// Canonical name of a named property, if it is one of the well-known ones above.
pub fn named_name(guid: &[u8; 16], id: &NamedId) -> Option<&'static str> {
    match id {
        NamedId::Lid(lid) => LID_NAMES.iter().find(|e| &e.0 == guid && e.1 == *lid).map(|e| e.2),
        NamedId::Name(n) => STRING_NAMES.iter().find(|e| &e.0 == guid && e.1.eq_ignore_ascii_case(n)).map(|e| e.2),
    }
}

/// Short symbolic name of a property-set GUID (`PS_PUBLIC_STRINGS`), if well known.
pub fn guid_name(guid: &[u8; 16]) -> Option<&'static str> {
    Some(match *guid {
        PS_MAPI => "PS_MAPI",
        PS_PUBLIC_STRINGS => "PS_PUBLIC_STRINGS",
        PS_INTERNET_HEADERS => "PS_INTERNET_HEADERS",
        PSETID_APPOINTMENT => "PSETID_Appointment",
        PSETID_TASK => "PSETID_Task",
        PSETID_ADDRESS => "PSETID_Address",
        PSETID_COMMON => "PSETID_Common",
        PSETID_LOG => "PSETID_Log",
        PSETID_NOTE => "PSETID_Note",
        PSETID_SHARING => "PSETID_Sharing",
        PSETID_POST_RSS => "PSETID_PostRss",
        PSETID_MEETING => "PSETID_Meeting",
        PSETID_MESSAGING => "PSETID_Messaging",
        PSETID_UNIFIED_MESSAGING => "PSETID_UnifiedMessaging",
        PSETID_AIR_SYNC => "PSETID_AirSync",
        PSETID_CALENDAR_ASSISTANT => "PSETID_CalendarAssistant",
        PSETID_ATTACHMENT => "PSETID_Attachment",
        _ => return None,
    })
}

/// `{00020329-0000-0000-C000-000000000046}` from on-disk bytes.
pub fn format_guid(g: &[u8; 16]) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        u32::from_le_bytes([g[0], g[1], g[2], g[3]]),
        u16::from_le_bytes([g[4], g[5]]),
        u16::from_le_bytes([g[6], g[7]]),
        g[8],
        g[9],
        g[10],
        g[11],
        g[12],
        g[13],
        g[14],
        g[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_byte_order_matches_disk() {
        assert_eq!(PS_PUBLIC_STRINGS, [0x29, 0x03, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]);
        assert_eq!(format_guid(&PSETID_MEETING), "{6ED8DA90-450B-101B-98DA-00AA003F1305}");
        assert_eq!(guid_name(&PSETID_COMMON), Some("PSETID_Common"));
    }

    #[test]
    fn named_lookup() {
        assert_eq!(named_name(&PSETID_COMMON, &NamedId::Lid(0x8516)), Some("PidLidCommonStart"));
        assert_eq!(named_name(&PS_PUBLIC_STRINGS, &NamedId::Name("keywords".into())), Some("PidNameKeywords"));
        assert_eq!(named_name(&PSETID_TASK, &NamedId::Lid(0x8516)), None);
    }
}
