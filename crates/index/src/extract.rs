//! Plain-text extraction for indexing: message bodies (text / HTML / RTF) and
//! attachment content (text, HTML, RTF, PDF, OOXML, ODF).
//!
//! Everything here is total: malformed input yields less (or no) text, never a panic
//! (the PDF parser is additionally isolated with `catch_unwind`).

use std::borrow::Cow;
use std::cell::Cell;
use std::io::{Cursor, Read};
use std::panic::AssertUnwindSafe;
use std::sync::Once;

use mail_model::{AttachmentKind, AttachmentMeta, Body};

/// Attachments larger than this are not fetched for text extraction.
pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
/// Extracted text kept per attachment.
pub const MAX_ATTACHMENT_TEXT: usize = 1024 * 1024;
/// Upper bound on one decompressed XML part of an Office/ODF zip (zip-bomb guard).
const MAX_XML_PART: u64 = 64 * 1024 * 1024;
/// Upper bound on all XML parts read from one Office/ODF zip together: parts that yield no
/// text never reach `MAX_ATTACHMENT_TEXT`, so the per-part cap alone would allow
/// gigabytes of inflation from one small file.
const MAX_XML_TOTAL: u64 = 128 * 1024 * 1024;
/// Upper bound on the decoded size of all streams of one PDF. lopdf decodes every stream
/// in full and without limit (an allocation failure aborts the process, which
/// `catch_unwind` cannot stop), so PDFs that would exceed this are not handed to it.
const MAX_PDF_DECODED: usize = 128 * 1024 * 1024;

/// Content types we can turn into text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Html,
    Rtf,
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    /// OpenDocument text / spreadsheet / presentation.
    Odf,
}

/// Best plain-text rendition of a body: `text`, else HTML stripped, else RTF stripped.
/// The result is truncated to `max_bytes`.
pub fn body_text(body: Body, max_bytes: usize) -> String {
    let mut out = match body {
        Body { text: Some(t), .. } if !t.trim().is_empty() => t,
        Body { html: Some(h), .. } if !h.trim().is_empty() => html_to_text(&h),
        Body { rtf: Some(r), .. } => rtf_to_text(&r),
        _ => String::new(),
    };
    crate::util::truncate_at_char(&mut out, max_bytes);
    out
}

/// RTF (bytes mapped 1:1 to chars, as in `Body::rtf`) → plain text, via the shared MAPI RTF reader.
pub fn rtf_to_text(rtf: &str) -> String {
    mapi_props::rtf::to_text(rtf)
}

/// Which extractor applies to an attachment, judged from its name and MIME type only
/// (so the bytes are fetched only when they can be used). `None` for embedded messages,
/// images and other binaries.
pub fn attachment_kind(meta: &AttachmentMeta) -> Option<Kind> {
    if !matches!(meta.kind, AttachmentKind::File) {
        return None;
    }
    let ext = meta.filename.as_deref().and_then(|n| n.rsplit_once('.')).map(|(_, e)| e.to_ascii_lowercase());
    if let Some(k) = ext.as_deref().and_then(kind_from_ext) {
        return Some(k);
    }
    let mime = meta.mime_type.as_deref().unwrap_or("").to_ascii_lowercase();
    let essence = mime.split(';').next().unwrap_or("").trim();
    kind_from_mime(essence)
}

fn kind_from_ext(ext: &str) -> Option<Kind> {
    Some(match ext {
        "txt" | "text" | "csv" | "tsv" | "log" | "md" | "markdown" | "json" | "xml" | "ics" | "vcs" | "vcf" | "ini"
        | "cfg" | "conf" | "yaml" | "yml" | "sql" | "eml_txt" => Kind::Text,
        "htm" | "html" | "xhtml" => Kind::Html,
        "rtf" => Kind::Rtf,
        "pdf" => Kind::Pdf,
        "docx" | "docm" | "dotx" | "dotm" => Kind::Docx,
        "xlsx" | "xlsm" | "xltx" | "xltm" => Kind::Xlsx,
        "pptx" | "pptm" | "ppsx" | "ppsm" | "potx" => Kind::Pptx,
        "odt" | "ods" | "odp" | "ott" | "ots" | "otp" => Kind::Odf,
        _ => return None,
    })
}

fn kind_from_mime(essence: &str) -> Option<Kind> {
    Some(match essence {
        "text/html" | "application/xhtml+xml" => Kind::Html,
        "text/rtf" | "application/rtf" | "text/richtext" => Kind::Rtf,
        "application/pdf" | "application/x-pdf" => Kind::Pdf,
        "application/json" | "application/xml" | "text/calendar" => Kind::Text,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.ms-word.document.macroenabled.12" => Kind::Docx,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.ms-excel.sheet.macroenabled.12" => Kind::Xlsx,
        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/vnd.ms-powerpoint.presentation.macroenabled.12" => Kind::Pptx,
        m if m.starts_with("application/vnd.oasis.opendocument.") => Kind::Odf,
        m if m.starts_with("text/") => Kind::Text,
        _ => return None,
    })
}

/// Extracts searchable text from attachment bytes. `None` when the type is unsupported,
/// the attachment is too large, or nothing could be extracted.
pub fn attachment_text(meta: &AttachmentMeta, bytes: &[u8]) -> Option<String> {
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return None;
    }
    let kind = attachment_kind(meta).or_else(|| sniff(bytes))?;
    let charset = meta.mime_type.as_deref().and_then(charset_param);
    let mut text = match kind {
        Kind::Text => decode_text(bytes, charset.as_deref()).into_owned(),
        Kind::Html => html_to_text(&decode_text(bytes, charset.as_deref())),
        Kind::Rtf => rtf_to_text(&mapi_props::rtf::latin1(bytes)),
        Kind::Pdf => pdf_text(bytes)?,
        Kind::Docx => ooxml_text(bytes, OfficeKind::Docx)?,
        Kind::Xlsx => ooxml_text(bytes, OfficeKind::Xlsx)?,
        Kind::Pptx => ooxml_text(bytes, OfficeKind::Pptx)?,
        Kind::Odf => ooxml_text(bytes, OfficeKind::Odf)?,
    };
    crate::util::truncate_at_char(&mut text, MAX_ATTACHMENT_TEXT);
    (!text.trim().is_empty()).then_some(text)
}

fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(b"%PDF-") {
        Some(Kind::Pdf)
    } else if bytes.starts_with(b"{\\rtf") {
        Some(Kind::Rtf)
    } else {
        None
    }
}

fn charset_param(mime: &str) -> Option<String> {
    mime.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Decodes bytes using a BOM, then the declared charset, then UTF-8, then Windows-1252.
pub fn decode_text<'a>(bytes: &'a [u8], charset: Option<&str>) -> Cow<'a, str> {
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return Cow::Owned(enc.decode_without_bom_handling(&bytes[bom_len..]).0.into_owned());
    }
    if let Some(enc) = charset.and_then(|c| encoding_rs::Encoding::for_label(c.as_bytes())) {
        if enc != encoding_rs::UTF_8 {
            return enc.decode_without_bom_handling(bytes).0;
        }
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0,
    }
}

// ---------------------------------------------------------------------------------------------
// HTML

/// Elements whose content is not rendered text.
const RAW_SKIP: &[&str] = &["script", "style", "noscript", "template", "xml"];
/// Elements that imply a line break.
const BLOCK: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "tr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "table",
    "blockquote",
    "hr",
    "ul",
    "ol",
    "pre",
    "section",
    "article",
    "header",
    "footer",
    "dt",
    "dd",
    "address",
    "center",
    "form",
    "title",
    "caption",
    "thead",
    "tbody",
    "tfoot",
];

/// Fast tag stripper: drops comments, `<script>`/`<style>` content and tags, decodes
/// entities, collapses whitespace, and turns block elements into line breaks.
pub fn html_to_text(html: &str) -> String {
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 3);
    let mut i = 0;
    while i < bytes.len() {
        let Some(off) = memchr(b'<', &bytes[i..]) else {
            push_html_text(&mut out, &html[i..]);
            break;
        };
        if off > 0 {
            push_html_text(&mut out, &html[i..i + off]);
        }
        i += off;
        let rest = &bytes[i..];
        if rest.starts_with(b"<!--") {
            i = find(&bytes[i + 4..], b"-->").map_or(bytes.len(), |p| i + 4 + p + 3);
            continue;
        }
        if rest.len() > 1 && (rest[1] == b'!' || rest[1] == b'?') {
            i = memchr(b'>', rest).map_or(bytes.len(), |p| i + p + 1);
            continue;
        }
        let mut j = i + 1;
        let closing = j < bytes.len() && bytes[j] == b'/';
        if closing {
            j += 1;
        }
        let name_start = j;
        while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b':' || bytes[j] == b'-') {
            j += 1;
        }
        if j == name_start {
            // A literal '<' in text.
            push_html_text(&mut out, "<");
            i += 1;
            continue;
        }
        let name = html[name_start..j].to_ascii_lowercase();
        let mut quote = 0u8;
        while j < bytes.len() {
            let b = bytes[j];
            if quote != 0 {
                if b == quote {
                    quote = 0;
                }
            } else if b == b'"' || b == b'\'' {
                quote = b;
            } else if b == b'>' {
                break;
            }
            j += 1;
        }
        let self_closing = j > 0 && j <= bytes.len() && bytes[j - 1] == b'/';
        i = (j + 1).min(bytes.len());
        if !closing && !self_closing && RAW_SKIP.contains(&name.as_str()) {
            let close = format!("</{name}");
            i = find_ci(&bytes[i..], close.as_bytes()).map_or(bytes.len(), |p| {
                let after = i + p;
                memchr(b'>', &bytes[after..]).map_or(bytes.len(), |q| after + q + 1)
            });
            continue;
        }
        if BLOCK.contains(&name.as_str()) {
            push_break(&mut out);
        } else if (name == "td" || name == "th") && !out.ends_with([' ', '\n']) && !out.is_empty() {
            out.push(' ');
        }
    }
    let trimmed = out.trim_end().len();
    out.truncate(trimmed);
    out
}

fn push_html_text(out: &mut String, raw: &str) {
    let decoded = html_escape::decode_html_entities(raw);
    for c in decoded.chars() {
        if c.is_whitespace() {
            if !out.is_empty() && !out.ends_with([' ', '\n']) {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
}

fn push_break(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

fn memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&b| b == needle)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w.eq_ignore_ascii_case(needle))
}

// ---------------------------------------------------------------------------------------------
// PDF

fn pdf_text(bytes: &[u8]) -> Option<String> {
    if !pdf_decodes_within(bytes, MAX_PDF_DECODED) {
        return None;
    }
    // pdf-extract (and lopdf below it) panics on plenty of real-world PDFs (missing font
    // maps, odd encodings); isolate it.
    quiet_catch_unwind(|| pdf_extract::extract_text_from_mem(bytes)).and_then(|r| r.ok())
}

/// Whether every stream of `pdf` decodes to at most `budget` bytes in total, as lopdf would
/// decode them (`FlateDecode`, `LZWDecode` and `ASCII85Decode`, chained in `/Filter` order;
/// other filters it leaves alone). Streams are located by their `stream` keyword and decoded
/// until the codec's own end (not `/Length` or `endstream`, which a crafted file can fake),
/// so the estimate errs high. Encrypted PDFs cannot be measured before decryption and are
/// rejected.
fn pdf_decodes_within(pdf: &[u8], budget: usize) -> bool {
    if find(pdf, b"/Encrypt").is_some() {
        return false;
    }
    let mut left = budget;
    let mut pos = 0;
    while let Some(off) = find(&pdf[pos..], b"stream") {
        let kw = pos + off;
        pos = kw + b"stream".len();
        if pdf[..kw].ends_with(b"end") {
            continue;
        }
        let mut data = &pdf[pos..];
        data = data.strip_prefix(b"\r").unwrap_or(data);
        data = data.strip_prefix(b"\n").unwrap_or(data);
        // The stream dictionary sits between the object header and the keyword.
        let lo = kw.saturating_sub(PDF_DICT_LOOKBACK);
        let dict_start = rfind(&pdf[lo..kw], b" obj").map_or(lo, |p| lo + p);
        let filters = pdf_filters(&pdf[dict_start..kw]);
        match decoded_len(data, &filters, left) {
            Some(n) => left -= n,
            None => return false,
        }
    }
    true
}

#[derive(Clone, Copy)]
enum PdfFilter {
    Flate,
    Lzw,
    Ascii85,
}

/// The decoding filters named in a stream dictionary, in order.
fn pdf_filters(dict: &[u8]) -> Vec<PdfFilter> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = memchr(b'/', &dict[i..]) {
        i += off + 1;
        let name_len = dict[i..].iter().position(|b| !b.is_ascii_alphanumeric()).unwrap_or(dict.len() - i);
        match &dict[i..i + name_len] {
            b"FlateDecode" => out.push(PdfFilter::Flate),
            b"LZWDecode" => out.push(PdfFilter::Lzw),
            b"ASCII85Decode" => out.push(PdfFilter::Ascii85),
            _ => {}
        }
    }
    out
}

/// Total bytes produced by decoding `data` through `filters` (every stage counts: lopdf
/// holds each in memory), or `None` once that exceeds `budget`.
fn decoded_len(data: &[u8], filters: &[PdfFilter], budget: usize) -> Option<usize> {
    let mut input = Cow::Borrowed(data);
    let mut total = 0usize;
    for f in filters {
        let limit = budget - total;
        let out = match f {
            PdfFilter::Flate => inflate_capped(&input, limit),
            PdfFilter::Lzw => lzw_capped(&input, limit),
            PdfFilter::Ascii85 => ascii85(&input),
        };
        if out.len() > limit {
            return None;
        }
        total += out.len();
        input = Cow::Owned(out);
    }
    Some(total)
}

/// zlib-inflates at most `limit + 1` bytes (the `+ 1` tells "over the limit" apart), with
/// lopdf's raw-deflate fallback for streams whose zlib header or checksum is broken.
fn inflate_capped(input: &[u8], limit: usize) -> Vec<u8> {
    let cap = (limit as u64).saturating_add(1);
    let mut out = Vec::new();
    let _ = flate2::read::ZlibDecoder::new(input).take(cap).read_to_end(&mut out);
    if out.is_empty() && input.len() > 2 {
        let _ = flate2::read::DeflateDecoder::new(&input[2..]).take(cap).read_to_end(&mut out);
    }
    out
}

/// LZW-decodes (PDF bit order, early change) at most `limit + 1` bytes.
fn lzw_capped(input: &[u8], limit: usize) -> Vec<u8> {
    let mut decoder = weezl::decode::Decoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8);
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut rest = input;
    while out.len() <= limit {
        let r = decoder.decode_bytes(rest, &mut chunk);
        rest = &rest[r.consumed_in..];
        out.extend_from_slice(&chunk[..r.consumed_out]);
        match r.status {
            Ok(weezl::LzwStatus::Ok) => {}
            _ => break,
        }
    }
    out
}

/// ASCII85 decoding (output is smaller than the input: no budget needed), up to `~>`.
fn ascii85(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 5 * 4);
    let (mut acc, mut n) = (0u32, 0);
    for &b in input {
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                acc = acc.wrapping_mul(85).wrapping_add(u32::from(b - b'!'));
                n += 1;
                if n == 5 {
                    out.extend_from_slice(&acc.to_be_bytes());
                    (acc, n) = (0, 0);
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for _ in n..5 {
            acc = acc.wrapping_mul(85).wrapping_add(84);
        }
        out.extend_from_slice(&acc.to_be_bytes()[..n - 1]);
    }
    out
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// How far before a `stream` keyword its dictionary is looked for (bounds the scan on
/// crafted files full of `stream` keywords).
const PDF_DICT_LOOKBACK: usize = 4096;

thread_local! {
    static QUIET_PANICS: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f`, turning a panic into `None` without the panic hook's report: an expected
/// third-party panic per malformed attachment would otherwise spam stderr. Installs (once,
/// process-wide) a hook that defers to the previous one except on a thread inside this call.
fn quiet_catch_unwind<T>(f: impl FnOnce() -> T) -> Option<T> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // `try_with`: the hook may run during thread-local teardown.
            if !QUIET_PANICS.try_with(Cell::get).unwrap_or(false) {
                previous(info);
            }
        }));
    });
    QUIET_PANICS.with(|q| q.set(true));
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    QUIET_PANICS.with(|q| q.set(false));
    result.ok()
}

// ---------------------------------------------------------------------------------------------
// Office Open XML / OpenDocument

#[derive(Clone, Copy, PartialEq, Eq)]
enum OfficeKind {
    Docx,
    Xlsx,
    Pptx,
    Odf,
}

fn ooxml_text(bytes: &[u8], kind: OfficeKind) -> Option<String> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut names: Vec<String> =
        zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).filter(|n| part_wanted(kind, n)).collect();
    // slide2 before slide10.
    names.sort_by(|a, b| (a.len(), a.as_str()).cmp(&(b.len(), b.as_str())));
    if kind == OfficeKind::Xlsx {
        // Shared strings first: they hold most of a workbook's text.
        names.sort_by_key(|n| n != "xl/sharedStrings.xml");
    }
    let mut out = String::new();
    let mut buf = Vec::new();
    let mut budget = MAX_XML_TOTAL;
    for name in names {
        if out.len() >= MAX_ATTACHMENT_TEXT || budget == 0 {
            break;
        }
        let Ok(entry) = zip.by_name(&name) else { continue };
        buf.clear();
        let read = entry.take(MAX_XML_PART.min(budget)).read_to_end(&mut buf);
        budget -= buf.len() as u64;
        if read.is_err() {
            continue;
        }
        let mode = match kind {
            OfficeKind::Xlsx if name.starts_with("xl/worksheets/") => XmlMode::Sheet,
            OfficeKind::Odf => XmlMode::All,
            _ => XmlMode::Tagged,
        };
        xml_text(&buf, mode, &mut out);
    }
    Some(out)
}

fn part_wanted(kind: OfficeKind, name: &str) -> bool {
    let xml = name.ends_with(".xml");
    match kind {
        OfficeKind::Docx => {
            xml && (name == "word/document.xml"
                || name == "word/footnotes.xml"
                || name == "word/endnotes.xml"
                || name.starts_with("word/header")
                || name.starts_with("word/footer"))
        }
        OfficeKind::Xlsx => name == "xl/sharedStrings.xml" || (xml && name.starts_with("xl/worksheets/sheet")),
        OfficeKind::Pptx => {
            xml && (name.starts_with("ppt/slides/slide") || name.starts_with("ppt/notesSlides/notesSlide"))
        }
        OfficeKind::Odf => name == "content.xml",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum XmlMode {
    /// OOXML: text lives in `<w:t>` / `<a:t>` / `<t>`; paragraphs end with `</w:p>` / `</a:p>` / `</si>`.
    Tagged,
    /// SpreadsheetML worksheet: inline `<t>` plus `<v>` of non-shared-string cells.
    Sheet,
    /// ODF: every text node; paragraphs `</text:p>`, `</text:h>`.
    All,
}

fn xml_text(xml: &[u8], mode: XmlMode, out: &mut String) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut capture = 0u32;
    let mut cell_is_shared = false;
    loop {
        if out.len() >= MAX_ATTACHMENT_TEXT {
            return;
        }
        let ev = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => return,
            Ok(ev) => ev,
        };
        match ev {
            Event::Start(e) => {
                let local = e.local_name();
                let name = local.as_ref();
                match mode {
                    XmlMode::Tagged if name == "t" => capture += 1,
                    XmlMode::Sheet => {
                        if name == "c" {
                            cell_is_shared =
                                e.try_get_attribute("t").ok().flatten().is_some_and(|a| a.value.as_ref() == "s");
                        } else if name == "t" || (name == "v" && !cell_is_shared) {
                            capture += 1;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let local = e.local_name();
                let name = local.as_ref();
                match mode {
                    XmlMode::Tagged | XmlMode::Sheet => {
                        if name == "t" || (mode == XmlMode::Sheet && name == "v" && capture > 0) {
                            capture = capture.saturating_sub(1);
                            if mode == XmlMode::Sheet {
                                out.push(' ');
                            }
                        } else if name == "p" || name == "si" || name == "row" {
                            push_line(out);
                        }
                    }
                    XmlMode::All => {
                        if name == "p" || name == "h" || name == "list-item" {
                            push_line(out);
                        }
                    }
                }
            }
            Event::Empty(e) => {
                let local = e.local_name();
                match local.as_ref() {
                    "tab" | "s" => out.push(' '),
                    "br" | "cr" | "line-break" => out.push('\n'),
                    _ => {}
                }
            }
            Event::Text(t) if capture > 0 || mode == XmlMode::All => {
                out.push_str(&t.xml10_content());
            }
            Event::CData(t) if capture > 0 || mode == XmlMode::All => {
                out.push_str(&t.xml10_content());
            }
            Event::GeneralRef(r) if capture > 0 || mode == XmlMode::All => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    out.push(c);
                } else {
                    match r.xml10_content().as_ref() {
                        "amp" => out.push('&'),
                        "lt" => out.push('<'),
                        "gt" => out.push('>'),
                        "quot" => out.push('"'),
                        "apos" => out.push('\''),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn push_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn meta(name: &str, mime: Option<&str>) -> AttachmentMeta {
        AttachmentMeta { filename: Some(name.into()), mime_type: mime.map(Into::into), ..Default::default() }
    }

    #[test]
    fn html_strips_tags_scripts_comments_and_entities() {
        let html = r#"<html><head><style>p { color: red }</style><title>T</title></head>
            <body><!-- hidden --><p>Hello&nbsp;<b>world</b> &amp; caf&eacute;</p>
            <script>var x = "<p>nope</p>";</script><div title="a>b">Line&#x32;</div>
            <!--[if mso]><p>mso only</p><![endif]-->1 &lt; 2<br>end</body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Hello world & café"), "{text}");
        assert!(text.contains("Line2"), "{text}");
        assert!(text.contains("1 < 2\nend"), "{text}");
        assert!(!text.contains("color"), "{text}");
        assert!(!text.contains("nope"), "{text}");
        assert!(!text.contains("mso only"), "{text}");
        assert!(!text.contains("hidden"), "{text}");
    }

    #[test]
    fn html_handles_truncated_and_odd_input() {
        assert_eq!(html_to_text("a < b"), "a < b");
        assert_eq!(html_to_text("<p>unterminated <b"), "unterminated");
        assert_eq!(html_to_text("x<style>never closed"), "x");
        assert_eq!(html_to_text("<!-- open comment"), "");
        assert_eq!(html_to_text(""), "");
        assert_eq!(html_to_text("<SCRIPT>a</ScRiPt>b"), "b");
    }

    #[test]
    fn body_precedence() {
        let b = Body { text: Some("plain".into()), html: Some("<p>html</p>".into()), rtf: None };
        assert_eq!(body_text(b, 100), "plain");
        let b = Body { text: Some("  ".into()), html: Some("<p>html</p>".into()), rtf: None };
        assert_eq!(body_text(b, 100), "html");
        let b = Body { text: None, html: None, rtf: Some(r"{\rtf1\ansi Hello\par World}".into()) };
        assert_eq!(body_text(b, 100).trim_end(), "Hello\nWorld");
        let b = Body { text: Some("abcdef".into()), ..Default::default() };
        assert_eq!(body_text(b, 3), "abc");
    }

    #[test]
    fn kinds() {
        assert_eq!(attachment_kind(&meta("a.PDF", None)), Some(Kind::Pdf));
        assert_eq!(attachment_kind(&meta("x", Some("text/plain; charset=utf-8"))), Some(Kind::Text));
        assert_eq!(attachment_kind(&meta("photo.jpg", Some("image/jpeg"))), None);
        assert_eq!(attachment_kind(&meta("r.docx", Some("application/octet-stream"))), Some(Kind::Docx));
        let mut m = meta("fwd.msg", None);
        m.kind = AttachmentKind::EmbeddedMessage;
        assert_eq!(attachment_kind(&m), None);
    }

    #[test]
    fn text_charsets() {
        let latin1 = b"caf\xe9";
        assert_eq!(attachment_text(&meta("a.txt", None), latin1).unwrap(), "café");
        let utf16 = [0xFF, 0xFE, b'h', 0, b'i', 0];
        assert_eq!(attachment_text(&meta("a.txt", None), &utf16).unwrap(), "hi");
        let koi8 = [0xF0, 0xD2, 0xC9];
        assert_eq!(attachment_text(&meta("a", Some("text/plain; charset=\"koi8-r\"")), &koi8).unwrap(), "При");
        assert!(attachment_text(&meta("a.txt", None), b"   ").is_none());
    }

    fn zip_bytes(files: &[(&str, &str)]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, body) in files {
            w.start_file(*name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn docx_text() {
        let doc = r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>Quarterly</w:t></w:r><w:r><w:tab/><w:t xml:space="preserve">revenue &amp; costs</w:t></w:r></w:p>
            <w:p><w:r><w:instrText>IGNORED</w:instrText><w:t>Second&#46;</w:t></w:r></w:p></w:body></w:document>"#;
        let bytes = zip_bytes(&[("[Content_Types].xml", "<Types/>"), ("word/document.xml", doc)]);
        let text = attachment_text(&meta("r.docx", None), &bytes).unwrap();
        assert_eq!(text, "Quarterly revenue & costs\nSecond.\n");
    }

    #[test]
    fn xlsx_text() {
        let shared = r#"<sst><si><t>Widget</t></si><si><r><t>Gad</t></r><r><t>get</t></r></si></sst>"#;
        let sheet = r#"<worksheet><sheetData><row><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42.5</v></c>
            <c r="C1" t="inlineStr"><is><t>inline note</t></is></c></row></sheetData></worksheet>"#;
        let bytes = zip_bytes(&[("xl/worksheets/sheet1.xml", sheet), ("xl/sharedStrings.xml", shared)]);
        let text = attachment_text(&meta("b.xlsx", None), &bytes).unwrap();
        assert!(text.starts_with("Widget\nGadget\n"), "{text:?}");
        assert!(text.contains("42.5"), "{text:?}");
        assert!(text.contains("inline note"), "{text:?}");
        assert!(!text.contains(" 0 "), "shared-string index leaked: {text:?}");
    }

    #[test]
    fn pptx_and_odt_text() {
        let s = |t: &str| format!(r#"<p:sld><p:txBody><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:txBody></p:sld>"#);
        let bytes = zip_bytes(&[
            ("ppt/slides/slide10.xml", &s("tenth")),
            ("ppt/slides/slide2.xml", &s("second")),
            ("ppt/slideLayouts/slideLayout1.xml", &s("layout")),
        ]);
        let text = attachment_text(&meta("d.pptx", None), &bytes).unwrap();
        assert_eq!(text, "second\ntenth\n");

        let content = r#"<office:document-content><office:body><office:text><text:h>Title</text:h>
            <text:p>Body<text:s/>text &#233;</text:p></office:text></office:body></office:document-content>"#;
        let bytes = zip_bytes(&[("content.xml", content), ("styles.xml", "<s>no</s>")]);
        let text = attachment_text(&meta("e.odt", None), &bytes).unwrap();
        assert!(text.contains("Title\n"), "{text:?}");
        assert!(text.contains("Body text é"), "{text:?}");
        assert!(!text.contains("no"), "{text:?}");
    }

    #[test]
    fn corrupt_containers_yield_none() {
        assert!(attachment_text(&meta("x.docx", None), b"PK\x03\x04garbage").is_none());
        assert!(attachment_text(&meta("x.pdf", None), b"%PDF-1.4\n1 0 obj <<").is_none());
        assert!(attachment_text(&meta("x.pdf", None), b"").is_none());
        let bytes = zip_bytes(&[("word/document.xml", "<w:t>unclosed <<<")]);
        // Text before the syntax error is kept; nothing after it, and no panic.
        let partial = attachment_text(&meta("x.docx", None), &bytes);
        assert_eq!(partial.as_deref().map(str::trim), Some("unclosed"));
    }

    /// Builds a minimal single-page PDF with correct xref offsets.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 712 Td ({text}) Tj ET");
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes(),
        );
        out
    }

    fn pdf_with_stream(filter: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("%PDF-1.4\n1 0 obj\n<< /Length {} /Filter {filter} >>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        v
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn pdf_stream_bombs_are_measured_before_decoding() {
        const MB: usize = 1 << 20;
        let zeros = vec![0u8; 16 * MB];
        let inner = zlib(&zeros);
        let single = pdf_with_stream("/FlateDecode", &inner);
        assert!(pdf_decodes_within(&single, 17 * MB));
        assert!(!pdf_decodes_within(&single, 15 * MB));
        // Chained filters: every stage counts.
        let chained = pdf_with_stream("[/FlateDecode /FlateDecode]", &zlib(&inner));
        assert!(!pdf_decodes_within(&chained, 16 * MB));
        assert!(pdf_decodes_within(&chained, 17 * MB));
        let lzw = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8).encode(&zeros).unwrap();
        assert!(!pdf_decodes_within(&pdf_with_stream("/LZWDecode", &lzw), 15 * MB));
        assert!(pdf_decodes_within(&pdf_with_stream("/LZWDecode", &lzw), 17 * MB));
        assert!(!pdf_decodes_within(b"%PDF-1.4\ntrailer << /Encrypt 5 0 R >>", usize::MAX));
        assert!(pdf_decodes_within(&tiny_pdf("ok"), MAX_PDF_DECODED));
        // Over budget: no text (and no allocation of the expanded stream).
        let meta = meta("bomb.pdf", None);
        let bomb = pdf_with_stream("[/FlateDecode /FlateDecode]", &zlib(&zlib(&vec![0u8; MAX_PDF_DECODED])));
        assert!(attachment_text(&meta, &bomb).is_none());
    }

    #[test]
    fn ascii85_decodes() {
        assert_eq!(ascii85(b"9jqo^z~>ignored"), b"Man \0\0\0\0");
    }

    #[test]
    fn quiet_catch_unwind_contains_panics() {
        assert_eq!(quiet_catch_unwind(|| 7), Some(7));
        assert_eq!(quiet_catch_unwind(|| -> u8 { panic!("expected test panic") }), None);
        assert!(!QUIET_PANICS.with(Cell::get));
    }

    #[test]
    fn pdf_text_extracted() {
        let pdf = tiny_pdf("Invoice number 12345");
        let text = attachment_text(&meta("inv.pdf", None), &pdf).unwrap();
        assert!(text.contains("Invoice number 12345"), "{text:?}");
        // Sniffed by magic when the name/type say nothing.
        let text = attachment_text(&meta("noext", None), &pdf).unwrap();
        assert!(text.contains("12345"));
    }
}
