//! Byte-mutation sweep over valid `.msg` / TNEF inputs: every decoder entry point must
//! return `Ok` or an error — never panic (not even internally, behind the panic guard).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;

static PANICS: AtomicUsize = AtomicUsize::new(0);

fn msg_bytes() -> Vec<u8> {
    let inner = MsgB::default()
        .prop(0x0037, V::String8(b"inner".to_vec()))
        .with_attachment(AttB::File(vec![(K::Id(0x3701), V::Binary(vec![1, 2, 3]))]));
    build_msg(
        &MsgB::default()
            .prop(0x0037, V::Unicode("subject".into()))
            .prop(0x0E06, V::Time(filetime(1_000)))
            .named(K::Name(PS_PUBLIC_STRINGS, "Keywords".into()), V::MultiUnicode(vec!["k".into()]))
            .with_recipient(vec![(K::Id(0x3001), V::Unicode("r".into())), (K::Id(0x0C15), V::I32(1))])
            .with_attachment(AttB::File(vec![(K::Id(0x3701), V::Binary(vec![5; 300]))]))
            .with_attachment(AttB::Embedded(vec![], inner)),
    )
}

fn tnef_bytes() -> Vec<u8> {
    let inner = TnefB::new(1252).attr(LVL_MSG, ATT_SUBJECT, sz("in")).build();
    TnefB::new(1252)
        .attr(LVL_MSG, ATT_SUBJECT, sz("subject"))
        .attr(LVL_MSG, ATT_FROM, trp("n", "SMTP:a@b"))
        .attr(LVL_MSG, ATT_DATE_SENT, dtr(2020, 1, 2, 3, 4, 5))
        .attr(
            LVL_MSG,
            ATT_MSG_PROPS,
            prop_list(
                &[
                    (K::Name(PS_PUBLIC_STRINGS, "Keywords".into()), V::MultiUnicode(vec!["a".into(), "b".into()])),
                    (K::Id(0x1013), V::Binary(b"<p>x</p>".to_vec())),
                    (K::Id(0x6000), V::MultiI32(vec![1, 2])),
                ],
                &[],
            ),
        )
        .attr(LVL_MSG, ATT_RECIP_TABLE, recip_table(&[vec![(K::Id(0x3001), V::Unicode("r".into()))]]))
        .attr(LVL_ATT, ATT_ATTACH_REND_DATA, rend_data_file())
        .attr(LVL_ATT, ATT_ATTACH_DATA, vec![9; 40])
        .attr(LVL_ATT, ATT_ATTACH_REND_DATA, rend_data_file())
        .attr(LVL_ATT, ATT_ATTACHMENT, prop_list(&[(K::Id(0x3705), V::I32(5))], &[(0x3701, Obj(IID_IMESSAGE, inner))]))
        .build()
}

fn exercise(bytes: &[u8]) {
    let _ = stv_msg::message_from_msg_bytes(bytes, None);
    let _ = stv_msg::message_from_tnef_bytes(bytes, None);
    for i in 0..3 {
        let _ = stv_msg::attachment_from_msg_bytes(bytes, i);
        let _ = stv_msg::attachment_from_tnef_bytes(bytes, i);
    }
    let _ = stv_msg::decode_mapi(bytes, &[1]);
    let _ = stv_msg::decode_mapi(bytes, &[1, 0]);
}

#[test]
fn mutations_never_panic() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::SeqCst);
        prev(info);
    }));

    for base in [msg_bytes(), tnef_bytes()] {
        exercise(&base);
        // Truncations.
        for cut in (0..base.len()).step_by((base.len() / 97).max(1)) {
            exercise(&base[..cut]);
        }
        // Single-byte corruptions spread over the whole file (headers, FAT, directory, streams).
        let step = (base.len() / 1500).max(1);
        for pos in (0..base.len()).step_by(step) {
            for x in [0xFF, 0x80, 0x01] {
                let mut b = base.clone();
                b[pos] ^= x;
                exercise(&b);
            }
        }
    }
    assert_eq!(PANICS.load(Ordering::SeqCst), 0, "a decoder panicked (see stderr)");
}
