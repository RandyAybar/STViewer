//! Large-folder performance check (ignored by default; run in release):
//! `cargo test --release -p stv-pst --target-dir target/pst --test perf -- --ignored --nocapture`
//!
//! Builds a Unicode PST whose "Bulk" folder has 100 000 contents-table rows and measures open,
//! first folder indexing and 100-row pages under every sort.

mod common;

use std::time::Instant;

use common::*;
use mail_model::{MailSource, Sort, SortKey};
use stv_pst::crypt::NDB_CRYPT_PERMUTE;
use stv_pst::ndb::Format;
use stv_pst::PstSource;

#[test]
#[ignore]
fn hundred_thousand_row_folder() {
    const N: usize = 100_000;
    let t = Instant::now();
    let mut w = Writer::new(Format::Unicode, NDB_CRYPT_PERMUTE, false);
    w.contents_only = true;
    let bytes = build_store(w, &fixture(N), b"SM");
    eprintln!("generated {} MB in {:?}", bytes.len() >> 20, t.elapsed());
    let path = std::env::temp_dir().join(format!("stv-pst-perf-{}.pst", std::process::id()));
    std::fs::write(&path, &bytes).unwrap();
    drop(bytes);

    let t = Instant::now();
    let src = PstSource::open(&path).unwrap();
    let open = t.elapsed();
    let bulk = src.folders().unwrap().into_iter().find(|f| f.name == "Bulk").unwrap();
    let t = Instant::now();
    assert_eq!(src.count(&bulk.id).unwrap(), N as u64);
    let index = t.elapsed();
    eprintln!("open {open:?}; first count (row-key index of {N} rows) {index:?}");
    let mut worst_page = std::time::Duration::ZERO;
    for key in [SortKey::Native, SortKey::Date, SortKey::Size, SortKey::From, SortKey::Subject] {
        let sort = Sort { key, descending: true };
        let t = Instant::now();
        let first = src.list(&bulk.id, 50_000..50_100, sort).unwrap();
        let build = t.elapsed();
        let t = Instant::now();
        let page = src.list(&bulk.id, 99_900..100_000, sort).unwrap();
        let p = t.elapsed();
        worst_page = worst_page.max(p);
        assert_eq!((first.len(), page.len()), (100, 100));
        eprintln!("{key:?}: first page (incl. sort build) {build:?}; next page {p:?}");
    }
    drop(src);
    std::fs::remove_file(&path).ok();
    if !cfg!(debug_assertions) {
        assert!(open.as_millis() < 50, "open took {open:?}");
        assert!(worst_page.as_millis() < 50, "100-row page took {worst_page:?}");
        assert!(index.as_millis() < 500, "folder index took {index:?}");
    }
}
