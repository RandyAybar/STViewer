# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.1] - 2026-11-03

### Changed

- Faster on hot paths: opening an email loads its conversation at the same time,
  startup reads sources and indexing progress together, a newly added source opens while
  the source list refreshes, and tagging, saving or exporting a large selection fetches
  the missing list pages a few at a time instead of one by one.
- Save all / save selected attachments reads up to four attachments in parallel (files
  are still written one at a time, in order).
- macOS CI and release builds run on Intel runners; the universal app and the arm64 CLI
  are cross-compiled.

## [0.1.0] - 2026-11-03

First public release.

### Added

- Desktop app (Tauri 2, Svelte 5) for Windows, macOS and Linux, and the `stv`
  command-line tool. Both share one local library.
- Readers for Outlook PST/OST (ANSI, Unicode, 4K-page OST with zlib, permute/cyclic
  encoding), split multi-part PSTs, mbox (mboxo/mboxrd/mboxcl2), Gmail Takeout labels,
  Thunderbird profiles, Apple Mail (`.emlx`), `.eml` folders, Outlook `.msg` and TNEF
  (`winmail.dat`). Embedded messages open in place across formats.
- Incremental background full-text index (tantivy) over headers, bodies and attachment
  text (PDF, Office, OpenDocument, RTF, HTML, text). Gmail-style query syntax with field
  filters, phrases, fuzzy terms, flags, labels, and date and size ranges.
- Conversations across sources, with duplicate copies merged.
- Calendar, contact and task items: Outlook appointments, meeting requests, replies,
  cancellations and counter-proposals ("propose new time"), contacts and tasks (PST/OST,
  MSG, TNEF) and iCalendar invites/to-dos in MIME mail get a details card,
  folder-specific list columns with a Start sort, `is:appointment` / `is:meeting` /
  `is:contact` / `is:task` search terms, and vCard / iCalendar export. Search results
  show an appointment's start or a task's due date. All-day items keep their calendar
  date in every display time zone. Invites whose TZID has no VTIMEZONE (IANA name,
  Windows id or Outlook display name) resolve through a bundled time-zone database;
  floating times are read as UTC. Location, attendees, contact emails, phones and
  company are searchable. Agent access (`read_message`) returns the details too.
- Sorting search results by date uses the message date; the Start sort, `before:` /
  `after:` and the year facet use an item's start or due date.
- vCard / iCalendar export writes no file when nothing applies, writes each UID once
  (repeats of the same meeting are counted as duplicates) and skips counter-proposals.
- Attachments view with previews of images, PDF first pages and embedded document
  preview pictures (Quick Look on macOS), tags on messages and attachments, and export
  to EML, mbox, HTML, text, CSV and raw attachments. PDF pages render in a separate
  worker process with memory and time limits; previews run on their own threads with a
  size cap and a self-trimming disk cache.
- Headers view with SPF/DKIM/DMARC results and the delivery path. Display time zone and
  light/dark/system theme settings.
- Optional read-only MCP server ("Agent access") for local AI agents. It is off by
  default, bound to loopback, token-protected, and limited to the mailboxes you choose.
  `read_message` reports link mismatches (SafeLinks and Proofpoint wrappers unwrapped)
  and a transport summary (delivery hops, SPF/DKIM/DMARC, spam verdict). Attached
  emails can be read at any depth.
