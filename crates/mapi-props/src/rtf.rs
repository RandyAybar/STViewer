//! Compressed RTF ([MS-OXRTFCP]), HTML de-encapsulation from RTF ([MS-OXRTFEX]) and
//! RTF → plain text.
//!
//! RTF is a byte format; [`decompress`] returns raw bytes. [`latin1`] maps those bytes 1:1
//! onto `char`s so they can travel as `&str` (`Body::rtf`) without loss; [`extract_html`] and
//! [`to_text`] accept such strings (chars above U+00FF are passed through as text).

use mail_model::{Error, Result};

use crate::codepage;

const COMPRESSED: u32 = 0x7546_5A4C; // "LZFu"
const UNCOMPRESSED: u32 = 0x414C_454D; // "MELA"

/// Pre-loaded LZ dictionary. [MS-OXRTFCP] 2.1.2.1
const INITIAL_DICTIONARY: &[u8; 207] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

/// Decompress `PidTagRtfCompressed`. Lenient: a CRC mismatch or a truncated stream yields
/// whatever decompressed cleanly; only an unusable header is an error.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 16 {
        return Err(Error::Format(format!("compressed RTF header truncated ({} bytes)", data.len())));
    }
    let rd = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    let comp_size = rd(0) as usize;
    let raw_size = rd(4) as usize;
    let magic = rd(8);
    // COMPSIZE counts everything after itself.
    let end = comp_size.saturating_add(4).min(data.len());
    let body = &data[16..end.max(16)];
    match magic {
        UNCOMPRESSED => Ok(body[..raw_size.min(body.len())].to_vec()),
        COMPRESSED => {
            let mut dict = [0u8; 4096];
            dict[..INITIAL_DICTIONARY.len()].copy_from_slice(INITIAL_DICTIONARY);
            let mut wp = INITIAL_DICTIONARY.len();
            let mut out = Vec::with_capacity(raw_size.min(body.len().saturating_mul(8)));
            let mut i = 0;
            'outer: while i < body.len() {
                let control = body[i];
                i += 1;
                for bit in 0..8 {
                    if control & (1 << bit) == 0 {
                        let Some(&b) = body.get(i) else { break 'outer };
                        i += 1;
                        out.push(b);
                        dict[wp] = b;
                        wp = (wp + 1) & 0xFFF;
                    } else {
                        let (Some(&hi), Some(&lo)) = (body.get(i), body.get(i + 1)) else {
                            break 'outer;
                        };
                        i += 2;
                        let v = u16::from_be_bytes([hi, lo]) as usize;
                        let offset = v >> 4;
                        if offset == wp {
                            break 'outer; // end-of-stream marker
                        }
                        let len = (v & 0xF) + 2;
                        for k in 0..len {
                            let b = dict[(offset + k) & 0xFFF];
                            out.push(b);
                            dict[wp] = b;
                            wp = (wp + 1) & 0xFFF;
                        }
                    }
                }
            }
            if raw_size > 0 && out.len() > raw_size {
                out.truncate(raw_size);
            }
            Ok(out)
        }
        other => Err(Error::Format(format!("unknown compressed RTF type 0x{other:08X}"))),
    }
}

/// Map raw RTF bytes 1:1 onto chars (ISO-8859-1) so they can be stored as a `String` losslessly.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// True if the RTF declares HTML encapsulation (`\fromhtml1`).
pub fn is_html_encapsulated(rtf: &str) -> bool {
    let head = &rtf.as_bytes()[..rtf.len().min(2048)];
    memfind(head, b"\\fromhtml1")
}

/// De-encapsulate HTML from RTF produced by `\fromhtml1` ([MS-OXRTFEX] 2.4.2).
/// Returns `None` if the RTF does not encapsulate HTML.
pub fn extract_html(rtf: &str) -> Option<String> {
    if !is_html_encapsulated(rtf) {
        return None;
    }
    Some(Walker::new(Mode::Html).run(rtf))
}

/// Visible text of an RTF document (fields' results kept, destinations such as font
/// tables, pictures and HTML tags dropped).
pub fn to_text(rtf: &str) -> String {
    Walker::new(Mode::Text).run(rtf)
}

fn memfind(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Html,
    Text,
}

#[derive(Clone, Copy)]
struct GroupState {
    skip: bool,
    font_table: bool,
    htmlrtf: bool,
    in_tag: bool,
    uc: u32,
    cp: u32,
}

/// Destinations whose content is never rendered.
const SKIP_DESTINATIONS: &[&str] = &[
    "aftncn",
    "aftnsep",
    "aftnsepc",
    "author",
    "bkmkend",
    "bkmkstart",
    "colorschememapping",
    "colortbl",
    "comment",
    "datastore",
    "fldinst",
    "filetbl",
    "footer",
    "footerf",
    "footerl",
    "footerr",
    "footnote",
    "ftncn",
    "ftnsep",
    "ftnsepc",
    "generator",
    "header",
    "headerf",
    "headerl",
    "headerr",
    "info",
    "keywords",
    "latentstyles",
    "listoverridetable",
    "listtable",
    "mmathPr",
    "nonshppict",
    "object",
    "objdata",
    "operator",
    "pgdsctbl",
    "pict",
    "private",
    "revtbl",
    "rsidtbl",
    "shpinst",
    "stylesheet",
    "subject",
    "tc",
    "themedata",
    "title",
    "wgrffmtfilter",
    "xe",
    "xmlnstbl",
];

struct Walker {
    mode: Mode,
    cur: GroupState,
    stack: Vec<GroupState>,
    out: String,
    pending: Vec<u8>,
    pending_cp: u32,
    fallback_skip: u32,
    at_group_start: bool,
    star: bool,
    default_cp: u32,
    fonts: Vec<(i32, u32)>,
    font_def: Option<i32>,
}

impl Walker {
    fn new(mode: Mode) -> Self {
        Walker {
            mode,
            cur: GroupState {
                skip: false,
                font_table: false,
                htmlrtf: false,
                in_tag: false,
                uc: 1,
                cp: codepage::DEFAULT_CODEPAGE,
            },
            stack: Vec::new(),
            out: String::new(),
            pending: Vec::new(),
            pending_cp: codepage::DEFAULT_CODEPAGE,
            fallback_skip: 0,
            at_group_start: false,
            star: false,
            default_cp: codepage::DEFAULT_CODEPAGE,
            fonts: Vec::new(),
            font_def: None,
        }
    }

    fn visible(&self) -> bool {
        !self.cur.skip
            && match self.mode {
                Mode::Html => self.cur.in_tag || !self.cur.htmlrtf,
                Mode::Text => true,
            }
    }

    fn flush(&mut self) {
        if !self.pending.is_empty() {
            let s = codepage::decode_string8(&self.pending, Some(self.pending_cp));
            self.out.push_str(&s);
            self.pending.clear();
        }
    }

    fn emit(&mut self, s: &str) {
        if self.visible() {
            self.flush();
            self.out.push_str(s);
        }
    }

    fn emit_char(&mut self, c: char) {
        if self.visible() {
            self.flush();
            self.out.push(c);
        }
    }

    fn emit_byte(&mut self, b: u8) {
        if self.visible() {
            if !self.pending.is_empty() && self.pending_cp != self.cur.cp {
                self.flush();
            }
            self.pending_cp = self.cur.cp;
            self.pending.push(b);
        }
    }

    fn newline(&mut self) {
        match self.mode {
            Mode::Html => self.emit("\r\n"),
            Mode::Text => self.emit_char('\n'),
        }
    }

    /// Consume one fallback character after `\uN`; returns true if the token must be dropped.
    fn eat_fallback(&mut self) -> bool {
        if self.fallback_skip > 0 {
            self.fallback_skip -= 1;
            true
        } else {
            false
        }
    }

    fn run(mut self, rtf: &str) -> String {
        let mut it = rtf.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '{' => {
                    self.flush();
                    self.fallback_skip = 0;
                    self.stack.push(self.cur);
                    self.at_group_start = true;
                    self.star = false;
                    continue;
                }
                '}' => {
                    self.flush();
                    self.fallback_skip = 0;
                    if self.cur.font_table {
                        self.font_def = None;
                    }
                    if let Some(prev) = self.stack.pop() {
                        self.cur = prev;
                    }
                }
                '\\' => {
                    let Some(&n) = it.peek() else { break };
                    if n.is_ascii_alphabetic() {
                        let mut word = String::new();
                        while let Some(&ch) = it.peek() {
                            if ch.is_ascii_alphabetic() && word.len() < 32 {
                                word.push(ch);
                                it.next();
                            } else {
                                break;
                            }
                        }
                        let mut param: Option<i32> = None;
                        let neg = it.peek() == Some(&'-');
                        if neg {
                            it.next();
                        }
                        let mut digits = 0;
                        let mut v: i64 = 0;
                        while let Some(&ch) = it.peek() {
                            if let Some(d) = ch.to_digit(10) {
                                if digits < 10 {
                                    v = v * 10 + d as i64;
                                }
                                digits += 1;
                                it.next();
                            } else {
                                break;
                            }
                        }
                        if digits > 0 {
                            let v = if neg { -v } else { v };
                            param = Some(v.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
                        }
                        if it.peek() == Some(&' ') {
                            it.next();
                        }
                        if word == "bin" {
                            for _ in 0..param.unwrap_or(0).max(0) {
                                if it.next().is_none() {
                                    break;
                                }
                            }
                        } else {
                            self.control_word(&word, param);
                        }
                    } else {
                        it.next();
                        match n {
                            '\'' => {
                                let h = it.next().and_then(|c| c.to_digit(16));
                                let l = it.next().and_then(|c| c.to_digit(16));
                                if let (Some(h), Some(l)) = (h, l) {
                                    if !self.eat_fallback() {
                                        self.emit_byte((h * 16 + l) as u8);
                                    }
                                }
                            }
                            '*' => {
                                if self.at_group_start {
                                    self.star = true;
                                    continue; // still at group start for the next control word
                                }
                            }
                            '\\' | '{' | '}' => {
                                if !self.eat_fallback() {
                                    self.emit_char(n);
                                }
                            }
                            '~' => self.emit_char('\u{A0}'),
                            '_' => self.emit_char('\u{2011}'),
                            '\n' | '\r' => self.newline(),
                            _ => {
                                self.eat_fallback();
                            }
                        }
                    }
                }
                // Insignificant (keeps "at group start" state); NULs pad the decompressed stream.
                '\r' | '\n' | '\0' => continue,
                _ => {
                    if self.at_group_start && self.star {
                        // `{\*` followed by text: unknown destination, ignore its content.
                        self.cur.skip = true;
                    }
                    if !self.eat_fallback() {
                        if (c as u32) < 0x100 {
                            if c.is_ascii() {
                                self.emit_char(c);
                            } else {
                                self.emit_byte(c as u8);
                            }
                        } else {
                            self.emit_char(c);
                        }
                    }
                }
            }
            self.at_group_start = false;
            self.star = false;
        }
        self.flush();
        self.out
    }

    fn control_word(&mut self, word: &str, param: Option<i32>) {
        if self.at_group_start {
            self.at_group_start = false;
            let star = std::mem::take(&mut self.star);
            if star {
                match (self.mode, word) {
                    (Mode::Html, "htmltag") => {
                        self.flush();
                        self.cur.in_tag = true;
                    }
                    _ => self.cur.skip = true,
                }
                return;
            }
            if word == "fonttbl" {
                self.cur.font_table = true;
                self.cur.skip = true;
                return;
            }
            if SKIP_DESTINATIONS.contains(&word) {
                self.cur.skip = true;
                return;
            }
        }
        if self.cur.font_table {
            match word {
                "f" => self.font_def = param,
                "fcharset" => {
                    if let (Some(f), Some(cp)) =
                        (self.font_def, param.and_then(|c| codepage::codepage_for_rtf_charset(c as u32)))
                    {
                        self.set_font_cp(f, cp);
                    }
                }
                "cpg" => {
                    if let (Some(f), Some(cp)) = (self.font_def, param) {
                        self.set_font_cp(f, cp as u32);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.eat_fallback() {
            return;
        }
        match word {
            "par" | "line" | "sect" | "page" => self.newline(),
            "row" => match self.mode {
                Mode::Text => self.emit_char('\n'),
                Mode::Html => {}
            },
            "tab" => self.emit_char('\t'),
            "cell" => match self.mode {
                Mode::Text => self.emit_char('\t'),
                Mode::Html => {}
            },
            "lquote" => self.emit_char('\u{2018}'),
            "rquote" => self.emit_char('\u{2019}'),
            "ldblquote" => self.emit_char('\u{201C}'),
            "rdblquote" => self.emit_char('\u{201D}'),
            "bullet" => self.emit_char('\u{2022}'),
            "endash" => self.emit_char('\u{2013}'),
            "emdash" => self.emit_char('\u{2014}'),
            "enspace" | "emspace" | "qmspace" => self.emit_char(' '),
            "u" => {
                if let Some(p) = param {
                    let unit = p as i16 as u16 as u32;
                    if let Some(ch) = char::from_u32(unit) {
                        self.emit_char(ch);
                    }
                    self.fallback_skip = self.cur.uc;
                }
            }
            "uc" => self.cur.uc = param.unwrap_or(1).clamp(0, 16) as u32,
            "ansicpg" => {
                if let Some(cp) = param.filter(|&cp| cp > 0) {
                    self.default_cp = cp as u32;
                    self.cur.cp = cp as u32;
                }
            }
            "f" => {
                let f = param.unwrap_or(0);
                self.cur.cp = self.fonts.iter().find(|e| e.0 == f).map(|e| e.1).unwrap_or(self.default_cp);
            }
            "htmlrtf" if self.mode == Mode::Html => {
                self.flush();
                self.cur.htmlrtf = param != Some(0);
            }
            _ => {}
        }
    }

    fn set_font_cp(&mut self, font: i32, cp: u32) {
        match self.fonts.iter_mut().find(|e| e.0 == font) {
            Some(e) => e.1 = cp,
            None => self.fonts.push((font, cp)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compress_with_lib(rtf: &str) -> Vec<u8> {
        compressed_rtf::compress_rtf(rtf).unwrap()
    }

    #[test]
    fn decompress_spec_example() {
        // [MS-OXRTFCP] 3.1.1 "{\rtf1\ansi\ansicpg1252\pard hello world}\r\n"
        let comp: &[u8] = &[
            0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5, 0xc7, 0xa7, 0x03, 0x00,
            0x0a, 0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42, 0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09,
            0x00, 0x20, 0x62, 0x77, 0x05, 0xb0, 0x6c, 0x64, 0x7d, 0x0a, 0x80, 0x0f, 0xa0,
        ];
        let out = decompress(comp).unwrap();
        assert_eq!(out, b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
    }

    #[test]
    fn decompress_roundtrip_and_uncompressed() {
        let rtf = "{\\rtf1\\ansi\\ansicpg1252\\fromtext \\deff0{\\fonttbl{\\f0 Arial;}}\\f0 Hello \\par world, hello world, hello world!}";
        assert_eq!(decompress(&compress_with_lib(rtf)).unwrap(), rtf.as_bytes());
        let mut mela = Vec::new();
        mela.extend_from_slice(&(12u32 + 5).to_le_bytes());
        mela.extend_from_slice(&5u32.to_le_bytes());
        mela.extend_from_slice(&UNCOMPRESSED.to_le_bytes());
        mela.extend_from_slice(&0u32.to_le_bytes());
        mela.extend_from_slice(b"{\\a}");
        mela.push(b'x');
        assert_eq!(decompress(&mela).unwrap(), b"{\\a}x");
    }

    #[test]
    fn decompress_corrupt_never_panics() {
        assert!(matches!(decompress(b"short"), Err(Error::Format(_))));
        let mut bad = vec![0xFFu8; 64];
        bad[8..12].copy_from_slice(&COMPRESSED.to_le_bytes());
        let _ = decompress(&bad).unwrap();
        bad[8..12].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
        assert!(decompress(&bad).is_err());
        // Truncated compressed stream returns the prefix.
        let full = compress_with_lib("{\\rtf1 abcdefghijklmnopqrstuvwxyz}");
        let _ = decompress(&full[..full.len() - 3]).unwrap();
    }

    #[test]
    fn html_deencapsulation() {
        let rtf = r#"{\rtf1\ansi\ansicpg1252\fromhtml1 \deff0{\fonttbl{\f0\fswiss Arial;}{\f1\fnil\fcharset128 MS Gothic;}}
{\colortbl\red0\green0\blue0;}
\uc1\pard\plain\f0\fs20{\*\htmltag19 <html>}{\*\htmltag34 <head>}{\*\htmltag161 <title>}\htmlrtf {\htmlrtf0 T&amp;itle\htmlrtf }\htmlrtf0 {\*\htmltag169 </title>}
{\*\htmltag50 <body>}\htmlrtf \lang1033 \htmlrtf0 caf\'e9 \u8364? {\*\htmltag84 <a href="cid:x@y">}\htmlrtf {\htmlrtf0 link\htmlrtf }\htmlrtf0 {\*\htmltag92 </a>}\htmlrtf\par\htmlrtf0
{\*\mhtmltag84 <img src="orig">}{\*\htmltag84 <img src="cid:img">}{\f1 \'82\'a0}{\*\htmltag58 </body>}{\*\htmltag27 </html>}}"#;
        let html = extract_html(rtf).unwrap();
        assert_eq!(
            html,
            r#"<html><head><title>T&amp;itle</title><body>café € <a href="cid:x@y">link</a><img src="cid:img">あ</body></html>"#
        );
        assert!(extract_html(r"{\rtf1\ansi\fromtext hi}").is_none());
    }

    #[test]
    fn plain_text_extraction() {
        let rtf = r#"{\rtf1\ansi\ansicpg1251\deff0{\fonttbl{\f0 Arial;}}{\colortbl;\red255\green0\blue0;}{\*\generator x;}\pard Line\'e0 one\par Second\tab col {\field{\*\fldinst HYPERLINK "http://x"}{\fldrslt link}}\par \ldblquote q\rdblquote  \u-3913?\bin3 abc end\{\}}"#;
        assert_eq!(to_text(rtf), "Lineа one\nSecond\tcol link\n“q” \u{F0B7} end{}");
    }

    #[test]
    fn nul_padding_is_no_text() {
        assert_eq!(to_text("{\\rtf1\\ansi{\\fonttbl{\\f0 Arial;}}\\pard\\par}}\0"), "\n");
        assert_eq!(to_text("{\\rtf1 a\0b}"), "ab");
    }
}
