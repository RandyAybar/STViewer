# testdata/msg

Fixtures for `stv-msg` (`crates/msg`).

## Synthetic fixtures

Most `.msg` / TNEF test inputs are generated at test time by the test-only writers in
`crates/msg/tests/common/mod.rs` (a `cfb`-based [MS-OXMSG] writer and an [MS-OXTNEF]
writer), so every asserted value is known exactly. Nothing generated is committed.

## `poi/` — real-world samples

Copied unmodified from the Apache POI test corpus,
<https://github.com/apache/poi/tree/trunk/test-data> (`hsmf/` for `.msg`, `hmef/` for TNEF),
licensed under the **Apache License 2.0** (<https://www.apache.org/licenses/LICENSE-2.0>).
Expected values in `crates/msg/tests/samples.rs` mirror the assertions in POI's own tests
(`TestBasics`, `TestFileWithAttachmentsRead`, `TestNameIdChunks`, `Test7BitCodepage`,
`TestHMEFMessage`, `TestAttachments`).

| File | Source path | What it covers |
| --- | --- | --- |
| `quick.msg`, `simple_test_msg.msg` | `hsmf/` | basic Unicode/ANSI messages, transport headers |
| `attachment_test_msg.msg` | `hsmf/` | two file attachments (24064 and 89 bytes) |
| `attachment_msg_pdf.msg` | `hsmf/` | PDF attachment + embedded message (`__substg1.0_3701000D`) |
| `58214_with_attachment.msg` | `hsmf/` | embedded message with HTML body |
| `example_received_unicode.msg` | `hsmf/` | Unicode strings, inline image with content id |
| `keywords.msg` | `hsmf/` | named properties: `Keywords` (PS_PUBLIC_STRINGS), PidLidCurrentVersionName |
| `cyrillic_message.msg`, `chinese-traditional.msg` | `hsmf/` | String8 in cp1251 / cp950 |
| `ASCII_CP1251_LCID1049.msg`, `ASCII_UTF-8_CP1252_LCID1031.msg`, `ASCII_UTF-8_CP1252_LCID1031_HTML.msg`, `HTMLBodyBinary_CP1251.msg`, `HTMLBodyBinary_UTF-8.msg` | `hsmf/` | String8 codepage selection (locale vs. internet codepage) |
| `msgClassAppointment.msg` | `hsmf/` | non-mail message class |
| `clusterfuzz-testcase-minimized-POIHSMFFuzzer-4735011465854976.msg` | `hsmf/` | corrupt compound file (malformed FAT) |
| `quick-winmail.dat` | `hmef/` | Outlook TNEF: compressed RTF body, five attachments |
| `quick-contents/quick.txt`, `quick-contents/message.rtf` | `hmef/quick-contents/` | expected attachment / RTF bytes for `quick-winmail.dat` |
| `winmail-sample1.dat`, `bug52400-winmail-simple.dat` | `hmef/` | further TNEF writers (attachments, HTML body) |
| `oom.tnef` | `hmef/` | corrupt TNEF claiming an oversized property |

Inspect any of them with `cargo run -p stv-msg --example msg-dump -- [--props] <file-or-dir>`.
