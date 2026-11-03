//! Print what `stv-msg` decodes from `.msg` / TNEF files or directories.
//!
//! `cargo run -p stv-msg --example msg-dump -- [--props] <file-or-dir>...`

use std::path::Path;

use mail_model::{Address, AttachmentKind, Locator, MailSource, Sort};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let props = args.iter().any(|a| a == "--props");
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if paths.is_empty() {
        eprintln!("usage: msg-dump [--props] <file-or-dir>...");
        std::process::exit(2);
    }
    for p in paths {
        dump_source(Path::new(p), props);
    }
}

fn dump_source(path: &Path, props: bool) {
    let opener = stv_msg::opener();
    println!("=== {} (sniff: {:?})", path.display(), opener.sniff(path));
    let src = match opener.open(path) {
        Ok(s) => s,
        Err(e) => {
            println!("  open error: {e}");
            return;
        }
    };
    println!("  kind={:?} name={:?}", src.kind(), src.display_name());
    let folders = match src.folders() {
        Ok(f) => f,
        Err(e) => {
            println!("  folders error: {e}");
            return;
        }
    };
    for f in folders {
        let n = src.count(&f.id).unwrap_or(0);
        println!("  folder {:?} name={:?} messages={n}", f.id.0, f.name);
        let headers = match src.list(&f.id, 0..n, Sort::default()) {
            Ok(h) => h,
            Err(e) => {
                println!("    list error: {e}");
                continue;
            }
        };
        for h in headers {
            if let Some(loc) = &h.locator {
                dump_message(src.as_ref(), loc, 2, props);
            }
        }
    }
}

fn addr(a: &Address) -> String {
    match (&a.name, &a.email) {
        (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
        (_, Some(e)) => e.clone(),
        (Some(n), None) => n.clone(),
        (None, None) => "?".into(),
    }
}

fn addrs(v: &[Address]) -> String {
    v.iter().map(addr).collect::<Vec<_>>().join(", ")
}

fn excerpt(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    let t = t.replace('\r', "\\r").replace('\n', "\\n");
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

fn date(ms: i64) -> String {
    let (y, m, d) = mail_model::time::civil_from_days(ms.div_euclid(mail_model::time::MS_PER_DAY));
    let rem = ms.rem_euclid(mail_model::time::MS_PER_DAY) / 1000;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

fn dump_message(src: &dyn MailSource, loc: &Locator, depth: usize, props: bool) {
    let ind = "  ".repeat(depth);
    let m = match src.message(loc) {
        Ok(m) => m,
        Err(e) => {
            println!("{ind}message {loc:?}: error: {e}");
            return;
        }
    };
    let h = &m.header;
    println!("{ind}message {loc:?}");
    println!("{ind}  subject: {:?}", h.subject);
    println!("{ind}  class:   {:?}", h.message_class);
    if let Some(f) = &h.from {
        println!("{ind}  from:    {}", addr(f));
    }
    if !h.to.is_empty() {
        println!("{ind}  to:      {}", addrs(&h.to));
    }
    if !h.cc.is_empty() {
        println!("{ind}  cc:      {}", addrs(&h.cc));
    }
    if !h.bcc.is_empty() {
        println!("{ind}  bcc:     {}", addrs(&h.bcc));
    }
    if let Some(d) = h.date {
        println!("{ind}  date:    {}", date(d));
    }
    println!("{ind}  size={} has_attachments={} flags={:?}", h.size, h.has_attachments, h.flags);
    if let Some(id) = &h.message_id {
        println!("{ind}  message-id: {id}");
    }
    if !h.labels.is_empty() {
        println!("{ind}  labels:  {:?}", h.labels);
    }
    if let Some(c) = &h.conversation {
        println!("{ind}  conversation: {c}");
    }
    if let Some(t) = &m.body.text {
        println!("{ind}  text ({} chars): {}", t.chars().count(), excerpt(t, 160));
    }
    if let Some(t) = &m.body.html {
        println!("{ind}  html ({} chars): {}", t.chars().count(), excerpt(t, 120));
    }
    if let Some(t) = &m.body.rtf {
        println!("{ind}  rtf ({} chars): {}", t.chars().count(), excerpt(t, 80));
    }
    if let Some(r) = &m.raw_headers {
        println!("{ind}  raw headers: {} bytes", r.len());
    }
    match src.properties(loc) {
        Ok(list) => {
            println!("{ind}  properties: {}", list.len());
            if props {
                for p in &list {
                    println!("{ind}    {} {} {} = {}", p.tag, p.name, p.type_name, excerpt(&p.value, 100));
                }
            }
        }
        Err(e) => println!("{ind}  properties: error: {e}"),
    }
    for a in &m.attachments {
        let bytes = src.attachment(loc, a.index);
        let got = match &bytes {
            Ok(b) => format!("{} bytes", b.len()),
            Err(e) => format!("error: {e}"),
        };
        println!(
            "{ind}  attachment #{} {:?} kind={:?} mime={:?} size={} cid={:?} inline={} -> {got}",
            a.index, a.filename, a.kind, a.mime_type, a.size, a.content_id, a.inline
        );
        if matches!(a.kind, AttachmentKind::EmbeddedMessage | AttachmentKind::Tnef) {
            let child = Locator::Embedded { parent: Box::new(loc.clone()), attachment: a.index };
            dump_message(src, &child, depth + 2, props);
        }
    }
}
