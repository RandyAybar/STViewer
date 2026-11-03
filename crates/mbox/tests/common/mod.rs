//! Fixture generator: writes small, deterministic sample mailboxes into `testdata/mbox/`
//! (only rewriting files whose content changed, so committed fixtures stay stable).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

pub const PDF: &[u8] = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n";
pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00";

pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for (n, c) in data.chunks(3).enumerate() {
        if n > 0 && n % 19 == 0 {
            out.push_str("\r\n");
        }
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/mbox")
}

fn put(rel: &str, data: &[u8]) {
    let p = root().join(rel);
    if std::fs::read(&p).ok().as_deref() == Some(data) {
        return;
    }
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, data).unwrap();
}

pub fn from_line(sender: &str, n: u32) -> String {
    format!("From {sender} Tue Mar  1 1{}:00:00 2022\n", n % 10)
}

/// The rich multipart message (attachments, inline image, nested rfc822, TNEF, .msg).
pub fn rich_message(eol: &str) -> String {
    let s = format!(
        "From: Rich Sender <rich@example.com>\n\
To: a@example.com\n\
Subject: Attachments galore\n\
Date: Wed, 02 Mar 2022 08:30:00 -0500\n\
Message-ID: <rich@example.com>\n\
MIME-Version: 1.0\n\
Content-Type: multipart/mixed; boundary=\"mix\"\n\
\n\
--mix\n\
Content-Type: multipart/related; boundary=\"rel\"\n\
\n\
--rel\n\
Content-Type: multipart/alternative; boundary=\"alt\"\n\
\n\
--alt\n\
Content-Type: text/plain; charset=utf-8\n\
\n\
Plain body \u{fc}n\u{ef}code\n\
--alt\n\
Content-Type: text/html; charset=iso-8859-1\n\
Content-Transfer-Encoding: quoted-printable\n\
\n\
<html><body><p>HTML body caf=E9</p><img src=3D\"cid:logo@x\"></body></html>\n\
--alt--\n\
--rel\n\
Content-Type: image/png\n\
Content-ID: <logo@x>\n\
Content-Transfer-Encoding: base64\n\
\n\
{png}\n\
--rel--\n\
--mix\n\
Content-Type: application/pdf; name=\"report.pdf\"\n\
Content-Disposition: attachment; filename=\"report.pdf\"\n\
Content-Transfer-Encoding: base64\n\
\n\
{pdf}\n\
--mix\n\
Content-Type: message/rfc822\n\
Content-Disposition: attachment\n\
\n\
From: Inner <inner@example.com>\n\
Subject: Inner message\n\
Message-ID: <inner@example.com>\n\
Content-Type: multipart/mixed; boundary=\"in\"\n\
\n\
--in\n\
Content-Type: text/plain\n\
\n\
Inner body\n\
--in\n\
Content-Type: message/rfc822\n\
\n\
From: Deep <deep@example.com>\n\
Subject: Deepest\n\
Content-Type: text/plain\n\
\n\
Deepest body\n\
--in\n\
Content-Type: text/csv; name=\"data.csv\"\n\
Content-Disposition: attachment; filename=\"data.csv\"\n\
\n\
a,b\n\
1,2\n\
--in--\n\
--mix\n\
Content-Type: application/ms-tnef; name=\"winmail.dat\"\n\
Content-Disposition: attachment; filename=\"winmail.dat\"\n\
Content-Transfer-Encoding: base64\n\
\n\
eJ8+IgAAAQ==\n\
--mix\n\
Content-Type: application/vnd.ms-outlook; name=\"fwd.msg\"\n\
Content-Disposition: attachment; filename=\"fwd.msg\"\n\
Content-Transfer-Encoding: base64\n\
\n\
0M8R4KGxGuE=\n\
--mix--\n",
        png = b64(PNG).replace("\r\n", "\n"),
        pdf = b64(PDF).replace("\r\n", "\n"),
    );
    s.replace('\n', eol)
}

fn basic_mbox() -> String {
    let mut s = String::new();
    s += &from_line("jurgen@example.com", 0);
    s += "From: =?utf-8?B?SsO8cmdlbg==?= <jurgen@example.com>\n\
To: Ann <ann@example.com>, bob@example.com\n\
Cc: \"Carol C.\" <carol@example.com>\n\
Subject: =?iso-8859-1?Q?Caf=E9?= meeting\n\
Date: Tue, 01 Mar 2022 10:00:00 +0100\n\
Message-ID: <m1@example.com>\n\
\n\
Hello team,\n\
From the desk of Jurgen: agenda attached later.\n\
>From escaped line\n\
\n";
    s += &from_line("ann@example.com", 1);
    s += "From: Ann <ann@example.com>\n\
To: jurgen@example.com\n\
Subject: Re: =?iso-8859-1?Q?Caf=E9?= meeting\n\
Message-ID: <m2@example.com>\n\
In-Reply-To: <m1@example.com>\n\
References: <m1@example.com>\n\
Status: O\n\
X-Status: F\n\
\n\
Sounds good.\n\
\n";
    s += &from_line("rich@example.com", 2);
    s += &rich_message("\n");
    s += "\n";
    s
}

fn crlf_mbox() -> Vec<u8> {
    let mut s = String::from("\u{feff}\r\n\r\n");
    for i in 0..3 {
        s += &format!(
            "From sender{i}@example.com Thu Jan  6 0{i}:00:00 2022\r\n\
From: Sender {i} <sender{i}@example.com>\r\n\
Subject: CRLF message {i}\r\n\
Date: Thu, 06 Jan 2022 0{i}:00:00 +0000\r\n\
\r\n\
Body line {i}\r\n\
\r\n"
        );
    }
    s.into_bytes()
}

fn rd_mbox() -> String {
    "From a@example.com Mon Feb  7 09:00:00 2022\n\
From: a@example.com\n\
Subject: mboxrd\n\
\n\
>From quoted once\n\
>>From quoted twice\n\
> From not an escape\n\
\n\
From b@example.com Mon Feb  7 09:05:00 2022\n\
From: b@example.com\n\
Subject: second\n\
\n\
done\n"
        .to_string()
}

fn cl2_mbox() -> String {
    let body1 = "Line one\nFrom someone Mon Jan  1 00:00:00 2001\nLine three\n";
    let body3 = "short body\n";
    format!(
        "From x@example.com Sat Apr  2 10:00:00 2022\n\
From: x@example.com\n\
Subject: cl2 one\n\
Content-Length: {}\n\
\n\
{body1}\n\
From y@example.com Sat Apr  2 11:00:00 2022\n\
From: y@example.com\n\
Subject: cl2 two\n\
Content-Length: 4\n\
\n\
body\n\
From z@example.com Sat Apr  2 12:00:00 2022\n\
From: z@example.com\n\
Subject: wrong length\n\
Content-Length: 999999\n\
\n\
{body3}\n\
From w@example.com Sat Apr  2 13:00:00 2022\n\
From: w@example.com\n\
Subject: after wrong\n\
\n\
tail\n",
        body1.len()
    )
}

fn smime_mbox() -> String {
    let inner = "Content-Type: multipart/mixed; boundary=\"sx\"\r\n\r\n--sx\r\n\
Content-Type: text/plain\r\n\r\nOpaque signed text\r\n--sx\r\n\
Content-Type: text/plain; name=\"note.txt\"\r\nContent-Disposition: attachment; filename=\"note.txt\"\r\n\r\n\
note\r\n--sx--\r\n";
    let der = stv_mbox::smime::encode_signed_data(inner.as_bytes());
    format!(
        "From s@example.com Sun May  1 10:00:00 2022\n\
From: Enc <enc@example.com>\n\
Subject: Encrypted\n\
MIME-Version: 1.0\n\
Content-Type: application/pkcs7-mime; smime-type=enveloped-data; name=\"smime.p7m\"\n\
Content-Disposition: attachment; filename=\"smime.p7m\"\n\
Content-Transfer-Encoding: base64\n\
\n\
MIAGCSqGSIb3DQEHA6CAMIACAQAxggE=\n\
\n\
From s@example.com Sun May  1 11:00:00 2022\n\
From: Signer <sig@example.com>\n\
Subject: Clear signed\n\
MIME-Version: 1.0\n\
Content-Type: multipart/signed; protocol=\"application/pkcs7-signature\"; micalg=sha-256; boundary=\"sig\"\n\
\n\
--sig\n\
Content-Type: text/plain\n\
\n\
Signed visible text\n\
--sig\n\
Content-Type: application/pkcs7-signature; name=\"smime.p7s\"\n\
Content-Disposition: attachment; filename=\"smime.p7s\"\n\
Content-Transfer-Encoding: base64\n\
\n\
MIIBAA==\n\
--sig--\n\
\n\
From s@example.com Sun May  1 12:00:00 2022\n\
From: Opaque <opaque@example.com>\n\
Subject: Opaque signed\n\
MIME-Version: 1.0\n\
Content-Type: application/pkcs7-mime; smime-type=signed-data; name=\"smime.p7m\"\n\
Content-Transfer-Encoding: base64\n\
\n\
{}\n",
        b64(&der).replace("\r\n", "\n")
    )
}

fn gmail_mbox() -> String {
    let m = |n: u32, labels: &str, thr: &str, subj: &str| {
        format!(
            "From 17{n}@xxx Sat Jan 01 0{n}:00:00 +0000 2022\n\
X-GM-THRID: {thr}\n\
X-Gmail-Labels: {labels}\n\
From: G{n} <g{n}@gmail.com>\n\
Subject: {subj}\n\
Date: Sat, 01 Jan 2022 0{n}:00:00 +0000\n\
Message-ID: <g{n}@gmail.com>\n\
\n\
gmail body {n}\n\
\n"
        )
    };
    m(1, "Inbox,Important,Opened", "1001", "Thread start")
        + &m(2, "\"Work, Projects\",Work/Sub,Unread", "2002", "Work item")
        + &m(3, "Inbox,Starred,Category Personal,Opened", "1001", "Re: Thread start")
}

fn tb_msg(i: u32, status: &str, subject: &str) -> String {
    format!(
        "From - Mon Jun  6 1{i}:00:00 2022\n\
X-Mozilla-Status: {status}\n\
X-Mozilla-Status2: 00000000\n\
From: TB {i} <tb{i}@example.com>\n\
Subject: {subject}\n\
Date: Mon, 06 Jun 2022 1{i}:00:00 +0000\n\
Message-ID: <tb{i}@example.com>\n\
\n\
Thunderbird body {i}\n\
\n"
    )
}

fn emlx(payload: &str, flags: u32) -> Vec<u8> {
    let mut v = format!("{}\n", payload.len()).into_bytes();
    v.extend_from_slice(payload.as_bytes());
    v.extend_from_slice(
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\
\t<key>date-received</key>\n\t<integer>1656000000</integer>\n\
\t<key>flags</key>\n\t<integer>{flags}</integer>\n</dict>\n</plist>\n"
        )
        .as_bytes(),
    );
    v
}

fn apple_msg(n: u32, subject: &str) -> String {
    format!(
        "From: Apple {n} <apple{n}@example.com>\n\
Subject: {subject}\n\
Date: Thu, 23 Jun 2022 1{n}:00:00 +0000\n\
Message-ID: <apple{n}@example.com>\n\
\n\
Apple body {n}\n"
    )
}

fn generate() -> PathBuf {
    put("basic.mbox", basic_mbox().as_bytes());
    put("crlf.mbox", &crlf_mbox());
    put("rd.mbox", rd_mbox().as_bytes());
    put("cl2.mbox", cl2_mbox().as_bytes());
    put("smime.mbox", smime_mbox().as_bytes());
    put("gmail.mbox", gmail_mbox().as_bytes());

    // Thunderbird profile `Mail/` tree.
    let tb = "thunderbird/Mail/Local Folders";
    put(
        &format!("{tb}/Inbox"),
        (tb_msg(1, "0001", "TB read")
            + &tb_msg(2, "0000", "TB unread")
            + &tb_msg(3, "0009", "TB deleted")
            + &tb_msg(4, "0005", "TB flagged"))
            .as_bytes(),
    );
    put(&format!("{tb}/Inbox.msf"), b"// <!-- <mdb:mork:z v=\"1.4\"/> -->\n");
    put(&format!("{tb}/Inbox.sbd/Work"), tb_msg(5, "0001", "TB work").as_bytes());
    put(&format!("{tb}/Inbox.sbd/Work.msf"), b"mork");
    put(&format!("{tb}/Trash"), b"");
    put(&format!("{tb}/Trash.msf"), b"mork");
    put(&format!("{tb}/Sent.sbd/Archive"), tb_msg(6, "0001", "TB archived sent").as_bytes());
    put(&format!("{tb}/filterlog.html"), b"<html></html>");
    put(&format!("{tb}/popstate.dat"), b"# POP3 State File\n");
    put("thunderbird/Mail/folderTree.json", b"{}");
    put("thunderbird/Mail/stray.txt", b"From nobody Mon Jun  6 10:00:00 2022 (txt is ignored)\n");

    // Directory of .eml files.
    let eml = |n: u32, subj: &str| {
        format!(
            "From: E{n} <e{n}@example.com>\r\nSubject: {subj}\r\nDate: Fri, 0{n} Jul 2022 10:00:00 +0000\r\n\
Message-ID: <e{n}@example.com>\r\n\r\nEml body {n}\r\n"
        )
    };
    put("emldir/a.eml", eml(1, "Top level").as_bytes());
    put("emldir/sub/b.eml", eml(2, "In sub").as_bytes());
    put("emldir/sub/deeper/c.eml", eml(3, "Deeper").as_bytes());
    put("emldir/sub/deeper/d.EML", eml(4, "Upper ext").as_bytes());
    put("emldir/notes.txt", b"not mail");
    put("emldir/empty/readme.md", b"no mail here");
    put("single.eml", rich_message("\r\n").as_bytes());

    // Apple Mail V10 tree.
    let inbox = "apple/V10/1A2B3C4D-0000-0000-0000-000000000001/INBOX.mbox/9F8E7D6C-0000-0000-0000-000000000002/Data";
    put(&format!("{inbox}/Messages/1.emlx"), &emlx(&apple_msg(1, "Apple read"), 1));
    put(
        &format!("{inbox}/Messages/2.partial.emlx"),
        &emlx(
            "From: Apple 2 <apple2@example.com>\n\
Subject: Apple partial\n\
Date: Thu, 23 Jun 2022 12:00:00 +0000\n\
Content-Type: multipart/mixed; boundary=\"p\"\n\
\n\
--p\n\
Content-Type: text/plain\n\
\n\
See attached\n\
--p\n\
Content-Type: application/pdf; name=\"report.pdf\"\n\
Content-Disposition: attachment; filename=\"report.pdf\"\n\
Content-Transfer-Encoding: base64\n\
\n\
\n\
--p--\n",
            0,
        ),
    );
    put(&format!("{inbox}/Attachments/2/2/report.pdf"), PDF);
    put(&format!("{inbox}/9/Messages/90.emlx"), &emlx(&apple_msg(9, "Apple partitioned flagged"), 1 | 16));
    put(&format!("{inbox}/Messages/4.emlx"), &emlx(&apple_msg(4, "Apple deleted"), 1 | 2));
    put(
        "apple/V10/1A2B3C4D-0000-0000-0000-000000000001/Work.mbox/Messages/3.emlx",
        &emlx(&apple_msg(3, "Apple work"), 0),
    );
    put(
        "apple/V10/1A2B3C4D-0000-0000-0000-000000000001/Work.mbox/Sub.mbox/Messages/5.emlx",
        &emlx(&apple_msg(5, "Apple nested"), 1),
    );
    put("apple/V10/MailData/Envelope Index", b"SQLite format 3\0");
    // Exported Apple Mail bundle (contains a plain mbox).
    put(
        "apple_export/Archive.mbox/mbox",
        (tb_msg(7, "0001", "Exported one") + &tb_msg(8, "0001", "Exported two")).as_bytes(),
    );
    put("apple_export/Archive.mbox/table_of_contents", b"\x00\x01binary");

    // Corrupt inputs.
    let garbage: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    put("corrupt/garbage.bin", &garbage);
    put("corrupt/truncated.mbox", b"From a@b Mon Feb  1 10:00:00 2021\nSubject: cut off\nX-Long: aaaa");
    put("corrupt/bad.emlx", b"99999\nSubject: short\n\nbody");
    put("corrupt/nocount.emlx", b"Subject: no count line\n\nbody");
    root()
}

static FIXTURES: LazyLock<PathBuf> = LazyLock::new(generate);

/// Generates (once per test binary) and returns the fixture root.
pub fn fixtures() -> &'static Path {
    &FIXTURES
}
