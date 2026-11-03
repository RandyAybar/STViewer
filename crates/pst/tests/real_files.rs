//! Tests on real PST/OST files downloaded into `testdata/large/` (see its README). The
//! directory is git-ignored, so each test is skipped (with a note) when its file is missing.

use std::path::PathBuf;

use mail_model::{AttachmentKind, Locator, MailSource, Sort, SortKey, SourceKind};
use stv_pst::crypt;
use stv_pst::ndb::Format;
use stv_pst::PstSource;

fn fixture(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/large").join(name);
    if p.is_file() {
        Some(p)
    } else {
        eprintln!("skipping: {} not present (see testdata/large/README.md)", p.display());
        None
    }
}

/// Every block's stored CRC (trailer) matches the [MS-PST] CRC of its stored bytes; validates
/// the CRC implementation, block allocation sizes and trailer layout for each format.
fn check_block_crcs(src: &PstSource) -> (usize, usize) {
    let ndb = &src.file().ndb;
    let bytes = ndb.bytes();
    let fmt = ndb.format();
    let (mut ok, mut compressed) = (0, 0);
    for b in ndb.all_blocks().unwrap() {
        let ib = b.ib as usize;
        let cb = b.cb as usize;
        let t = ib + fmt.block_alloc_size(cb) - fmt.block_trailer_size();
        let tr = &bytes[t..t + fmt.block_trailer_size()];
        assert_eq!(u16::from_le_bytes([tr[0], tr[1]]) as usize, cb, "trailer cb of block 0x{:X}", b.bid);
        let (crc, bid) = match fmt {
            Format::Ansi => (
                u32::from_le_bytes(tr[8..12].try_into().unwrap()),
                u32::from_le_bytes(tr[4..8].try_into().unwrap()) as u64,
            ),
            _ => (u32::from_le_bytes(tr[4..8].try_into().unwrap()), u64::from_le_bytes(tr[8..16].try_into().unwrap())),
        };
        assert_eq!(bid & !1, b.bid & !1, "trailer bid");
        assert_eq!(crc, crypt::crc(&bytes[ib..ib + cb]), "CRC of block 0x{:X}", b.bid);
        if fmt == Format::Unicode4k {
            let inflated = u16::from_le_bytes([tr[18], tr[19]]) as usize;
            if inflated != 0 && inflated != cb {
                compressed += 1;
            }
        }
        // And every block decodes (decrypt / inflate).
        ndb.block(b.bid).unwrap();
        ok += 1;
    }
    (ok, compressed)
}

fn check_header_crc(src: &PstSource) {
    let h = &src.file().ndb.header;
    assert_eq!(h.crc_partial.0, h.crc_partial.1, "dwCRCPartial");
    if h.format != Format::Ansi {
        let b = src.file().ndb.bytes();
        assert_eq!(u32::from_le_bytes(b[524..528].try_into().unwrap()), crypt::crc(&b[8..524]), "dwCRCFull");
    }
}

/// Open every message, attachment payload and embedded message; returns counts.
fn deep(src: &PstSource) -> (usize, usize, usize) {
    let (mut msgs, mut atts, mut emb) = (0, 0, 0);
    let mut stack: Vec<Locator> = src.iter_headers().map(|h| h.unwrap().locator.unwrap()).collect();
    while let Some(loc) = stack.pop() {
        let m = src.message(&loc).unwrap_or_else(|e| panic!("{loc:?}: {e}"));
        msgs += 1;
        for a in &m.attachments {
            if a.kind == AttachmentKind::EmbeddedMessage {
                emb += 1;
                stack.push(Locator::Embedded { parent: Box::new(loc.clone()), attachment: a.index });
            } else if let Ok(data) = src.attachment(&loc, a.index) {
                assert_eq!(data.len() as u64, a.size, "attachment {} size of {loc:?}", a.index);
                atts += 1;
            }
        }
    }
    (msgs, atts, emb)
}

#[test]
fn outlook_2013_4k_ost() {
    let Some(p) = fixture("example-2013.ost") else {
        return;
    };
    let src = PstSource::open(&p).unwrap();
    assert_eq!(src.format(), Format::Unicode4k);
    assert_eq!(src.kind(), SourceKind::Ost);
    check_header_crc(&src);
    let (blocks, compressed) = check_block_crcs(&src);
    eprintln!("example-2013.ost: {blocks} blocks, {compressed} zlib-compressed");
    assert!(compressed > 0, "real 4K OST should contain compressed blocks");
    let folders = src.folders().unwrap();
    let inbox =
        folders.iter().find(|f| f.name == "Inbox" && src.count(&f.id).unwrap() == 2).expect("Inbox with 2 messages");
    let hs = src.list(&inbox.id, 0..10, Sort { key: SortKey::Date, descending: true }).unwrap();
    assert!(hs.iter().all(|h| h.subject == "Microsoft Outlook Test Message"));
    assert_eq!(hs[0].date, Some(1_401_985_352_000));
    let m = src.message(hs[0].locator.as_ref().unwrap()).unwrap();
    assert_eq!(m.header.from.unwrap().email.as_deref(), Some("arc.test1@apogeephysicians.com"));
    assert!(m.body.html.as_deref().unwrap().contains("This is an e-mail message sent automatically"));
    let (msgs, _, _) = deep(&src);
    assert_eq!(msgs, 3);
}

#[test]
fn aspose_outlook_pst_with_attachments_and_embedded() {
    let Some(p) = fixture("aspose-MAPI-Outlook.pst") else {
        return;
    };
    let src = PstSource::open(&p).unwrap();
    assert_eq!(src.format(), Format::Unicode);
    check_header_crc(&src);
    check_block_crcs(&src);
    let folders = src.folders().unwrap();
    assert_eq!(folders[0].name, "Outlook");
    let inbox = folders.iter().find(|f| f.name == "Inbox").unwrap();
    assert_eq!(src.count(&inbox.id).unwrap(), 8);
    let hs = src.list(&inbox.id, 0..8, Sort { key: SortKey::Subject, descending: false }).unwrap();
    assert!(hs.iter().any(|h| h.subject == "Multiple attachments" && h.has_attachments));
    let multi = hs.iter().find(|h| h.subject == "Multiple attachments").unwrap();
    let m = src.message(multi.locator.as_ref().unwrap()).unwrap();
    assert!(m.attachments.len() >= 2, "attachments: {:?}", m.attachments);
    assert!(m.attachments.iter().all(|a| a.filename.is_some()));
    let (msgs, atts, emb) = deep(&src);
    assert!(msgs >= 17 && atts >= 20 && emb >= 1, "{msgs} {atts} {emb}");
}

#[test]
fn aspose_unicode_ost_and_other_samples() {
    for (name, min_msgs) in [
        ("aspose-MAPI-SampleOstFile.ost", 92),
        ("aspose-MAPI-Sub.pst", 166),
        ("aspose-SMTP-Outlook.pst", 60),
        ("passworded.pst", 0),
        ("dist-list.pst", 0),
        ("Empty.pst", 0),
    ] {
        let Some(p) = fixture(name) else { continue };
        let src = PstSource::open(&p).unwrap();
        check_header_crc(&src);
        check_block_crcs(&src);
        let n = src.iter_headers().count();
        assert!(n >= min_msgs, "{name}: {n} headers");
        deep(&src);
    }
}
