//! Helpers shared by the MSG and TNEF decoders: little-endian readers, DTR dates, and the
//! deferred String8 decoding pass.

use mapi_props::{Prop, PropKey, PropMap, PropType, PropValue};

pub(crate) use mapi_props::filetime_to_unix_ms as filetime_to_ms;
pub(crate) use mapi_props::names::{PS_MAPI, PS_PUBLIC_STRINGS};
pub(crate) const IID_IMESSAGE: [u8; 16] = guid_ms(0x0002_0307);

/// `{XXXXXXXX-0000-0000-C000-000000000046}` in on-disk byte order.
const fn guid_ms(d1: u32) -> [u8; 16] {
    let b = d1.to_le_bytes();
    [b[0], b[1], b[2], b[3], 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]
}

// Property ids used by the decoders.
pub(crate) const PID_MESSAGE_CLASS: u16 = 0x001A;
pub(crate) const PID_IMPORTANCE: u16 = 0x0017;
pub(crate) const PID_SUBJECT: u16 = 0x0037;
pub(crate) const PID_CLIENT_SUBMIT_TIME: u16 = 0x0039;
pub(crate) const PID_SENT_REPRESENTING_NAME: u16 = 0x0042;
pub(crate) const PID_SENT_REPRESENTING_ADDRTYPE: u16 = 0x0064;
pub(crate) const PID_SENT_REPRESENTING_EMAIL: u16 = 0x0065;
pub(crate) const PID_ORIGINAL_MESSAGE_CLASS: u16 = 0x004B;
pub(crate) const PID_START_DATE: u16 = 0x0060;
pub(crate) const PID_END_DATE: u16 = 0x0061;
pub(crate) const PID_OWNER_APPT_ID: u16 = 0x0062;
pub(crate) const PID_RESPONSE_REQUESTED: u16 = 0x0063;
pub(crate) const PID_SENDER_NAME: u16 = 0x0C1A;
pub(crate) const PID_SENDER_ADDRTYPE: u16 = 0x0C1E;
pub(crate) const PID_SENDER_EMAIL: u16 = 0x0C1F;
pub(crate) const PID_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
pub(crate) const PID_MESSAGE_FLAGS: u16 = 0x0E07;
pub(crate) const PID_SEARCH_KEY: u16 = 0x300B;
pub(crate) const PID_BODY: u16 = 0x1000;
pub(crate) const PID_RTF_COMPRESSED: u16 = 0x1009;
pub(crate) const PID_BODY_HTML: u16 = 0x1013;
pub(crate) const PID_CREATION_TIME: u16 = 0x3007;
pub(crate) const PID_LAST_MODIFICATION_TIME: u16 = 0x3008;
pub(crate) const PID_ATTACH_SIZE: u16 = 0x0E20;
pub(crate) const PID_ATTACH_DATA: u16 = 0x3701;
pub(crate) const PID_ATTACH_FILENAME: u16 = 0x3704;
pub(crate) const PID_ATTACH_METHOD: u16 = 0x3705;
pub(crate) const PID_ATTACH_LONG_FILENAME: u16 = 0x3707;
pub(crate) const PID_ATTACH_RENDERING: u16 = 0x3709;
pub(crate) const PID_ATTACH_TAG: u16 = 0x370A;
pub(crate) const PID_RENDERING_POSITION: u16 = 0x370B;
pub(crate) const PID_ATTACH_TRANSPORT_NAME: u16 = 0x370C;
pub(crate) const PID_INTERNET_CODEPAGE: u16 = 0x3FDE;
pub(crate) const PID_MESSAGE_LOCALE_ID: u16 = 0x3FF1;
pub(crate) const PID_MESSAGE_CODEPAGE: u16 = 0x3FFD;

pub(crate) const ATTACH_BY_VALUE: i32 = 1;
pub(crate) const ATTACH_EMBEDDED_MSG: i32 = 5;
pub(crate) const ATTACH_OLE: i32 = 6;

/// Compound File Binary signature.
pub(crate) const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

#[inline]
pub(crate) fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}
#[inline]
pub(crate) fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}
#[inline]
pub(crate) fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// Civil date/time (UTC) → ms since the Unix epoch; `None` when out of range.
pub(crate) fn civil_to_ms(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<i64> {
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = mail_model::time::days_from_civil(y, mo, d);
    let secs = i64::from(h * 3600 + mi * 60 + s);
    Some(days * mail_model::time::MS_PER_DAY + secs * 1000)
}

/// UTF-16LE → String, stopping at the first NUL.
pub(crate) fn utf16_to_string(b: &[u8]) -> String {
    let units = b.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).take_while(|&u| u != 0);
    char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

/// 8-bit string bytes up to the first NUL, decoded with `codepage`.
pub(crate) fn string8_to_string(b: &[u8], codepage: Option<u32>) -> String {
    // UTF-16 "codepages" contain NUL bytes inside characters; the decoder trims the terminator.
    let end =
        if matches!(codepage, Some(1200 | 1201)) { b.len() } else { b.iter().position(|&c| c == 0).unwrap_or(b.len()) };
    mapi_props::codepage::decode_string8(&b[..end], codepage)
}

/// Readers store String8 values as raw bytes (`Binary` / `MultiBinary`) because the
/// codepage property may come later in the stream; this pass decodes them in place.
/// `PidTagHtml` holds the HTML body in the internet (body) codepage, not the property one.
pub(crate) fn decode_string8_props(map: &mut PropMap, codepage: Option<u32>) {
    let html_cp = int_prop(map, PID_INTERNET_CODEPAGE).and_then(|v| u32::try_from(v).ok()).filter(|&v| v != 0);
    for (k, p) in map.iter_mut() {
        match (p.ty, &mut p.value) {
            (PropType::String8, PropValue::Binary(b)) => {
                let cp = if *k == PropKey::Id(PID_BODY_HTML) { html_cp.or(codepage) } else { codepage };
                p.value = PropValue::String(string8_to_string(b, cp));
            }
            (PropType::MultiString8, PropValue::MultiBinary(v)) => {
                p.value = PropValue::MultiString(v.iter().map(|b| string8_to_string(b, codepage)).collect());
            }
            _ => {}
        }
    }
}

pub(crate) fn int_prop(map: &PropMap, id: u16) -> Option<i64> {
    match &map.get(&PropKey::Id(id))?.value {
        PropValue::Int16(v) => Some(*v as i64),
        PropValue::Int32(v) => Some(*v as i64),
        PropValue::Int64(v) => Some(*v),
        _ => None,
    }
}

/// Codepage used for the String8 values of a `.msg` object: `PidTagMessageCodepage`, else
/// the ANSI codepage of `PidTagMessageLocaleId` (the writer's locale), else
/// `PidTagInternetCodepage` (which describes the body, not the property strings), else
/// `inherited` from the parent message.
pub(crate) fn message_codepage(map: &PropMap, inherited: Option<u32>) -> Option<u32> {
    let valid = |v: i64| u32::try_from(v).ok().filter(|&v| v != 0);
    int_prop(map, PID_MESSAGE_CODEPAGE)
        .and_then(valid)
        .or_else(|| int_prop(map, PID_MESSAGE_LOCALE_ID).and_then(valid).map(lcid_ansi_codepage))
        .or_else(|| int_prop(map, PID_INTERNET_CODEPAGE).and_then(valid))
        .or(inherited)
}

/// Windows ANSI codepage for a locale id (by primary language, sublanguage where it matters).
pub(crate) fn lcid_ansi_codepage(lcid: u32) -> u32 {
    let lang = lcid & 0xFFFF;
    match lang & 0x3FF {
        0x04 => match lang {
            0x0404 | 0x0C04 | 0x1404 => 950,
            _ => 936,
        },
        0x11 => 932,
        0x12 => 949,
        0x1E => 874,
        0x2A => 1258,
        0x08 => 1253,
        0x0D => 1255,
        0x01 | 0x20 | 0x29 => 1256,
        0x25..=0x27 => 1257,
        0x1F => 1254,
        0x2C => {
            if lang == 0x082C {
                1251
            } else {
                1254
            }
        }
        0x43 => {
            if lang == 0x0843 {
                1251
            } else {
                1254
            }
        }
        0x02 | 0x19 | 0x22 | 0x23 | 0x2F | 0x3F | 0x40 | 0x44 | 0x50 | 0x28 | 0x45 | 0x6D => 1251,
        // Serbian/Bosnian Cyrillic sublanguages; Croatian and the Latin variants are 1250.
        0x1A => match lang {
            0x0C1A | 0x1C1A | 0x201A | 0x301A => 1251,
            _ => 1250,
        },
        0x05 | 0x0E | 0x15 | 0x18 | 0x1B | 0x24 | 0x1C => 1250,
        _ => 1252,
    }
}

pub(crate) fn set(map: &mut PropMap, id: u16, ty: PropType, value: PropValue) {
    map.insert(PropKey::Id(id), Prop { ty, value });
}

/// Decode one fixed-size scalar from its little-endian bytes. `b` must be long enough
/// for the type (8 bytes always suffices except for `Guid`).
pub(crate) fn decode_scalar(ty: PropType, b: &[u8]) -> Option<PropValue> {
    Some(match ty {
        PropType::Null => PropValue::Null,
        PropType::Int16 => PropValue::Int16(u16_at(b, 0)? as i16),
        PropType::Int32 => PropValue::Int32(u32_at(b, 0)? as i32),
        PropType::Float32 => PropValue::Float32(f32::from_bits(u32_at(b, 0)?)),
        PropType::Float64 => PropValue::Float64(f64::from_bits(u64_at(b, 0)?)),
        PropType::Currency => PropValue::Currency(u64_at(b, 0)? as i64),
        PropType::AppTime => PropValue::AppTime(f64::from_bits(u64_at(b, 0)?)),
        PropType::ErrorCode => PropValue::ErrorCode(u32_at(b, 0)?),
        PropType::Boolean => PropValue::Boolean(u16_at(b, 0)? != 0),
        PropType::Int64 => PropValue::Int64(u64_at(b, 0)? as i64),
        PropType::SysTime => PropValue::SysTime(filetime_to_ms(u64_at(b, 0)?)),
        PropType::Guid => PropValue::Guid(b.get(..16)?.try_into().ok()?),
        _ => return None,
    })
}

/// Size in bytes of one fixed-size value (as stored in multi-valued arrays).
pub(crate) fn scalar_size(ty: PropType) -> Option<usize> {
    use PropType::*;
    Some(match ty {
        Int16 | MultiInt16 => 2,
        Int32 | Float32 | ErrorCode | MultiInt32 | MultiFloat32 => 4,
        Boolean => 2,
        Float64 | Currency | AppTime | Int64 | SysTime | MultiFloat64 | MultiCurrency | MultiAppTime | MultiInt64
        | MultiSysTime => 8,
        Guid | MultiGuid => 16,
        _ => return None,
    })
}

/// Element type of a multi-valued fixed-size type.
pub(crate) fn mv_elem(ty: PropType) -> Option<PropType> {
    use PropType::*;
    Some(match ty {
        MultiInt16 => Int16,
        MultiInt32 => Int32,
        MultiFloat32 => Float32,
        MultiFloat64 => Float64,
        MultiCurrency => Currency,
        MultiAppTime => AppTime,
        MultiInt64 => Int64,
        MultiSysTime => SysTime,
        MultiGuid => Guid,
        _ => return None,
    })
}

/// Collect decoded scalars into the matching multi-valued `PropValue`.
pub(crate) fn collect_mv(ty: PropType, vals: Vec<PropValue>) -> Option<PropValue> {
    macro_rules! gather {
        ($variant:ident, $mv:ident) => {
            PropValue::$mv(
                vals.into_iter().filter_map(|v| if let PropValue::$variant(x) = v { Some(x) } else { None }).collect(),
            )
        };
    }
    Some(match ty {
        PropType::MultiInt16 => gather!(Int16, MultiInt16),
        PropType::MultiInt32 => gather!(Int32, MultiInt32),
        PropType::MultiFloat32 => gather!(Float32, MultiFloat32),
        PropType::MultiFloat64 => gather!(Float64, MultiFloat64),
        PropType::MultiCurrency => gather!(Currency, MultiCurrency),
        PropType::MultiAppTime => gather!(AppTime, MultiAppTime),
        PropType::MultiInt64 => gather!(Int64, MultiInt64),
        PropType::MultiSysTime => gather!(SysTime, MultiSysTime),
        PropType::MultiGuid => gather!(Guid, MultiGuid),
        _ => return None,
    })
}

/// Decode a packed array of fixed-size values (MSG multi-valued fixed streams).
pub(crate) fn decode_mv_fixed(ty: PropType, b: &[u8]) -> Option<PropValue> {
    let elem = mv_elem(ty)?;
    let size = scalar_size(ty)?;
    let vals = b.chunks_exact(size).filter_map(|c| decode_scalar(elem, c)).collect();
    collect_mv(ty, vals)
}

/// A decoded message whose attachment payloads were left out of the property maps.
pub(crate) struct Decoded {
    pub mapi: mapi_props::MapiMessage,
    /// Byte length of each attachment's payload (same order as `mapi.attachments`), when known.
    pub payload_sizes: Vec<Option<u64>>,
    /// Size of the container holding the message (file, TNEF stream or embedded storage);
    /// reported when the message has no `PidTagMessageSize`, which `.msg` files never store.
    pub size: u64,
}

impl Decoded {
    /// Drop `PidTagAttachDataBinary`/`Object` bytes from a fully decoded message of `size`
    /// bytes, keeping their lengths.
    pub fn strip(mut m: mapi_props::MapiMessage, size: u64) -> Self {
        let payload_sizes = m
            .attachments
            .iter_mut()
            .map(|a| {
                a.embedded = None;
                match a.props.remove(&PropKey::Id(PID_ATTACH_DATA)).map(|p| p.value) {
                    Some(PropValue::Binary(b)) => Some(b.len() as u64),
                    _ => None,
                }
            })
            .collect();
        Decoded { mapi: m, payload_sizes, size }
    }

    /// List header (no attachment details needed).
    pub fn to_header(
        &self,
        locator: Option<mail_model::Locator>,
        folder: Option<mail_model::FolderId>,
    ) -> mail_model::Header {
        let mut h = mapi_props::convert::header_from_mapi(&self.mapi, locator, folder);
        if h.size == 0 {
            h.size = self.size;
        }
        h
    }

    /// Convert to the shared model. Attachment sizes report the payload length (what a user
    /// saves to disk) rather than `PidTagAttachSize`, which includes property overhead.
    pub fn to_message(
        &self,
        locator: Option<mail_model::Locator>,
        folder: Option<mail_model::FolderId>,
    ) -> mail_model::Message {
        let mut msg = mapi_props::convert::message_from_mapi(&self.mapi, locator, folder);
        if msg.header.size == 0 {
            msg.header.size = self.size;
        }
        for a in &mut msg.attachments {
            if let Some(Some(len)) = self.payload_sizes.get(a.index as usize) {
                a.size = *len;
            }
        }
        msg
    }
}

/// 64-bit FNV-1a, used for fingerprints (stable across runs and platforms).
pub(crate) struct Fnv(pub u64);
impl Fnv {
    pub fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_to_ms(1970, 1, 1, 0, 0, 0), Some(0));
        assert_eq!(civil_to_ms(2010, 4, 28, 12, 40, 56), Some(1_272_458_456_000));
        assert_eq!(civil_to_ms(2000, 13, 1, 0, 0, 0), None);
    }

    #[test]
    fn utf16_stops_at_nul() {
        assert_eq!(utf16_to_string(&[b'h', 0, b'i', 0, 0, 0, b'x', 0]), "hi");
        // odd trailing byte ignored
        assert_eq!(utf16_to_string(&[b'a', 0, b'b']), "a");
    }
}
