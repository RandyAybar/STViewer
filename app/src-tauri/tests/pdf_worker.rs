//! The PDF worker process: draws ordinary first pages, and fails (instead of taking the
//! machine's memory) on a file whose declared sizes are hostile.

use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Runs the app binary as a PDF worker on `pdf`.
fn worker(pdf: &[u8], side: u32) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_stviewer-app"))
        .args(["--stv-pdf-worker", &side.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(pdf);
    child.wait_with_output().unwrap()
}

/// A one-page PDF (200 × 100 pt): `content` is the page's content stream, `image` an
/// optional `/Im0` image XObject (dictionary entries, stream data).
fn pdf(content: &str, image: Option<(&str, &[u8])>) -> Vec<u8> {
    let mut objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R \
           /Resources << /XObject << /Im0 5 0 R >> >> >>"
            .to_vec(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()).into_bytes(),
    ];
    if let Some((dict, data)) = image {
        let mut obj =
            format!("<< /Type /XObject /Subtype /Image {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        obj.extend_from_slice(data);
        obj.extend_from_slice(b"\nendstream");
        objects.push(obj);
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
    );
    out
}

#[test]
fn draws_the_first_page_as_jpeg() {
    // Left half blue, right half white.
    let out = worker(&pdf("0 0 1 rg 0 0 100 100 re f", None), 256);
    assert!(out.status.success());
    let img = image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Jpeg).unwrap().to_rgb8();
    assert_eq!(img.dimensions(), (256, 128));
    let [r, g, b] = img.get_pixel(40, 64).0;
    assert!(b > 200 && r < 60 && g < 60, "left half is blue: {r} {g} {b}");
    assert!(img.get_pixel(216, 64).0.iter().all(|&c| c > 240), "right half is white");
}

#[test]
fn hostile_declared_sizes_hit_the_memory_budget() {
    // A few bytes of fax data declared as a 40000 × 40000 image: decoding it allocates
    // 1.6 GB up front. Without the budget the worker would allocate it, fail to decode
    // the data and draw a blank page successfully.
    let bomb = pdf(
        "q 200 0 0 100 0 0 cm /Im0 Do Q",
        Some((
            "/Width 40000 /Height 40000 /ColorSpace /DeviceGray /BitsPerComponent 1 \
             /Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns 40000 /Rows 40000 >>",
            &[0, 0, 0, 0],
        )),
    );
    let out = worker(&bomb, 256);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn rejects_what_is_not_a_pdf() {
    let out = worker(b"%PDF-1.7\nnot really", 256);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    // Bad side argument.
    assert!(!worker(&pdf("0 0 1 rg 0 0 100 100 re f", None), 0).status.success());
}
