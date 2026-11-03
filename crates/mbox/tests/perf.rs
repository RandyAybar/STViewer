//! Large-mailbox performance check (generates `testdata/large/perf.mbox`, ~1 GB, if absent).
//! Run explicitly: `cargo test --profile release-fast -p stv-mbox --test perf -- --ignored --nocapture`

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use mail_model::{FolderId, MailSource, Sort, SortKey};
use stv_mbox::{MboxSource, OpenOptions};

const MESSAGES: u64 = 200_000;
const BODY_BYTES: usize = 4_900;

fn large_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/large/perf.mbox")
}

fn generate(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension("tmp");
    let mut w = BufWriter::with_capacity(8 << 20, std::fs::File::create(&tmp).unwrap());
    let filler: String =
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor.\n".repeat(BODY_BYTES / 80 + 1);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for i in 0..MESSAGES {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let day = 1 + (x % 28);
        let month = 1 + ((x >> 8) % 12);
        let year = 2005 + ((x >> 16) % 20);
        let hour = (x >> 24) % 24;
        let sender = (x >> 32) % 5000;
        let mon =
            ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(month - 1) as usize];
        write!(
            w,
            "From user{sender}@example.com Mon {mon} {day:2} {hour:02}:00:00 {year}\n\
From: =?utf-8?Q?User_{sender}_M=C3=BCller?= <user{sender}@example.com>\n\
To: team{}@example.com, other@example.com\n\
Subject: Status report #{i} for project {}\n\
Date: Mon, {day} {mon} {year} {hour:02}:00:00 +0000\n\
Message-ID: <msg{i}@example.com>\n\
References: <msg{}@example.com>\n\
X-Mozilla-Status: 000{}\n\
Content-Type: text/plain; charset=utf-8\n\
\n",
            sender % 50,
            x % 977,
            i / 2,
            x % 2,
        )
        .unwrap();
        w.write_all(&filler.as_bytes()[..BODY_BYTES - (x % 700) as usize]).unwrap();
        w.write_all(b"\n>From quoted line in body\n\n").unwrap();
    }
    w.flush().unwrap();
    drop(w);
    std::fs::rename(&tmp, path).unwrap();
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore]
fn large_mbox_perf() {
    let path = large_path();
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) < 900 << 20 {
        let t = Instant::now();
        generate(&path);
        println!("generated {} in {:.0} ms", path.display(), ms(t));
    }
    let size = std::fs::metadata(&path).unwrap().len();
    let cache = tempfile::tempdir().unwrap();
    let opts = OpenOptions { cache_dir: Some(cache.path().to_path_buf()), no_cache: false };
    let root = FolderId(String::new());

    let t = Instant::now();
    let src = MboxSource::open(&path, &opts).unwrap();
    let n = src.count(&root).unwrap();
    let first = ms(t);
    assert_eq!(n, MESSAGES);
    assert_eq!(src.load_outcomes(), vec!["fresh"]);
    drop(src);

    let t = Instant::now();
    let src = MboxSource::open(&path, &opts).unwrap();
    assert_eq!(src.count(&root).unwrap(), MESSAGES);
    let reopen = ms(t);
    assert_eq!(src.load_outcomes(), vec!["cached"]);

    let t = Instant::now();
    let p = src.list(&root, 0..100, Sort { key: SortKey::Date, descending: true }).unwrap();
    let date_first = ms(t);
    assert_eq!(p.len(), 100);
    assert!(p.windows(2).all(|w| w[0].date >= w[1].date));

    let mut worst = 0f64;
    let mut total = 0f64;
    let pages = 200u64;
    for k in 0..pages {
        let start = (k * 7919 * 100) % (MESSAGES - 100);
        let t = Instant::now();
        let p = src.list(&root, start..start + 100, Sort { key: SortKey::Date, descending: true }).unwrap();
        let e = ms(t);
        assert_eq!(p.len(), 100);
        worst = worst.max(e);
        total += e;
    }

    let t = Instant::now();
    let s = src.list(&root, 100_000..100_100, Sort { key: SortKey::Subject, descending: false }).unwrap();
    let subject_first = ms(t);
    assert_eq!(s.len(), 100);
    let t = Instant::now();
    src.list(&root, 50_000..50_100, Sort { key: SortKey::From, descending: false }).unwrap();
    let from_first = ms(t);

    let t = Instant::now();
    let m = src.message(p[50].locator.as_ref().unwrap()).unwrap();
    let msg = ms(t);
    assert!(m.body.text.unwrap().contains("From quoted line"));

    let t = Instant::now();
    let all = src.iter_headers().count();
    let iter = ms(t);
    assert_eq!(all as u64, MESSAGES);

    println!("file: {:.2} GiB, {MESSAGES} messages", size as f64 / (1u64 << 30) as f64);
    println!("first open (scan + parse + cache write, OS cache warm): {first:.0} ms");
    println!("cached reopen: {reopen:.1} ms");
    println!("list first page by date desc (incl. sort permutation): {date_first:.1} ms");
    println!("list 100-row page by date: avg {:.3} ms, worst {worst:.3} ms over {pages} pages", total / pages as f64);
    println!("first page by subject (incl. permutation): {subject_first:.1} ms; by from: {from_first:.1} ms");
    println!("message() one 5 KB message: {msg:.2} ms");
    println!("iter_headers all: {iter:.0} ms");
}
