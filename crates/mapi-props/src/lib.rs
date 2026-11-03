//! MAPI property model shared by `stv-pst` (PST/OST) and `stv-msg` (MSG + TNEF).
//!
//! Containers decode their bytes into a [`MapiMessage`] (property maps with named
//! properties already resolved to [`PropKey::Named`]); [`convert`] turns that into
//! `mail_model::{Header, Message}` so every MAPI-based format renders identically.

use std::collections::BTreeMap;

pub mod codepage;
pub mod convert;
pub mod items;
pub mod names;
pub mod rtf;
pub mod tags;

/// MAPI property type (low 16 bits of a property tag). [MS-OXCDATA] 2.11.1
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u16)]
pub enum PropType {
    Null = 0x0001,
    Int16 = 0x0002,
    Int32 = 0x0003,
    Float32 = 0x0004,
    Float64 = 0x0005,
    Currency = 0x0006,
    AppTime = 0x0007,
    ErrorCode = 0x000A,
    Boolean = 0x000B,
    Object = 0x000D,
    Int64 = 0x0014,
    String8 = 0x001E,
    Unicode = 0x001F,
    SysTime = 0x0040,
    Guid = 0x0048,
    ServerId = 0x00FB,
    Restriction = 0x00FD,
    RuleAction = 0x00FE,
    Binary = 0x0102,
    MultiInt16 = 0x1002,
    MultiInt32 = 0x1003,
    MultiFloat32 = 0x1004,
    MultiFloat64 = 0x1005,
    MultiCurrency = 0x1006,
    MultiAppTime = 0x1007,
    MultiInt64 = 0x1014,
    MultiString8 = 0x101E,
    MultiUnicode = 0x101F,
    MultiSysTime = 0x1040,
    MultiGuid = 0x1048,
    MultiBinary = 0x1102,
}

impl PropType {
    pub fn from_u16(v: u16) -> Option<Self> {
        use PropType::*;
        Some(match v {
            0x0001 => Null,
            0x0002 => Int16,
            0x0003 => Int32,
            0x0004 => Float32,
            0x0005 => Float64,
            0x0006 => Currency,
            0x0007 => AppTime,
            0x000A => ErrorCode,
            0x000B => Boolean,
            0x000D => Object,
            0x0014 => Int64,
            0x001E => String8,
            0x001F => Unicode,
            0x0040 => SysTime,
            0x0048 => Guid,
            0x00FB => ServerId,
            0x00FD => Restriction,
            0x00FE => RuleAction,
            0x0102 => Binary,
            0x1002 => MultiInt16,
            0x1003 => MultiInt32,
            0x1004 => MultiFloat32,
            0x1005 => MultiFloat64,
            0x1006 => MultiCurrency,
            0x1007 => MultiAppTime,
            0x1014 => MultiInt64,
            0x101E => MultiString8,
            0x101F => MultiUnicode,
            0x1040 => MultiSysTime,
            0x1048 => MultiGuid,
            0x1102 => MultiBinary,
            _ => return None,
        })
    }
}

/// Decoded property value. Strings are UTF-8 (String8 decoded with the message codepage by the container).
#[derive(Clone, Debug, PartialEq)]
pub enum PropValue {
    Null,
    Int16(i16),
    Int32(i32),
    Float32(f32),
    Float64(f64),
    /// Currency, scaled by 10_000.
    Currency(i64),
    AppTime(f64),
    ErrorCode(u32),
    Boolean(bool),
    Int64(i64),
    String(String),
    /// FILETIME converted to UTC ms since Unix epoch.
    SysTime(i64),
    Guid([u8; 16]),
    Binary(Vec<u8>),
    /// PtypObject: container-specific handle (subnode id, storage index) to an embedded object.
    Object(u64),
    MultiInt16(Vec<i16>),
    MultiInt32(Vec<i32>),
    MultiFloat32(Vec<f32>),
    MultiFloat64(Vec<f64>),
    MultiCurrency(Vec<i64>),
    MultiAppTime(Vec<f64>),
    MultiInt64(Vec<i64>),
    MultiString(Vec<String>),
    MultiSysTime(Vec<i64>),
    MultiGuid(Vec<[u8; 16]>),
    MultiBinary(Vec<Vec<u8>>),
}

/// Named property identity. [MS-OXCDATA] 2.6.1
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NamedId {
    Lid(u32),
    Name(String),
}

/// Key of a property in a [`PropMap`]: a fixed tag id (< 0x8000) or a resolved named property.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PropKey {
    /// Property id (high 16 bits of the tag), e.g. `0x0037` for PidTagSubject.
    Id(u16),
    Named {
        guid: [u8; 16],
        id: NamedId,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    pub ty: PropType,
    pub value: PropValue,
}

pub type PropMap = BTreeMap<PropKey, Prop>;

/// One attachment's properties. The binary payload (`PidTagAttachDataBinary`) may be omitted
/// from `props` when the container loads it lazily; `embedded` holds a decoded embedded message
/// only when the container decoded it eagerly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapiAttachment {
    pub props: PropMap,
    pub embedded: Option<Box<MapiMessage>>,
}

/// Container-independent MAPI message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MapiMessage {
    pub props: PropMap,
    pub recipients: Vec<PropMap>,
    pub attachments: Vec<MapiAttachment>,
}

impl MapiMessage {
    pub fn get(&self, id: u16) -> Option<&PropValue> {
        self.props.get(&PropKey::Id(id)).map(|p| &p.value)
    }
}

/// Lookup of a fixed-id property in any [`PropMap`].
pub fn get(map: &PropMap, id: u16) -> Option<&PropValue> {
    map.get(&PropKey::Id(id)).map(|p| &p.value)
}

impl PropType {
    /// [MS-OXCDATA] name, e.g. `PtypString`.
    pub fn name(self) -> &'static str {
        use PropType::*;
        match self {
            Null => "PtypNull",
            Int16 => "PtypInteger16",
            Int32 => "PtypInteger32",
            Float32 => "PtypFloating32",
            Float64 => "PtypFloating64",
            Currency => "PtypCurrency",
            AppTime => "PtypFloatingTime",
            ErrorCode => "PtypErrorCode",
            Boolean => "PtypBoolean",
            Object => "PtypObject",
            Int64 => "PtypInteger64",
            String8 => "PtypString8",
            Unicode => "PtypString",
            SysTime => "PtypTime",
            Guid => "PtypGuid",
            ServerId => "PtypServerId",
            Restriction => "PtypRestriction",
            RuleAction => "PtypRuleAction",
            Binary => "PtypBinary",
            MultiInt16 => "PtypMultipleInteger16",
            MultiInt32 => "PtypMultipleInteger32",
            MultiFloat32 => "PtypMultipleFloating32",
            MultiFloat64 => "PtypMultipleFloating64",
            MultiCurrency => "PtypMultipleCurrency",
            MultiAppTime => "PtypMultipleFloatingTime",
            MultiInt64 => "PtypMultipleInteger64",
            MultiString8 => "PtypMultipleString8",
            MultiUnicode => "PtypMultipleString",
            MultiSysTime => "PtypMultipleTime",
            MultiGuid => "PtypMultipleGuid",
            MultiBinary => "PtypMultipleBinary",
        }
    }

    /// Size in bytes of a single fixed-size value, `None` for variable-size types.
    pub fn fixed_size(self) -> Option<usize> {
        use PropType::*;
        Some(match self {
            Null => 0,
            Int16 => 2,
            Boolean => 1,
            Int32 | Float32 | ErrorCode => 4,
            Float64 | Currency | AppTime | Int64 | SysTime => 8,
            Guid => 16,
            _ => return None,
        })
    }
}

impl PropValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// Integer view of Int16/Int32/Int64/Boolean/ErrorCode values.
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            PropValue::Int16(v) => Some(v as i64),
            PropValue::Int32(v) => Some(v as i64),
            PropValue::Int64(v) => Some(v),
            PropValue::Boolean(b) => Some(b as i64),
            PropValue::ErrorCode(v) => Some(v as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            PropValue::Boolean(b) => Some(b),
            _ => self.as_i64().map(|v| v != 0),
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            PropValue::Binary(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_time(&self) -> Option<i64> {
        match *self {
            PropValue::SysTime(t) => Some(t),
            _ => None,
        }
    }

    /// Human-readable rendering for the property inspector. Long strings and binaries are
    /// truncated (`max_chars` characters / `max_chars / 2` bytes of hex).
    pub fn display(&self, max_chars: usize) -> String {
        fn trunc(s: &str, max: usize) -> String {
            match s.char_indices().nth(max) {
                Some((i, _)) => format!("{}… ({} chars)", &s[..i], s.chars().count()),
                None => s.to_owned(),
            }
        }
        fn hex(b: &[u8], max_chars: usize) -> String {
            const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
            let n = (max_chars / 2).max(8);
            let mut s = String::with_capacity(2 * n.min(b.len()) + 24);
            for &x in b.iter().take(n) {
                s.push(DIGITS[usize::from(x >> 4)] as char);
                s.push(DIGITS[usize::from(x & 0xF)] as char);
            }
            if b.len() > n {
                s.push_str(&format!("… ({} bytes)", b.len()));
            }
            s
        }
        fn join<T>(v: &[T], f: impl Fn(&T) -> String) -> String {
            v.iter().map(f).collect::<Vec<_>>().join("; ")
        }
        match self {
            PropValue::Null => String::new(),
            PropValue::Int16(v) => v.to_string(),
            PropValue::Int32(v) => v.to_string(),
            PropValue::Float32(v) => v.to_string(),
            PropValue::Float64(v) | PropValue::AppTime(v) => v.to_string(),
            PropValue::Currency(v) => {
                let a = v.unsigned_abs();
                format!("{}{}.{:04}", if *v < 0 { "-" } else { "" }, a / 10_000, a % 10_000)
            }
            PropValue::ErrorCode(v) => format!("error 0x{v:08X}"),
            PropValue::Boolean(b) => b.to_string(),
            PropValue::Int64(v) => v.to_string(),
            PropValue::String(s) => trunc(s, max_chars),
            PropValue::SysTime(t) => format_unix_ms(*t),
            PropValue::Guid(g) => names::format_guid(g),
            PropValue::Binary(b) => hex(b, max_chars),
            PropValue::Object(h) => format!("object 0x{h:X}"),
            PropValue::MultiInt16(v) => join(v, |x| x.to_string()),
            PropValue::MultiInt32(v) => join(v, |x| x.to_string()),
            PropValue::MultiFloat32(v) => join(v, |x| x.to_string()),
            PropValue::MultiFloat64(v) | PropValue::MultiAppTime(v) => join(v, |x| x.to_string()),
            PropValue::MultiCurrency(v) | PropValue::MultiInt64(v) => join(v, |x| x.to_string()),
            PropValue::MultiString(v) => trunc(&v.join("; "), max_chars),
            PropValue::MultiSysTime(v) => join(v, |t| format_unix_ms(*t)),
            PropValue::MultiGuid(v) => join(v, names::format_guid),
            PropValue::MultiBinary(v) => join(v, |b| hex(b, 64)),
        }
    }
}

/// 100-ns intervals between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;

/// Windows FILETIME (100 ns since 1601, UTC) → UTC ms since the Unix epoch. Exact for every
/// `u64`: values past `i64::MAX` (e.g. "never" sentinels) do not wrap.
pub fn filetime_to_unix_ms(ft: u64) -> i64 {
    (ft / 10_000) as i64 - FILETIME_UNIX_EPOCH / 10_000
}

/// UTC ms since the Unix epoch → Windows FILETIME, clamped to the representable range.
pub fn unix_ms_to_filetime(ms: i64) -> u64 {
    ms.saturating_mul(10_000).saturating_add(FILETIME_UNIX_EPOCH).max(0) as u64
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` for UTC ms since the Unix epoch.
pub fn format_unix_ms(ms: i64) -> String {
    use mail_model::time::{civil_from_days, MS_PER_DAY};
    let rem = ms.rem_euclid(MS_PER_DAY);
    let (y, m, d) = civil_from_days(ms.div_euclid(MS_PER_DAY));
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_conversion() {
        // 2009-02-13T23:31:30Z
        let ft = unix_ms_to_filetime(1_234_567_890_000);
        assert_eq!(ft, 128_790_414_900_000_000);
        assert_eq!(filetime_to_unix_ms(ft), 1_234_567_890_000);
        assert_eq!(format_unix_ms(1_234_567_890_123), "2009-02-13T23:31:30.123Z");
        assert_eq!(format_unix_ms(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(filetime_to_unix_ms(0), -11_644_473_600_000);
        // Sentinels above i64::MAX stay far in the future instead of wrapping to the past.
        assert_eq!(filetime_to_unix_ms(u64::MAX), (u64::MAX / 10_000) as i64 - 11_644_473_600_000);
        assert!(filetime_to_unix_ms(1 << 63) > 0);
        assert_eq!(unix_ms_to_filetime(i64::MIN), 0);
    }

    #[test]
    fn value_display() {
        assert_eq!(PropValue::Currency(-12_345).display(10), "-1.2345");
        assert_eq!(PropValue::Binary(vec![1, 2, 255]).display(10), "0102FF");
        assert_eq!(PropValue::String("abcdef".into()).display(3), "abc… (6 chars)");
        assert_eq!(PropType::from_u16(0x001F).unwrap().name(), "PtypString");
    }
}
