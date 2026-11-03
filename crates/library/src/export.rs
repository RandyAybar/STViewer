//! Export of messages to EML / mbox / HTML / text / CSV / attachment files, and of
//! contacts, appointments and tasks to vCard / iCalendar.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use base64::Engine as _;
use mail_model::{AttachmentKind, AttachmentMeta, Folder, FolderId, Header, Locator, Message, Result, SourceId};

use crate::fsname::NameAllocator;
use crate::text::{body_text, format_address, format_addresses, html_escape};
use crate::{cards, dates, embedded, htmlsafe, mime, ExportFormat, ExportReport, Library};

pub(crate) fn export(
    lib: &Library,
    items: &[(SourceId, Locator)],
    format: ExportFormat,
    dest: &Path,
) -> Result<ExportReport> {
    std::fs::create_dir_all(dest)?;
    let mut names = NameAllocator::new(dest);
    let mut report = ExportReport::default();
    match format {
        ExportFormat::Mbox => {
            let mut w = BufWriter::new(names.create("export.mbox")?);
            for (id, loc) in items {
                match message_and_mime(lib, *id, loc) {
                    Ok((msg, bytes)) => {
                        write_mbox_entry(&mut w, &msg.header, &bytes)?;
                        report.written += 1;
                    }
                    Err(e) => report.failed.push((loc.clone(), e.to_string())),
                }
            }
            w.flush()?;
        }
        ExportFormat::Csv => {
            let mut w = BufWriter::new(names.create("export.csv")?);
            // BOM so Excel opens the UTF-8 file with the right encoding.
            w.write_all(b"\xEF\xBB\xBF")?;
            write_csv_row(&mut w, &["date", "from", "to", "cc", "subject", "folder", "attachments", "size"])?;
            let mut folder_names: HashMap<SourceId, HashMap<FolderId, String>> = HashMap::new();
            for (id, loc) in items {
                match lib.message(*id, loc) {
                    Ok(msg) => {
                        let folders = folder_names
                            .entry(*id)
                            .or_insert_with(|| lib.folders(*id).map(|f| folder_paths(&f)).unwrap_or_default());
                        let h = &msg.header;
                        let folder = h
                            .folder
                            .as_ref()
                            .map(|f| folders.get(f).cloned().unwrap_or_else(|| f.0.clone()))
                            .unwrap_or_default();
                        let atts: Vec<&str> = msg.attachments.iter().filter_map(|a| a.filename.as_deref()).collect();
                        write_csv_row(
                            &mut w,
                            &[
                                &h.date.map(dates::format_long).unwrap_or_default(),
                                &h.from.as_ref().map(format_address).unwrap_or_default(),
                                &format_addresses(&h.to),
                                &format_addresses(&h.cc),
                                &h.subject,
                                &folder,
                                &atts.join("; "),
                                &h.size.to_string(),
                            ],
                        )?;
                        report.written += 1;
                    }
                    Err(e) => report.failed.push((loc.clone(), e.to_string())),
                }
            }
            w.flush()?;
        }
        ExportFormat::Eml => {
            for (id, loc) in items {
                match message_and_mime(lib, *id, loc) {
                    Ok((msg, bytes)) => {
                        names.create(&message_file_name(&msg.header, "eml"))?.write_all(&bytes)?;
                        report.written += 1;
                    }
                    Err(e) => report.failed.push((loc.clone(), e.to_string())),
                }
            }
        }
        ExportFormat::Html => {
            for (id, loc) in items {
                match lib.message(*id, loc).and_then(|m| render_html(lib, *id, loc, &m).map(|h| (m, h))) {
                    Ok((msg, html)) => {
                        names.create(&message_file_name(&msg.header, "html"))?.write_all(html.as_bytes())?;
                        report.written += 1;
                    }
                    Err(e) => report.failed.push((loc.clone(), e.to_string())),
                }
            }
        }
        ExportFormat::Text => {
            for (id, loc) in items {
                match lib.message(*id, loc) {
                    Ok(msg) => {
                        names
                            .create(&message_file_name(&msg.header, "txt"))?
                            .write_all(render_text(&msg).as_bytes())?;
                        report.written += 1;
                    }
                    Err(e) => report.failed.push((loc.clone(), e.to_string())),
                }
            }
        }
        ExportFormat::Vcf => {
            let mut w = LazyFile::new(&mut names, "export.vcf");
            for (id, loc) in items {
                match lib.message(*id, loc) {
                    Ok(msg) if cards::write_vcard(&mut w, &msg)? => report.written += 1,
                    Ok(_) => report.skipped += 1,
                    Err(e) => report.failed.push((loc.clone(), e.to_string())),
                }
            }
            w.flush()?;
        }
        ExportFormat::Ics => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
            let mut cal = cards::Calendar::new(LazyFile::new(&mut names, "export.ics"), stamp);
            for (id, loc) in items {
                match lib.message(*id, loc).map(|msg| cal.add(*id, &msg)) {
                    Ok(added) => match added? {
                        cards::Added::Written => report.written += 1,
                        cards::Added::NotApplicable => report.skipped += 1,
                        cards::Added::Duplicate => report.duplicates += 1,
                    },
                    Err(e) => report.failed.push((loc.clone(), e.to_string())),
                }
            }
            cal.finish()?.flush()?;
        }
        ExportFormat::Attachments => {
            for (id, loc) in items {
                let msg = match lib.message(*id, loc) {
                    Ok(m) => m,
                    Err(e) => {
                        report.failed.push((loc.clone(), e.to_string()));
                        continue;
                    }
                };
                for a in &msg.attachments {
                    let data = if a.kind == AttachmentKind::EmbeddedMessage {
                        let eloc = Locator::Embedded { parent: Box::new(loc.clone()), attachment: a.index };
                        lib.mime(*id, &eloc).map(|b| (b, true))
                    } else {
                        lib.attachment(*id, loc, a.index).map(|b| (b, false))
                    };
                    match data {
                        Ok((bytes, is_msg)) => {
                            // Only the last component of a stored path (`C:\docs\a.pdf`, `../a.pdf`).
                            let base = a.filename.as_deref().and_then(|n| n.rsplit(['/', '\\']).next());
                            let mut name = base.filter(|n| !n.trim().is_empty()).map_or_else(
                                || {
                                    format!(
                                        "attachment-{}{}",
                                        a.index,
                                        mime::extension_for(a.mime_type.as_deref().unwrap_or(""))
                                    )
                                },
                                str::to_owned,
                            );
                            if is_msg && !name.to_ascii_lowercase().ends_with(".eml") {
                                name.push_str(".eml");
                            }
                            names.create(&name)?.write_all(&bytes)?;
                            report.written += 1;
                        }
                        Err(e) if matches!(a.kind, AttachmentKind::Link) => {
                            tracing::debug!("skipping link attachment {}: {e}", a.index);
                        }
                        Err(e) => report.failed.push((loc.clone(), format!("attachment {}: {e}", a.index))),
                    }
                }
            }
        }
    }
    Ok(report)
}

/// Output file created on the first write, so an export with nothing to write leaves no
/// (empty, and as vCard / iCalendar invalid) file behind.
struct LazyFile<'a> {
    names: &'a mut NameAllocator,
    name: &'static str,
    file: Option<BufWriter<File>>,
}

impl<'a> LazyFile<'a> {
    fn new(names: &'a mut NameAllocator, name: &'static str) -> Self {
        LazyFile { names, name, file: None }
    }
}

impl Write for LazyFile<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let file = match &mut self.file {
            Some(f) => f,
            None => self.file.insert(BufWriter::new(self.names.create(self.name)?)),
        };
        file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

fn message_and_mime(lib: &Library, id: SourceId, loc: &Locator) -> Result<(Message, Vec<u8>)> {
    let src = lib.source(id)?;
    let mut msg = embedded::message(&*src, loc)?;
    if msg.header.locator.is_none() {
        msg.header.locator = Some(loc.clone());
    }
    let bytes = mime::message_mime(&*src, loc, &msg, 0)?;
    Ok((msg, bytes))
}

/// `2024-03-05 1407 Subject.ext`
fn message_file_name(h: &Header, ext: &str) -> String {
    let date = h.date.map(|d| {
        let c = dates::civil(d);
        format!("{:04}-{:02}-{:02} {:02}{:02} ", c.year, c.month, c.day, c.hour, c.minute)
    });
    let subject = h.subject.trim();
    let subject = if subject.is_empty() { "(no subject)" } else { subject };
    // Keep room for the extension and a collision suffix within the name limit.
    let subject: String = subject.chars().take(90).collect();
    format!("{}{}.{ext}", date.unwrap_or_default(), subject)
}

/// `/`-joined name path of every folder (parents precede children, per the `MailSource`
/// contract).
pub(crate) fn folder_paths(folders: &[Folder]) -> HashMap<FolderId, String> {
    let mut out: HashMap<FolderId, String> = HashMap::with_capacity(folders.len());
    for f in folders {
        let path = match f.parent.as_ref().and_then(|p| out.get(p)) {
            Some(parent) if !parent.is_empty() => format!("{parent}/{}", f.name),
            _ => f.name.clone(),
        };
        out.insert(f.id.clone(), path);
    }
    out
}

/// RFC 4180 row; fields with `,` `"` CR or LF are quoted. Fields that a spreadsheet would
/// evaluate as a formula (leading `=` `+` `-` `@` TAB CR) get a `'` prefix, so opening the
/// export in Excel cannot run attacker-supplied formulas (CSV injection).
fn write_csv_row(w: &mut impl Write, fields: &[&str]) -> std::io::Result<()> {
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            w.write_all(b",")?;
        }
        let formula = f.starts_with(['=', '+', '-', '@', '\t', '\r']);
        let quoted = f.contains([',', '"', '\r', '\n']);
        if quoted {
            w.write_all(b"\"")?;
        }
        if formula {
            w.write_all(b"'")?;
        }
        if quoted {
            w.write_all(f.replace('"', "\"\"").as_bytes())?;
            w.write_all(b"\"")?;
        } else {
            w.write_all(f.as_bytes())?;
        }
    }
    w.write_all(b"\r\n")
}

/// One mboxrd entry: `From ` separator, body lines with `>*From ` quoted by one more `>`,
/// LF line endings, and a blank line after the message.
pub(crate) fn write_mbox_entry(w: &mut impl Write, h: &Header, mime: &[u8]) -> std::io::Result<()> {
    let sender: String = h
        .from
        .as_ref()
        .and_then(|a| a.email.as_deref())
        .filter(|e| !e.trim().is_empty())
        .unwrap_or("MAILER-DAEMON")
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    writeln!(w, "From {} {}", sender, dates::format_asctime(h.date.unwrap_or(0)))?;
    let mut lines = mime.split(|&b| b == b'\n').peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() && lines.peek().is_none() {
            break; // the split piece after a final newline
        }
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let gt = line.iter().take_while(|&&b| b == b'>').count();
        if line[gt..].starts_with(b"From ") {
            w.write_all(b">")?;
        }
        w.write_all(line)?;
        w.write_all(b"\n")?;
    }
    w.write_all(b"\n")
}

fn render_text(msg: &Message) -> String {
    let h = &msg.header;
    let mut s = String::new();
    let mut line = |k: &str, v: &str| {
        if !v.is_empty() {
            s.push_str(k);
            s.push_str(": ");
            s.push_str(v);
            s.push('\n');
        }
    };
    line("From", &h.from.as_ref().map(format_address).unwrap_or_default());
    line("To", &format_addresses(&h.to));
    line("Cc", &format_addresses(&h.cc));
    line("Bcc", &format_addresses(&h.bcc));
    line("Date", &h.date.map(dates::format_rfc2822).unwrap_or_default());
    line("Subject", &h.subject);
    let atts: Vec<&str> = msg.attachments.iter().filter_map(|a| a.filename.as_deref()).collect();
    line("Attachments", &atts.join(", "));
    s.push('\n');
    s.push_str(&body_text(&msg.body).replace("\r\n", "\n"));
    s.push('\n');
    s
}

const HTML_STYLE: &str = "table.stv-hdr{border-collapse:collapse;font:13px system-ui,sans-serif;margin:0 0 12px}\
table.stv-hdr th{text-align:right;padding:2px 8px 2px 0;color:#555;vertical-align:top}\
table.stv-hdr td{padding:2px 0}hr.stv-sep{border:0;border-top:1px solid #ccc;margin:8px 0 16px}";

/// Self-contained HTML: header table + body; `cid:` images become `data:` URIs.
fn render_html(lib: &Library, id: SourceId, loc: &Locator, msg: &Message) -> Result<String> {
    let h = &msg.header;
    let mut table = String::from("<table class=\"stv-hdr\">");
    let mut row = |k: &str, v: &str| {
        if !v.is_empty() {
            let _ = write!(table, "<tr><th>{k}</th><td>{}</td></tr>", html_escape(v));
        }
    };
    row("From", &h.from.as_ref().map(format_address).unwrap_or_default());
    row("To", &format_addresses(&h.to));
    row("Cc", &format_addresses(&h.cc));
    row("Bcc", &format_addresses(&h.bcc));
    row("Date", &h.date.map(dates::format_rfc2822).unwrap_or_default());
    row("Subject", &h.subject);
    let listed: Vec<String> = msg
        .attachments
        .iter()
        .filter(|a| !a.inline)
        .map(|a| format!("{} ({} bytes)", a.filename.as_deref().unwrap_or("attachment"), a.size))
        .collect();
    row("Attachments", &listed.join(", "));
    table.push_str("</table><hr class=\"stv-sep\">");

    let title = html_escape(&h.subject);
    // The CSP comes first so it governs everything after it, including the original head.
    let head =
        format!("{}<meta charset=\"utf-8\"><title>{title}</title><style>{HTML_STYLE}</style>", htmlsafe::CSP_META);
    let doc = match msg.body.html.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(body) => {
            let body = inline_cid_images(lib, id, loc, msg, &htmlsafe::neutralize(body));
            let lower = body.to_ascii_lowercase();
            let body_tag_end = lower.find("<body").and_then(|p| lower[p..].find('>').map(|q| p + q + 1));
            match body_tag_end {
                Some(end) => {
                    let mut out = String::with_capacity(body.len() + table.len() + head.len() + 16);
                    out.push_str(&head);
                    out.push_str(&body[..end]);
                    out.push_str(&table);
                    out.push_str(&body[end..]);
                    out
                }
                None => format!("<!DOCTYPE html><html><head>{head}</head><body>{table}{body}</body></html>"),
            }
        }
        None => format!(
            "<!DOCTYPE html><html><head>{head}</head><body>{table}<pre style=\"white-space:pre-wrap;font:13px monospace\">{}</pre></body></html>",
            html_escape(&body_text(&msg.body))
        ),
    };
    // UTF-8 BOM: overrides any charset declared by the original HTML.
    Ok(format!("\u{FEFF}{doc}"))
}

/// Most bytes of `data:` URIs inlined into one HTML export; references past it keep
/// their `cid:` (a message referencing one large image thousands of times must not
/// blow up the output).
const MAX_INLINED_BYTES: usize = 64 << 20;

/// Replaces each `cid:<id>` URL whose id is exactly an attachment's Content-ID with a
/// `data:` URI; each attachment is read and encoded at most once.
fn inline_cid_images(lib: &Library, id: SourceId, loc: &Locator, msg: &Message, html: &str) -> String {
    let by_cid: HashMap<&str, &AttachmentMeta> = msg
        .attachments
        .iter()
        .filter_map(|a| {
            let cid = a.content_id.as_deref()?.trim().trim_start_matches('<').trim_end_matches('>');
            (!cid.is_empty()).then_some((cid, a))
        })
        .collect();
    if by_cid.is_empty() {
        return html.to_owned();
    }
    let lower = html.to_ascii_lowercase();
    let mut uris: HashMap<&str, Option<String>> = HashMap::new();
    let mut budget = MAX_INLINED_BYTES;
    let mut out = String::with_capacity(html.len());
    let (mut copied, mut pos) = (0, 0);
    while let Some(off) = lower[pos..].find("cid:") {
        let start = pos + off;
        let token = start + 4;
        let end = html[token..]
            .find(|c: char| matches!(c, '"' | '\'' | '<' | '>' | ')') || c.is_whitespace())
            .map_or(html.len(), |p| token + p);
        pos = token;
        let cid = &html[token..end];
        let Some(meta) = by_cid.get(cid) else { continue };
        let uri = uris.entry(cid).or_insert_with(|| {
            let bytes = lib.attachment(id, loc, meta.index).ok()?;
            let mime_type = meta
                .mime_type
                .clone()
                .filter(|m| m.contains('/'))
                .unwrap_or_else(|| mime::guess_mime(meta.filename.as_deref().unwrap_or("")).to_owned());
            Some(format!("data:{mime_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes)))
        });
        let Some(uri) = uri.as_deref().filter(|u| u.len() <= budget) else { continue };
        budget -= uri.len();
        out.push_str(&html[copied..start]);
        out.push_str(uri);
        copied = end;
        pos = end;
    }
    out.push_str(&html[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_model::Address;

    #[test]
    fn mboxrd_quotes_from_lines_and_normalises_newlines() {
        let h = Header {
            from: Some(Address { name: None, email: Some("a b@example.com".into()) }),
            date: Some(0),
            ..Default::default()
        };
        let mut out = Vec::new();
        write_mbox_entry(&mut out, &h, b"Subject: x\r\n\r\nFrom here\r\n>From there\r\nok\r\n").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "From a_b@example.com Thu Jan  1 00:00:00 1970\nSubject: x\n\n>From here\n>>From there\nok\n\n"
        );
    }

    #[test]
    fn csv_quoting() {
        let mut out = Vec::new();
        write_csv_row(&mut out, &["plain", "a,b", "say \"hi\"", "line\nbreak"]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "plain,\"a,b\",\"say \"\"hi\"\"\",\"line\nbreak\"\r\n");
    }

    #[test]
    fn csv_formulas_are_not_evaluated() {
        let mut out = Vec::new();
        write_csv_row(&mut out, &["=HYPERLINK(\"x\")", "+1", "-2", "@SUM(A1)", "a=b", "\tx"]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\"'=HYPERLINK(\"\"x\"\")\",'+1,'-2,'@SUM(A1),a=b,'\tx\r\n");
    }
}
