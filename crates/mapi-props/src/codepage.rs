//! Windows codepage → UTF-8 decoding for PtypString8 values and RTF `\'hh` escapes.

use encoding_rs::Encoding;

/// Codepage used when a container does not declare one (Western European).
pub const DEFAULT_CODEPAGE: u32 = 1252;

/// encoding_rs encoding for a Windows codepage number, if supported.
pub fn encoding_for_codepage(cp: u32) -> Option<&'static Encoding> {
    use encoding_rs::*;
    Some(match cp {
        65001 => UTF_8,
        1200 => UTF_16LE,
        1201 => UTF_16BE,
        20127 | 1252 | 28591 => WINDOWS_1252,
        874 => WINDOWS_874,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        932 => SHIFT_JIS,
        936 => GBK,
        949 | 51949 => EUC_KR,
        950 => BIG5,
        54936 => GB18030,
        51932 | 20932 => EUC_JP,
        50220..=50222 => ISO_2022_JP,
        866 => IBM866,
        20866 => KOI8_R,
        21866 => KOI8_U,
        10000 => MACINTOSH,
        10007 => X_MAC_CYRILLIC,
        28592 => ISO_8859_2,
        28593 => ISO_8859_3,
        28594 => ISO_8859_4,
        28595 => ISO_8859_5,
        28596 => ISO_8859_6,
        28597 => ISO_8859_7,
        28598 => ISO_8859_8,
        38598 => ISO_8859_8_I,
        28600 => ISO_8859_10,
        28603 => ISO_8859_13,
        28604 => ISO_8859_14,
        28605 => ISO_8859_15,
        28606 => ISO_8859_16,
        _ => return None,
    })
}

/// Windows codepage for an RTF `\fcharsetN` value. [MS-OXRTFEX] / RTF 1.9.1 spec.
pub fn codepage_for_rtf_charset(charset: u32) -> Option<u32> {
    Some(match charset {
        0 => 1252,
        77 => 10000,
        128 => 932,
        129 => 949,
        130 => 1361,
        134 => 936,
        136 => 950,
        161 => 1253,
        162 => 1254,
        163 => 1258,
        177 => 1255,
        178 => 1256,
        186 => 1257,
        204 => 1251,
        222 => 874,
        238 => 1250,
        254 => 437,
        _ => return None,
    })
}

/// Decode a PtypString8 value. Trailing NULs are dropped; unknown codepages fall back to
/// Windows-1252; malformed sequences become U+FFFD (never fails).
pub fn decode_string8(bytes: &[u8], codepage: Option<u32>) -> String {
    let enc = codepage.and_then(encoding_for_codepage).unwrap_or(encoding_rs::WINDOWS_1252);
    // UTF-16 code units end in a zero byte ("A\0"); trim whole NUL units only.
    let unit = if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE { 2 } else { 1 };
    // encoding_rs returns ASCII-only input borrowed (after a SIMD scan), so the common
    // plain-ASCII case costs one scan and one copy.
    enc.decode_without_bom_handling(trim_trailing_nul(bytes, unit)).0.into_owned()
}

/// Decode UTF-16LE (PtypString). Trailing NULs dropped; odd trailing byte ignored; lossy.
pub fn decode_utf16le(bytes: &[u8]) -> String {
    let bytes = trim_trailing_nul(bytes, 2);
    let units = bytes.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c));
    char::decode_utf16(units).map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
}

fn trim_trailing_nul(mut b: &[u8], unit: usize) -> &[u8] {
    if unit == 2 {
        b = &b[..b.len() & !1];
        while b.len() >= 2 && b[b.len() - 2] == 0 && b[b.len() - 1] == 0 {
            b = &b[..b.len() - 2];
        }
    } else {
        while let [rest @ .., 0] = b {
            b = rest;
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_common_codepages() {
        assert_eq!(decode_string8(b"caf\xe9\0\0", None), "café");
        assert_eq!(decode_string8(b"\x80", Some(1252)), "€");
        assert_eq!(decode_string8(b"\xcf\xf0\xe8\xe2\xe5\xf2", Some(1251)), "Привет");
        assert_eq!(decode_string8(b"\x82\xa0", Some(932)), "あ");
        assert_eq!(decode_string8("ü".as_bytes(), Some(65001)), "ü");
        assert_eq!(decode_string8(b"plain", Some(99999)), "plain");
        // Codepage 1200: the final code unit's zero byte is not a NUL terminator.
        assert_eq!(decode_string8(b"A\0B\0", Some(1200)), "AB");
        assert_eq!(decode_string8(b"A\0B\0\0\0", Some(1200)), "AB");
        assert_eq!(decode_string8(b"\0A\0B", Some(1201)), "AB");
    }

    #[test]
    fn utf16_trailing_nul_and_odd_bytes() {
        assert_eq!(decode_utf16le(b"h\0i\0\0\0"), "hi");
        assert_eq!(decode_utf16le(b"h\0i\0x"), "hi");
        // Unpaired surrogate → replacement char, not a panic.
        assert_eq!(decode_utf16le(&[0x00, 0xD8, b'a', 0]), "\u{FFFD}a");
    }

    #[test]
    fn rtf_charsets() {
        assert_eq!(codepage_for_rtf_charset(128), Some(932));
        assert_eq!(codepage_for_rtf_charset(2), None);
    }
}
