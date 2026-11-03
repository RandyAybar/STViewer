//! iCalendar `TZID`s without a matching `VTIMEZONE` → UTC offsets: IANA names (bundled tz
//! database), Windows zone ids (`Pacific Standard Time`) and Outlook display names
//! (`(UTC-08:00) Pacific Time (US & Canada)`, `(GMT-08.00) …`).

use jiff::civil::DateTime;
use jiff::tz::{Offset, TimeZone};
use mail_model::time::civil_from_days;

/// Longer TZIDs are not looked up (real ones are well under this).
const MAX_TZID: usize = 128;

/// UTC offset (seconds east) in zone `tzid` of the wall-clock time `secs` into day `days`
/// (since 1970-01-01). A time skipped by a DST change takes the offset before the change, a
/// repeated one its first occurrence (RFC 5545 3.3.5). `None` for an unknown zone.
pub(crate) fn offset(tzid: &str, days: i64, secs: i64) -> Option<i64> {
    let tz = zone(tzid)?;
    let (y, m, d) = civil_from_days(days);
    // A leap second at 23:59:60 is read as 23:59:59.
    let s = secs.clamp(0, 86_399);
    let dt = DateTime::new(
        i16::try_from(y).ok()?,
        m as i8,
        d as i8,
        (s / 3600) as i8,
        (s / 60 % 60) as i8,
        (s % 60) as i8,
        0,
    )
    .ok()?;
    let ts = tz.to_ambiguous_timestamp(dt).compatible().ok()?;
    Some(days * 86_400 + s - ts.as_second())
}

fn zone(tzid: &str) -> Option<TimeZone> {
    let name = tzid.trim().trim_matches('"').trim();
    if name.is_empty() || name.len() > MAX_TZID {
        return None;
    }
    iana(name).or_else(|| windows(name)).or_else(|| display(name))
}

/// An IANA name, also as the tail of a globally unique id such as `/America/New_York` or
/// `/mozilla.org/20050126_1/America/New_York` (RFC 5545 3.2.19).
fn iana(name: &str) -> Option<TimeZone> {
    std::iter::once(name)
        .chain(name.match_indices('/').map(|(i, _)| &name[i + 1..]))
        .filter(|n| !n.is_empty())
        .find_map(|n| TimeZone::get(n).ok())
}

fn windows(name: &str) -> Option<TimeZone> {
    let (_, iana) = WINDOWS_TO_IANA.iter().find(|(w, _)| w.eq_ignore_ascii_case(name))?;
    TimeZone::get(iana).ok()
}

/// `(UTC+01:00) Amsterdam, Berlin, …`: the zone of a known label, else the fixed offset
/// the prefix states (`(UTC)` / `(GMT)` = 0).
fn display(name: &str) -> Option<TimeZone> {
    let (head, label) = name.strip_prefix('(')?.split_once(')')?;
    let head = head.trim();
    let prefix = head.get(..3).filter(|p| p.eq_ignore_ascii_case("UTC") || p.eq_ignore_ascii_case("GMT"))?;
    let label = label.trim();
    if let Some((_, w)) = DISPLAY_TO_WINDOWS.iter().find(|(d, _)| d.eq_ignore_ascii_case(label)) {
        return windows(w);
    }
    let secs = match head[prefix.len()..].trim().as_bytes() {
        [] => 0,
        &[sign @ (b'+' | b'-'), h1, h2, b':' | b'.', m1, m2] if [h1, h2, m1, m2].iter().all(u8::is_ascii_digit) => {
            let digit = |b: u8| i32::from(b - b'0');
            let secs = (digit(h1) * 10 + digit(h2)) * 3600 + (digit(m1) * 10 + digit(m2)) * 60;
            if sign == b'-' {
                -secs
            } else {
                secs
            }
        }
        _ => return None,
    };
    Some(TimeZone::fixed(Offset::from_seconds(secs).ok()?))
}

/// Windows time zone id → IANA name: CLDR `windowsZones.xml`, territory "001" (current IANA
/// spellings), plus ids older Outlook versions wrote.
static WINDOWS_TO_IANA: &[(&str, &str)] = &[
    ("Afghanistan Standard Time", "Asia/Kabul"),
    ("Alaskan Standard Time", "America/Anchorage"),
    ("Aleutian Standard Time", "America/Adak"),
    ("Altai Standard Time", "Asia/Barnaul"),
    ("Arab Standard Time", "Asia/Riyadh"),
    ("Arabian Standard Time", "Asia/Dubai"),
    ("Arabic Standard Time", "Asia/Baghdad"),
    ("Argentina Standard Time", "America/Argentina/Buenos_Aires"),
    ("Armenian Standard Time", "Asia/Yerevan"),
    ("Astrakhan Standard Time", "Europe/Astrakhan"),
    ("Atlantic Standard Time", "America/Halifax"),
    ("AUS Central Standard Time", "Australia/Darwin"),
    ("Aus Central W. Standard Time", "Australia/Eucla"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
    ("Azerbaijan Standard Time", "Asia/Baku"),
    ("Azores Standard Time", "Atlantic/Azores"),
    ("Bahia Standard Time", "America/Bahia"),
    ("Bangladesh Standard Time", "Asia/Dhaka"),
    ("Belarus Standard Time", "Europe/Minsk"),
    ("Bougainville Standard Time", "Pacific/Bougainville"),
    ("Canada Central Standard Time", "America/Regina"),
    ("Cape Verde Standard Time", "Atlantic/Cape_Verde"),
    ("Caucasus Standard Time", "Asia/Yerevan"),
    ("Cen. Australia Standard Time", "Australia/Adelaide"),
    ("Central America Standard Time", "America/Guatemala"),
    ("Central Asia Standard Time", "Asia/Bishkek"),
    ("Central Brazilian Standard Time", "America/Cuiaba"),
    ("Central Europe Standard Time", "Europe/Budapest"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("Central Pacific Standard Time", "Pacific/Guadalcanal"),
    ("Central Standard Time", "America/Chicago"),
    ("Central Standard Time (Mexico)", "America/Mexico_City"),
    ("Chatham Islands Standard Time", "Pacific/Chatham"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Cuba Standard Time", "America/Havana"),
    ("Dateline Standard Time", "Etc/GMT+12"),
    ("E. Africa Standard Time", "Africa/Nairobi"),
    ("E. Australia Standard Time", "Australia/Brisbane"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("E. South America Standard Time", "America/Sao_Paulo"),
    ("Easter Island Standard Time", "Pacific/Easter"),
    ("Eastern Standard Time", "America/New_York"),
    ("Eastern Standard Time (Mexico)", "America/Cancun"),
    ("Egypt Standard Time", "Africa/Cairo"),
    ("Ekaterinburg Standard Time", "Asia/Yekaterinburg"),
    ("Fiji Standard Time", "Pacific/Fiji"),
    ("FLE Standard Time", "Europe/Kyiv"),
    ("Georgian Standard Time", "Asia/Tbilisi"),
    ("GMT Standard Time", "Europe/London"),
    ("Greenland Standard Time", "America/Nuuk"),
    ("Greenwich Standard Time", "Atlantic/Reykjavik"),
    ("GTB Standard Time", "Europe/Bucharest"),
    ("Haiti Standard Time", "America/Port-au-Prince"),
    ("Hawaiian Standard Time", "Pacific/Honolulu"),
    ("India Standard Time", "Asia/Kolkata"),
    ("Iran Standard Time", "Asia/Tehran"),
    ("Israel Standard Time", "Asia/Jerusalem"),
    ("Jordan Standard Time", "Asia/Amman"),
    ("Kaliningrad Standard Time", "Europe/Kaliningrad"),
    ("Kamchatka Standard Time", "Asia/Kamchatka"),
    ("Korea Standard Time", "Asia/Seoul"),
    ("Libya Standard Time", "Africa/Tripoli"),
    ("Line Islands Standard Time", "Pacific/Kiritimati"),
    ("Lord Howe Standard Time", "Australia/Lord_Howe"),
    ("Magadan Standard Time", "Asia/Magadan"),
    ("Magallanes Standard Time", "America/Punta_Arenas"),
    ("Marquesas Standard Time", "Pacific/Marquesas"),
    ("Mauritius Standard Time", "Indian/Mauritius"),
    ("Mexico Standard Time", "America/Mexico_City"),
    ("Mexico Standard Time 2", "America/Chihuahua"),
    ("Mid-Atlantic Standard Time", "Etc/GMT+2"),
    ("Middle East Standard Time", "Asia/Beirut"),
    ("Montevideo Standard Time", "America/Montevideo"),
    ("Morocco Standard Time", "Africa/Casablanca"),
    ("Mountain Standard Time", "America/Denver"),
    ("Mountain Standard Time (Mexico)", "America/Mazatlan"),
    ("Myanmar Standard Time", "Asia/Yangon"),
    ("N. Central Asia Standard Time", "Asia/Novosibirsk"),
    ("Namibia Standard Time", "Africa/Windhoek"),
    ("Nepal Standard Time", "Asia/Kathmandu"),
    ("New Zealand Standard Time", "Pacific/Auckland"),
    ("Newfoundland Standard Time", "America/St_Johns"),
    ("Norfolk Standard Time", "Pacific/Norfolk"),
    ("North Asia East Standard Time", "Asia/Irkutsk"),
    ("North Asia Standard Time", "Asia/Krasnoyarsk"),
    ("North Korea Standard Time", "Asia/Pyongyang"),
    ("Omsk Standard Time", "Asia/Omsk"),
    ("Pacific SA Standard Time", "America/Santiago"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("Pacific Standard Time (Mexico)", "America/Tijuana"),
    ("Pakistan Standard Time", "Asia/Karachi"),
    ("Paraguay Standard Time", "America/Asuncion"),
    ("Qyzylorda Standard Time", "Asia/Qyzylorda"),
    ("Romance Standard Time", "Europe/Paris"),
    ("Russia Time Zone 10", "Asia/Srednekolymsk"),
    ("Russia Time Zone 11", "Asia/Kamchatka"),
    ("Russia Time Zone 3", "Europe/Samara"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("SA Eastern Standard Time", "America/Cayenne"),
    ("SA Pacific Standard Time", "America/Bogota"),
    ("SA Western Standard Time", "America/La_Paz"),
    ("Saint Pierre Standard Time", "America/Miquelon"),
    ("Sakhalin Standard Time", "Asia/Sakhalin"),
    ("Samoa Standard Time", "Pacific/Apia"),
    ("Sao Tome Standard Time", "Africa/Sao_Tome"),
    ("Saratov Standard Time", "Europe/Saratov"),
    ("SE Asia Standard Time", "Asia/Bangkok"),
    ("Singapore Standard Time", "Asia/Singapore"),
    ("South Africa Standard Time", "Africa/Johannesburg"),
    ("South Sudan Standard Time", "Africa/Juba"),
    ("Sri Lanka Standard Time", "Asia/Colombo"),
    ("Sudan Standard Time", "Africa/Khartoum"),
    ("Syria Standard Time", "Asia/Damascus"),
    ("Taipei Standard Time", "Asia/Taipei"),
    ("Tasmania Standard Time", "Australia/Hobart"),
    ("Tocantins Standard Time", "America/Araguaina"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("Tomsk Standard Time", "Asia/Tomsk"),
    ("Tonga Standard Time", "Pacific/Tongatapu"),
    ("Transbaikal Standard Time", "Asia/Chita"),
    ("Turkey Standard Time", "Europe/Istanbul"),
    ("Turks And Caicos Standard Time", "America/Grand_Turk"),
    ("Ulaanbaatar Standard Time", "Asia/Ulaanbaatar"),
    ("US Eastern Standard Time", "America/Indiana/Indianapolis"),
    ("US Mountain Standard Time", "America/Phoenix"),
    ("UTC", "Etc/UTC"),
    ("UTC+12", "Etc/GMT-12"),
    ("UTC+13", "Etc/GMT-13"),
    ("UTC-02", "Etc/GMT+2"),
    ("UTC-08", "Etc/GMT+8"),
    ("UTC-09", "Etc/GMT+9"),
    ("UTC-11", "Etc/GMT+11"),
    ("Venezuela Standard Time", "America/Caracas"),
    ("Vladivostok Standard Time", "Asia/Vladivostok"),
    ("Volgograd Standard Time", "Europe/Volgograd"),
    ("W. Australia Standard Time", "Australia/Perth"),
    ("W. Central Africa Standard Time", "Africa/Lagos"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("W. Mongolia Standard Time", "Asia/Hovd"),
    ("West Asia Standard Time", "Asia/Tashkent"),
    ("West Bank Standard Time", "Asia/Hebron"),
    ("West Pacific Standard Time", "Pacific/Port_Moresby"),
    ("Yakutsk Standard Time", "Asia/Yakutsk"),
    ("Yukon Standard Time", "America/Whitehorse"),
];

/// Windows display labels (after the `(UTC±hh:mm)` prefix) of common zones → Windows id.
static DISPLAY_TO_WINDOWS: &[(&str, &str)] = &[
    ("Pacific Time (US & Canada)", "Pacific Standard Time"),
    ("Mountain Time (US & Canada)", "Mountain Standard Time"),
    ("Central Time (US & Canada)", "Central Standard Time"),
    ("Eastern Time (US & Canada)", "Eastern Standard Time"),
    ("Atlantic Time (Canada)", "Atlantic Standard Time"),
    ("Arizona", "US Mountain Standard Time"),
    ("Alaska", "Alaskan Standard Time"),
    ("Hawaii", "Hawaiian Standard Time"),
    ("Indiana (East)", "US Eastern Standard Time"),
    ("Saskatchewan", "Canada Central Standard Time"),
    ("Newfoundland", "Newfoundland Standard Time"),
    ("Guadalajara, Mexico City, Monterrey", "Central Standard Time (Mexico)"),
    ("Bogota, Lima, Quito", "SA Pacific Standard Time"),
    ("Bogota, Lima, Quito, Rio Branco", "SA Pacific Standard Time"),
    ("Brasilia", "E. South America Standard Time"),
    ("Coordinated Universal Time", "UTC"),
    ("Dublin, Edinburgh, Lisbon, London", "GMT Standard Time"),
    ("Greenwich Mean Time : Dublin, Edinburgh, Lisbon, London", "GMT Standard Time"),
    ("Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna", "W. Europe Standard Time"),
    ("Brussels, Copenhagen, Madrid, Paris", "Romance Standard Time"),
    ("Belgrade, Bratislava, Budapest, Ljubljana, Prague", "Central Europe Standard Time"),
    ("Sarajevo, Skopje, Warsaw, Zagreb", "Central European Standard Time"),
    ("Helsinki, Kyiv, Riga, Sofia, Tallinn, Vilnius", "FLE Standard Time"),
    ("Helsinki, Kiev, Riga, Sofia, Tallinn, Vilnius", "FLE Standard Time"),
    ("Athens, Bucharest", "GTB Standard Time"),
    ("Istanbul", "Turkey Standard Time"),
    ("Cairo", "Egypt Standard Time"),
    ("Harare, Pretoria", "South Africa Standard Time"),
    ("Jerusalem", "Israel Standard Time"),
    ("Moscow, St. Petersburg", "Russian Standard Time"),
    ("Moscow, St. Petersburg, Volgograd", "Russian Standard Time"),
    ("Abu Dhabi, Muscat", "Arabian Standard Time"),
    ("Chennai, Kolkata, Mumbai, New Delhi", "India Standard Time"),
    ("Bangkok, Hanoi, Jakarta", "SE Asia Standard Time"),
    ("Beijing, Chongqing, Hong Kong, Urumqi", "China Standard Time"),
    ("Kuala Lumpur, Singapore", "Singapore Standard Time"),
    ("Taipei", "Taipei Standard Time"),
    ("Osaka, Sapporo, Tokyo", "Tokyo Standard Time"),
    ("Seoul", "Korea Standard Time"),
    ("Perth", "W. Australia Standard Time"),
    ("Darwin", "AUS Central Standard Time"),
    ("Adelaide", "Cen. Australia Standard Time"),
    ("Brisbane", "E. Australia Standard Time"),
    ("Canberra, Melbourne, Sydney", "AUS Eastern Standard Time"),
    ("Hobart", "Tasmania Standard Time"),
    ("Auckland, Wellington", "New Zealand Standard Time"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use mail_model::time::days_from_civil;

    fn at(tzid: &str, y: i64, m: u32, d: u32, h: i64) -> Option<i64> {
        offset(tzid, days_from_civil(y, m, d), h * 3600)
    }

    #[test]
    fn every_mapped_zone_exists_in_the_bundled_database() {
        for (w, iana) in WINDOWS_TO_IANA {
            assert!(TimeZone::get(iana).is_ok(), "{w} → {iana}");
        }
        for (label, w) in DISPLAY_TO_WINDOWS {
            assert!(windows(w).is_some(), "{label} → {w}");
        }
    }

    #[test]
    fn names_ids_and_display_labels() {
        assert_eq!(at("america/new_york", 2024, 7, 1, 12), Some(-4 * 3600));
        assert_eq!(at("/mozilla.org/20050126_1/America/New_York", 2024, 1, 2, 12), Some(-5 * 3600));
        assert_eq!(at("\"Romance Standard Time\"", 2024, 7, 1, 12), Some(2 * 3600));
        assert_eq!(at("(UTC-08:00) Pacific Time (US & Canada)", 2024, 7, 1, 12), Some(-7 * 3600));
        assert_eq!(at("(GMT+05.30) Somewhere Else", 2024, 7, 1, 12), Some(5 * 3600 + 1800), "stated offset");
        assert_eq!(at("(GMT) Greenwich Mean Time : Dublin, Edinburgh, Lisbon, London", 2024, 7, 1, 12), Some(3600));
        // 02:00 on the spring-forward day does not exist: the offset before the gap applies
        // (RFC 5545 3.3.5), i.e. 03:00 EDT.
        assert_eq!(at("America/New_York", 2024, 3, 10, 2), Some(-5 * 3600));
        for unknown in ["", "Nowhere/Example", "(UTC+99:99) x", "(PST) x", "Example Standard Time", &"A/".repeat(100)] {
            assert_eq!(at(unknown, 2024, 7, 1, 12), None, "{unknown}");
        }
        assert_eq!(offset("Europe/London", days_from_civil(2016, 12, 31), 86_400), Some(0), "leap second");
    }
}
