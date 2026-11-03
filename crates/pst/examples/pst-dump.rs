//! `cargo run -p stv-pst --example pst-dump -- <file> [--headers N] [--deep]`
//!
//! Prints format details, the folder tree with counts, the first N headers of every folder
//! and timings. `--deep` additionally opens every message, every attachment payload and every
//! embedded message, reporting failures.

use std::path::PathBuf;
use std::time::Instant;

use mail_model::{AttachmentKind, Locator, MailSource, Sort, SortKey};
use stv_pst::PstSource;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut headers = 3u64;
    let mut deep = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--headers" => headers = args.next().and_then(|n| n.parse().ok()).unwrap_or(3),
            "--deep" => deep = true,
            _ => path = Some(a.into()),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: pst-dump <file.pst|file.ost> [--headers N] [--deep]");
        std::process::exit(2);
    };

    let t = Instant::now();
    let src = match PstSource::open(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("open failed: {e}");
            std::process::exit(1);
        }
    };
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    let h = &src.file().ndb.header;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("file:     {} ({size} bytes)", path.display());
    println!(
        "format:   {:?} (wVer {}, client {}, wVerClient {}), crypt {}, kind {:?}",
        src.format(),
        h.version,
        String::from_utf8_lossy(&h.client),
        h.client_version,
        h.crypt,
        src.kind()
    );
    println!("store:    {:?}", src.display_name());
    println!("open:     {open_ms:.2} ms");

    let t = Instant::now();
    let folders = match src.folders() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("folders failed: {e}");
            std::process::exit(1);
        }
    };
    println!("folders:  {} in {:.2} ms", folders.len(), t.elapsed().as_secs_f64() * 1e3);

    let depth = |f: &mail_model::Folder| {
        let mut d = 0;
        let mut p = f.parent.clone();
        while let Some(id) = p {
            d += 1;
            p = folders.iter().find(|x| x.id == id).and_then(|x| x.parent.clone());
        }
        d
    };
    let mut total = 0u64;
    let mut largest: Option<(mail_model::FolderId, u64)> = None;
    for f in &folders {
        let t = Instant::now();
        let n = src.count(&f.id).unwrap_or_else(|e| {
            println!("  !! count failed for {}: {e}", f.id.0);
            0
        });
        let count_ms = t.elapsed().as_secs_f64() * 1e3;
        total += n;
        if largest.as_ref().is_none_or(|l| n > l.1) {
            largest = Some((f.id.clone(), n));
        }
        let indent = "  ".repeat(depth(f) + 1);
        println!(
            "{indent}{} [{:?}] {} — rows {n} (ContentCount {:?}, unread {:?}) [{count_ms:.2} ms]",
            f.name, f.kind, f.id.0, f.message_count, f.unread_count
        );
        if n > 0 && headers > 0 {
            let sort = Sort { key: SortKey::Date, descending: true };
            match src.list(&f.id, 0..headers, sort) {
                Ok(hs) => {
                    for h in hs {
                        let from = h.from.as_ref().and_then(|a| a.name.clone().or(a.email.clone())).unwrap_or_default();
                        let date = h.date.map(mapi_props::format_unix_ms).unwrap_or_default();
                        println!(
                            "{indent}  - {date} | {from} | {:?} | to {} | {} B{}",
                            h.subject,
                            h.to.iter().filter_map(|a| a.name.clone()).collect::<Vec<_>>().join(", "),
                            h.size,
                            if h.has_attachments { " | 📎" } else { "" }
                        );
                    }
                }
                Err(e) => println!("{indent}  !! list failed: {e}"),
            }
        }
    }
    println!("messages: {total}");

    if let Some((id, n)) = largest.filter(|l| l.1 > 0) {
        for key in [SortKey::Native, SortKey::Date, SortKey::Start, SortKey::Subject, SortKey::From, SortKey::Size] {
            let sort = Sort { key, descending: true };
            let mid = n / 2;
            let t = Instant::now();
            let first = src.list(&id, mid..mid + 100, sort).map(|v| v.len());
            let first_ms = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let again = src.list(&id, 0..100, sort).map(|v| v.len());
            println!(
                "list {} ({n} rows) by {key:?}: page@{mid} {first:?} in {first_ms:.2} ms (incl. sort build), page@0 {again:?} in {:.2} ms",
                id.0,
                t.elapsed().as_secs_f64() * 1e3
            );
        }
    }

    let t = Instant::now();
    let mut n_headers = 0;
    let mut n_errors = 0;
    for h in src.iter_headers() {
        match h {
            Ok(_) => n_headers += 1,
            Err(_) => n_errors += 1,
        }
    }
    println!("iter_headers: {n_headers} ok, {n_errors} errors in {:.2} ms", t.elapsed().as_secs_f64() * 1e3);

    if deep {
        deep_check(&src);
    }
}

fn deep_check(src: &PstSource) {
    let t = Instant::now();
    let (mut msgs, mut atts, mut embedded, mut bytes, mut html, mut text, mut rtf) = (0, 0, 0, 0u64, 0, 0, 0);
    let mut errors: Vec<String> = Vec::new();
    let mut stack: Vec<Locator> = src.iter_headers().filter_map(|h| h.ok()?.locator).collect();
    while let Some(loc) = stack.pop() {
        let m = match src.message(&loc) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("message {loc:?}: {e}"));
                continue;
            }
        };
        msgs += 1;
        html += m.body.html.is_some() as usize;
        text += m.body.text.is_some() as usize;
        rtf += m.body.rtf.is_some() as usize;
        for a in &m.attachments {
            if a.kind == AttachmentKind::EmbeddedMessage {
                embedded += 1;
                stack.push(Locator::Embedded { parent: Box::new(loc.clone()), attachment: a.index });
                continue;
            }
            match src.attachment(&loc, a.index) {
                Ok(b) => {
                    atts += 1;
                    bytes += b.len() as u64;
                }
                Err(mail_model::Error::Unsupported(_)) => {}
                Err(e) => errors.push(format!("attachment {} of {loc:?}: {e}", a.index)),
            }
        }
    }
    println!(
        "deep: {msgs} messages ({html} html, {text} text, {rtf} rtf-only), {atts} attachments ({bytes} bytes), {embedded} embedded, {} errors in {:.2} ms",
        errors.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    for e in errors.iter().take(10) {
        println!("  !! {e}");
    }
}
