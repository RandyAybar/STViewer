//! Dump a mailbox: `cargo run -p stv-mbox --release --example mbox-dump -- <path> [--rows N] [--no-cache]`

use std::time::Instant;

use mail_model::{FolderId, MailSource, Sort, SortKey};
use stv_mbox::{MboxSource, OpenOptions};

fn iso(ms: i64) -> String {
    let (y, m, d) = mail_model::time::civil_from_days(ms.div_euclid(mail_model::time::MS_PER_DAY));
    let rem = ms.rem_euclid(mail_model::time::MS_PER_DAY) / 1000;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut rows = 5u64;
    let mut opts = OpenOptions::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--rows" => rows = args.next().and_then(|v| v.parse().ok()).unwrap_or(rows),
            "--no-cache" => opts.no_cache = true,
            _ => path = Some(std::path::PathBuf::from(a)),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: mbox-dump <path> [--rows N] [--no-cache]");
        std::process::exit(2);
    };

    let t = Instant::now();
    let src = match MboxSource::open(&path, &opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let folders = src.folders().expect("folders");
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("{} ({:?})", src.display_name(), src.kind());
    println!("open: {open_ms:.1} ms, load folders: {load_ms:.1} ms ({:?})", src.load_outcomes());
    if let Ok(fp) = src.fingerprint() {
        println!("fingerprint: size={} mtime={} head_hash={:016x}", fp.size, fp.mtime, fp.head_hash);
    }

    let depth = |id: &FolderId| {
        let mut d = 0;
        let mut cur = folders.iter().find(|f| &f.id == id).and_then(|f| f.parent.clone());
        while let Some(p) = cur {
            d += 1;
            cur = folders.iter().find(|f| f.id == p).and_then(|f| f.parent.clone());
        }
        d
    };
    for f in &folders {
        let indent = "  ".repeat(depth(&f.id));
        println!(
            "{indent}[{}] {:?} {:?} messages={} unread={}",
            f.name,
            f.id.0,
            f.kind,
            f.message_count.unwrap_or(0),
            f.unread_count.unwrap_or(0)
        );
        let t = Instant::now();
        let list = src.list(&f.id, 0..rows, Sort { key: SortKey::Date, descending: true }).expect("list");
        let list_ms = t.elapsed().as_secs_f64() * 1000.0;
        for h in &list {
            let from =
                h.from.as_ref().map(|a| a.name.clone().or(a.email.clone()).unwrap_or_default()).unwrap_or_default();
            println!(
                "{indent}    {} | {:<24.24} | {}{}{}",
                h.date.map(iso).unwrap_or_else(|| "????-??-?? ??:??".into()),
                from,
                h.subject,
                if h.has_attachments { " [att]" } else { "" },
                if h.flags.read { "" } else { " [unread]" },
            );
        }
        if !list.is_empty() {
            println!("{indent}    (page in {list_ms:.2} ms)");
        }
    }

    let first = src.iter_headers().find_map(Result::ok);
    if let Some(h) = first {
        let loc = h.locator.expect("locator");
        let t = Instant::now();
        match src.message(&loc) {
            Ok(m) => {
                println!(
                    "\nfirst message ({:.2} ms): {:?}\n  text: {} chars, html: {} chars, {} properties",
                    t.elapsed().as_secs_f64() * 1000.0,
                    m.header.subject,
                    m.body.text.as_ref().map_or(0, |s| s.chars().count()),
                    m.body.html.as_ref().map_or(0, |s| s.chars().count()),
                    src.properties(&loc).map_or(0, |p| p.len())
                );
                for a in &m.attachments {
                    println!(
                        "  attachment #{} {:?} {:?} {} bytes {:?}{}",
                        a.index,
                        a.filename,
                        a.mime_type,
                        a.size,
                        a.kind,
                        if a.inline { " inline" } else { "" }
                    );
                }
            }
            Err(e) => println!("\nfirst message failed: {e}"),
        }
    }
}
