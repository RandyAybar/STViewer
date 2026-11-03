# testdata/mbox

Fixtures for `stv-mbox` (`crates/mbox`). All files are synthetic. Except for
`calendar-edge.mbox`, they are written by the fixture generator in
`crates/mbox/tests/common/mod.rs`, which runs as part of the test suite and only rewrites a
file when its content changes, so the committed copies stay stable. Senders use
`example.com`/`example.org`/`gmail.com` placeholder addresses. Like the rest of the
repository, the fixtures are licensed MIT OR Apache-2.0.

| Path | Covers |
| --- | --- |
| `basic.mbox`, `single.eml` | MIME structure, encoded words, attachments, nested `message/rfc822` |
| `rd.mbox`, `cl2.mbox`, `crlf.mbox` | mboxrd `>From` quoting, `Content-Length` (mboxcl2), CRLF line endings |
| `gmail.mbox` | Gmail Takeout `X-Gmail-Labels` → virtual folders, threading |
| `smime.mbox` | clear-signed, opaque-signed and encrypted S/MIME messages |
| `thunderbird/` | Thunderbird profile `Mail/` tree: `.sbd` subfolders, `.msf` side files, stray files |
| `apple/`, `apple_export/` | Apple Mail V10 store (`.emlx`, `.partial.emlx`, external attachments) and an exported `.mbox` bundle |
| `emldir/` | folder of `.eml` files with nested subfolders and non-mail files |
| `corrupt/` | truncated mbox, malformed `.emlx`, random bytes — must fail cleanly, never panic |
| `calendar-edge.mbox` | hand-written meeting invites (CRLF): DST gap/overlap, VTIMEZONE vs. IANA/Windows TZIDs without one, all-day + `DURATION`, recurrence exceptions, `COUNTER`/`REPLY`, lowercase and tab-folded properties, end before start, VTODO, vCalendar 1.0, garbage, attendee roles |
