//! Format-neutral message model shared by every STViewer crate.
//!
//! Source crates (`stv-pst`, `stv-mbox`, `stv-msg`) implement [`MailSource`].
//! `stv-index` indexes any `MailSource`; `stv-library` owns a registry of open
//! sources plus the index, and is the only API the CLI and the app call.
//!
//! Conventions:
//! - Timestamps are UTC milliseconds since the Unix epoch (`i64`).
//! - Strings are already decoded to UTF-8 (charset / MAPI codepage handled by the source).
//! - Everything listed by `list`/`iter_headers` is cheap; bodies and attachment bytes
//!   are only produced by `message`/`attachment`.

pub mod time;

use std::ops::Range;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Result with [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error shared by every STViewer crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The bytes do not match the expected structure (corrupt or unknown variant).
    #[error("format error: {0}")]
    Format(String),
    /// The requested folder, message, attachment or source does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// Valid input this build cannot handle (unknown variant, unsupported operation).
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// Content is encrypted (S/MIME, password-protected PST) and cannot be decoded.
    #[error("encrypted content: {0}")]
    Encrypted(String),
    /// The operation was cancelled through its cancel flag.
    #[error("cancelled")]
    Cancelled,
    /// The library's data directory is locked by another process.
    #[error("this library is in use by another STViewer process{}", pid.map(|p| format!(" (pid {p})")).unwrap_or_default())]
    InUse { pid: Option<u32> },
    /// Anything else (invalid search query, index failure, ...); the message is user-facing.
    #[error("{0}")]
    Other(String),
}

/// Identifies an open source inside a `Library`. Stable across restarts (persisted registry).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceId(pub u32);

/// Container format of a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Outlook personal folders file (ANSI or Unicode, incl. 4K-page and split archives).
    Pst,
    /// Outlook offline storage (cached Exchange mailbox).
    Ost,
    /// A single mbox file (incl. Gmail Takeout exports).
    Mbox,
    /// Directory tree of mbox files (Thunderbird profile / `Mail` folder with `.sbd` dirs).
    Thunderbird,
    /// A single `.eml` file or a directory of `.eml` files.
    Eml,
    /// Apple Mail `.mbox` bundle(s) containing `.emlx` files.
    AppleMail,
    /// A single Outlook `.msg` file or a directory of them.
    Msg,
    /// A standalone `winmail.dat` / `.tnef` file.
    Tnef,
}

/// Opaque, source-defined folder identifier. Must round-trip through `folders()` → `list()`.
/// Examples: PST node id `"nid:8022"`, mbox `"Inbox.sbd/Work"`, Gmail label `"label:Important"`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FolderId(pub String);

/// Opaque, source-defined pointer to one message. Must be stable for a given file
/// fingerprint so the search index can store it and reopen the message later.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Locator {
    /// PST/OST message node id.
    Pst { nid: u32 },
    /// Byte range of one message inside an mbox file (`From ` line excluded).
    /// `file` is relative to the source root for multi-file sources, empty for single-file.
    Mbox { file: String, offset: u64, len: u64 },
    /// A standalone file (eml, emlx, msg, tnef), relative to the source root (empty if the source is that file).
    File { path: String },
    /// A message attached to another message (embedded .msg / message/rfc822 / TNEF).
    Embedded { parent: Box<Locator>, attachment: u32 },
    /// A message in one file of a multi-part archive (a mailbox split across
    /// `name.001.pst`, `name.002.pst`, ...); `part` is the 0-based file index.
    Part { part: u32, inner: Box<Locator> },
}

/// What a folder holds (Outlook container class); `Mail` unless the format says otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderKind {
    #[default]
    Mail,
    Calendar,
    Contacts,
    Tasks,
    Notes,
    Journal,
    /// Virtual folder derived from labels (Gmail `X-GM-LABELS`).
    Label,
    /// Messages a mailer marked deleted but did not expunge (Thunderbird `X-Mozilla-Status`).
    Deleted,
    Other,
}

/// One node of a source's folder tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub id: FolderId,
    /// `None` for a root folder.
    pub parent: Option<FolderId>,
    /// Display name (not a path).
    pub name: String,
    pub kind: FolderKind,
    /// Number of messages directly in this folder, if known without a scan.
    pub message_count: Option<u64>,
    /// Unread messages directly in this folder, if known without a scan.
    pub unread_count: Option<u64>,
}

/// A mailbox: display name and/or address (either may be missing, e.g. Exchange senders
/// without an SMTP address).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// Per-message status flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFlags {
    pub read: bool,
    pub flagged: bool,
    pub draft: bool,
    pub deleted: bool,
    pub encrypted: bool,
    pub signed: bool,
}

/// Everything needed to render a list row, thread, de-duplicate, and build an index document header.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// Where the message lives; set on everything `list`/`iter_headers` yields.
    pub locator: Option<Locator>,
    /// Folder the message was listed in.
    pub folder: Option<FolderId>,
    pub subject: String,
    pub from: Option<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    /// Sent/received date, UTC ms since epoch.
    pub date: Option<i64>,
    /// Size in bytes as reported by the container (approximate is fine).
    pub size: u64,
    /// The message has at least one non-inline attachment.
    pub has_attachments: bool,
    pub flags: MessageFlags,
    /// `Message-ID` without angle brackets.
    pub message_id: Option<String>,
    /// `In-Reply-To` Message-ID, without angle brackets.
    pub in_reply_to: Option<String>,
    /// `References` Message-IDs, oldest first, without angle brackets.
    pub references: Vec<String>,
    /// Native conversation key if the format has one (Gmail `X-GM-THRID`, MAPI conversation index root hex).
    pub conversation: Option<String>,
    /// Extra labels (Gmail labels, Outlook categories).
    pub labels: Vec<String>,
    /// MAPI message class (`IPM.Note`, `IPM.Appointment`, ...) or `None` for plain MIME mail.
    pub message_class: Option<String>,
    /// Calendar, contact or task details when the item is not plain mail (an Outlook
    /// appointment, meeting request, contact or task; a MIME mail carrying `text/calendar`
    /// or a vCard). List rows carry what the container's table holds (no attendees);
    /// [`MailSource::message`] fills in everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemDetails>,
}

impl Header {
    /// Sort/filter time for [`SortKey::Start`] and calendar date searches: appointment start
    /// or task due date, else [`Header::date`].
    pub fn start_ms(&self) -> Option<i64> {
        let when = match &self.item {
            Some(ItemDetails::Appointment(a)) => a.start.as_ref(),
            Some(ItemDetails::Task(t)) => t.due.as_ref(),
            _ => None,
        };
        when.and_then(When::to_ms).or(self.date)
    }
}

impl When {
    /// UTC ms; a [`When::Date`] is midnight UTC of that day. `None` for a date that is not a
    /// valid `YYYY-MM-DD` calendar day.
    pub fn to_ms(&self) -> Option<i64> {
        match self {
            When::Time(ms) => Some(*ms),
            When::Date(d) => {
                let b = d.as_bytes();
                let digits = |r: std::ops::Range<usize>| {
                    b[r].iter().try_fold(0u32, |n, &c| c.is_ascii_digit().then(|| n * 10 + u32::from(c - b'0')))
                };
                if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
                    return None;
                }
                let (y, m, day) = (i64::from(digits(0..4)?), digits(5..7)?, digits(8..10)?);
                let days = crate::time::days_from_civil(y, m, day);
                // Out-of-range months/days roll over in `days_from_civil`; reject them.
                (crate::time::civil_from_days(days) == (y, m, day)).then_some(days * crate::time::MS_PER_DAY)
            }
        }
    }
}

/// A point in time, or a calendar day (all-day appointments, birthdays, task dates) that
/// must be shown as that date in every time zone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum When {
    /// UTC ms since epoch.
    Time(i64),
    /// `YYYY-MM-DD`.
    Date(String),
}

/// Non-mail item details. See [`Header::item`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemDetails {
    Appointment(Appointment),
    Contact(Contact),
    Task(Task),
}

/// Appointment, or a meeting request/response/cancellation about one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Appointment {
    /// [`When::Date`] for all-day items (end exclusive, as stored).
    pub start: Option<When>,
    /// Like `start`.
    pub end: Option<When>,
    /// All-day event: `start`/`end` are [`When::Date`]s.
    pub all_day: bool,
    pub location: Option<String>,
    /// Meeting organizer (for a response, the organizer it is addressed to).
    pub organizer: Option<Address>,
    /// Required attendees.
    pub required: Vec<Address>,
    /// Optional attendees.
    pub optional: Vec<Address>,
    /// Rooms and equipment.
    pub resources: Vec<Address>,
    pub busy_status: Option<BusyStatus>,
    pub recurring: bool,
    /// Human-readable recurrence as the client stored it (Outlook `PidLidRecurrencePattern`,
    /// or an iCalendar `RRULE` verbatim).
    pub recurrence: Option<String>,
    /// Set when the item is a meeting message rather than the calendar entry itself.
    pub meeting: Option<MeetingKind>,
    /// iCalendar `UID` (also when Outlook kept it inside the global object id), else the
    /// Outlook global object id as upper-case hex; links requests to their entry.
    pub uid: Option<String>,
}

/// Free/busy state the item shows as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyStatus {
    Free,
    Tentative,
    Busy,
    OutOfOffice,
    WorkingElsewhere,
}

/// What a meeting message does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingKind {
    /// Invitation or update (`IPM.Schedule.Meeting.Request`, iCalendar `METHOD:REQUEST`).
    Request,
    /// `IPM.Schedule.Meeting.Canceled`, `METHOD:CANCEL`.
    Cancellation,
    /// `IPM.Schedule.Meeting.Resp.Pos`, `PARTSTAT=ACCEPTED`.
    Accepted,
    /// `IPM.Schedule.Meeting.Resp.Tent`, `PARTSTAT=TENTATIVE`.
    Tentative,
    /// `IPM.Schedule.Meeting.Resp.Neg`, `PARTSTAT=DECLINED`.
    Declined,
    /// An attendee proposes a new time (iCalendar `METHOD:COUNTER`; an Outlook response with
    /// `PidLidAppointmentCounterProposal`). `start`/`end` are the proposed times.
    Counter,
}

/// Address-book contact.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub company: Option<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    /// In the order the client lists them (Outlook Email1..3). An Exchange-internal entry
    /// without a known SMTP address has only a name.
    pub emails: Vec<Address>,
    /// Phone numbers labelled `mobile`, `business`, `home`, `business fax`, … .
    pub phones: Vec<Labeled>,
    /// Postal addresses, one multi-line string each, labelled `business`, `home` or `other`.
    pub addresses: Vec<Labeled>,
    /// A calendar day ([`When::Date`]).
    pub birthday: Option<When>,
    pub web_page: Option<String>,
}

/// A value with its label (`mobile`, `business`, `home`, …).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labeled {
    pub label: String,
    pub value: String,
}

/// To-do item or task request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub status: Option<TaskStatus>,
    /// 0–100.
    pub percent_complete: Option<u8>,
    /// Calendar days ([`When::Date`]) for Outlook tasks, as the user entered them.
    pub start: Option<When>,
    /// Like `start`.
    pub due: Option<When>,
    /// Like `start`.
    pub completed: Option<When>,
    pub owner: Option<String>,
    /// iCalendar `UID`, else the Outlook task global id (`PidLidTaskGlobalId`) as upper-case
    /// hex; identifies the task across copies and exports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
}

/// Task progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    NotStarted,
    InProgress,
    Complete,
    /// Waiting on someone else.
    Waiting,
    Deferred,
}

/// Message body in every form the container provides.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Body {
    /// Plain-text body.
    pub text: Option<String>,
    /// HTML body (incl. HTML de-encapsulated from RTF); `cid:` references point at attachments.
    pub html: Option<String>,
    /// Decompressed RTF, only when neither `html` (incl. RTF-encapsulated HTML) nor `text` captures it.
    pub rtf: Option<String>,
}

/// How an attachment is stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// Ordinary file bytes.
    #[default]
    File,
    /// Another message; open it with `Locator::Embedded`.
    EmbeddedMessage,
    /// winmail.dat / application/ms-tnef; also openable with `Locator::Embedded`.
    Tnef,
    /// Reference to an external file or URL; no bytes to open.
    Link,
    /// OLE object embedded by an Outlook editor.
    Ole,
}

/// An attachment as listed with its message (bytes come from `MailSource::attachment`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMeta {
    /// Index used by `MailSource::attachment` and `Locator::Embedded`.
    pub index: u32,
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    /// Size in bytes as reported by the container.
    pub size: u64,
    /// For inline images referenced from HTML as `cid:<content_id>`.
    pub content_id: Option<String>,
    /// Rendered inside the body (signature logos, `cid:` images) rather than attached.
    pub inline: bool,
    pub kind: AttachmentKind,
}

/// One raw property for the "inspect properties" view (MAPI tags, or MIME headers).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertyView {
    /// `0x0037001F` for MAPI, header name for MIME.
    pub tag: String,
    /// Canonical name if known (`PidTagSubject`), else empty.
    pub name: String,
    /// MAPI property type (`PtypString`, ...) or `MIME header`.
    pub type_name: String,
    /// Value rendered as text.
    pub value: String,
}

/// A fully loaded message.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub header: Header,
    pub body: Body,
    pub attachments: Vec<AttachmentMeta>,
    /// Transport headers as text, if available (`PidTagTransportMessageHeaders` or MIME header block).
    pub raw_headers: Option<String>,
}

/// Column a message list is ordered by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Date,
    /// Sender display name, else address.
    From,
    /// Subject with reply/forward prefixes ignored.
    Subject,
    Size,
    /// Container order (file order / table order). Cheapest.
    Native,
    /// Appointment start / task due date ([`Header::item`]); items without one sort as by
    /// [`SortKey::Date`]. Calendar and task folders default to it.
    Start,
}

/// Sort column and direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

/// Identity of a source file's content, used to detect a stale index.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint {
    pub path: PathBuf,
    /// File size in bytes (sum over files for multi-file sources).
    pub size: u64,
    /// Modification time, ms since epoch.
    pub mtime: i64,
    /// Hash of a fixed prefix (e.g. first 64 KiB) to tell an append from a rewrite.
    pub head_hash: u64,
}

/// Read-only view of one mail container. Implementations must be cheap to share
/// across threads (indexing runs on a background pool while the UI lists folders).
pub trait MailSource: Send + Sync {
    fn kind(&self) -> SourceKind;
    /// Human-readable name (usually the file or directory name).
    fn display_name(&self) -> String;
    /// Cheap identity of the current content; a change triggers an incremental re-index.
    fn fingerprint(&self) -> Result<Fingerprint>;

    /// Full folder tree, parents before children. Single-file sources return one root folder.
    fn folders(&self) -> Result<Vec<Folder>>;
    /// Number of messages directly in `folder`.
    fn count(&self, folder: &FolderId) -> Result<u64>;
    /// Page of headers in `folder` ordered by `sort`. Must be fast for any `range` on 100k+ folders.
    fn list(&self, folder: &FolderId, range: Range<u64>, sort: Sort) -> Result<Vec<Header>>;
    /// Every message header in the source, any order; used for indexing / threading / dedupe.
    /// Each yielded header has `locator` and `folder` set.
    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_>;

    /// Loads one message (body, attachment list).
    fn message(&self, loc: &Locator) -> Result<Message>;
    /// Bytes of attachment `index` of the message at `loc`.
    fn attachment(&self, loc: &Locator, index: u32) -> Result<Vec<u8>>;
    /// Native RFC 5322 bytes for MIME-based sources; `Ok(None)` for MAPI sources
    /// (the library synthesises MIME from `message()` when exporting).
    fn raw_mime(&self, loc: &Locator) -> Result<Option<Vec<u8>>>;
    /// Raw properties of the message at `loc` for inspection (MAPI tags or MIME headers).
    /// Kept out of `message()`: only the inspector needs them and rendering is costly. No
    /// default, so wrapper sources cannot forget to forward it.
    fn properties(&self, loc: &Locator) -> Result<Vec<PropertyView>>;
}

/// Constructor entry point each source crate exports as `pub fn opener() -> &'static dyn SourceOpener`.
pub trait SourceOpener: Send + Sync {
    /// Cheap check (magic bytes / extension / directory layout). No full parse.
    fn sniff(&self, path: &std::path::Path) -> Option<SourceKind>;
    /// Opens the source at `path` (the full parse happens here or lazily).
    fn open(&self, path: &std::path::Path) -> Result<Box<dyn MailSource>>;
}

// ---- Search contract (implemented by stv-index, surfaced by stv-library) ----

/// Page, filters and extras of a search.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchOptions {
    /// Hits to skip (paging).
    pub offset: usize,
    /// Hits to return; 0 = the index default (100).
    pub limit: usize,
    /// Restrict to these sources; empty = all.
    pub sources: Vec<SourceId>,
    /// Restrict to this folder. Folder ids are per source: pair it with `sources`.
    pub folder: Option<FolderId>,
    /// `None` = relevance.
    pub sort: Option<Sort>,
    /// Compute facet counts (top senders, years, sources).
    pub facets: bool,
}

/// One search result: the stored header, so it renders without opening the source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub source: SourceId,
    pub header: Header,
    /// Relevance score; 0 when results are sorted by a column.
    pub score: f32,
    /// HTML-escaped snippet with matches wrapped in `<b>`.
    pub snippet: String,
}

/// One facet bucket: a value and the number of matching messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetCount {
    pub value: String,
    pub count: u64,
}

/// One page of search results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchResults {
    /// Messages matching the query (all pages).
    pub total: u64,
    pub hits: Vec<SearchHit>,
    /// Search time in the index.
    pub took_ms: u64,
    /// Top senders (address, or display name when there is none), most frequent first.
    pub facets_from: Vec<FacetCount>,
    /// Matches per year (UTC), newest first.
    pub facets_year: Vec<FacetCount>,
    /// Matches per source (`value` is the `SourceId` number), most first.
    pub facets_source: Vec<FacetCount>,
}

/// Indexing progress of one source, reported while a sync runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexProgress {
    pub source: Option<SourceId>,
    /// Messages processed so far (already indexed ones included).
    pub indexed: u64,
    /// `None` until the total is known.
    pub total: Option<u64>,
    /// The sync finished (successfully, possibly with `read_errors`, or with a fatal `error`).
    pub done: bool,
    /// Why the sync failed (fatal), or `"cancelled"`; per-message failures are counted in `read_errors`.
    pub error: Option<String>,
    /// Messages that could not be read and were indexed from their listing header only
    /// (retried next sync). Not a sync failure.
    #[serde(default)]
    pub read_errors: u64,
}

// ---- Attachment listing (one row per attachment, recorded while indexing) ----

/// Coarse attachment type, derived from MIME type and file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentCategory {
    Image,
    Pdf,
    Document,
    Spreadsheet,
    Presentation,
    Archive,
    /// Attached / embedded e-mail (`.eml`, `.msg`, message/rfc822, embedded MAPI message).
    Email,
    Calendar,
    Audio,
    Video,
    Text,
    Other,
}

impl AttachmentCategory {
    /// Every category, in display order.
    pub const ALL: [AttachmentCategory; 12] = [
        Self::Image,
        Self::Pdf,
        Self::Document,
        Self::Spreadsheet,
        Self::Presentation,
        Self::Archive,
        Self::Email,
        Self::Calendar,
        Self::Audio,
        Self::Video,
        Self::Text,
        Self::Other,
    ];

    /// Stable lowercase name (also the serde form).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Document => "document",
            Self::Spreadsheet => "spreadsheet",
            Self::Presentation => "presentation",
            Self::Archive => "archive",
            Self::Email => "email",
            Self::Calendar => "calendar",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Text => "text",
            Self::Other => "other",
        }
    }

    /// Classify by lowercase extension (no dot), falling back to the MIME type.
    pub fn classify(extension: &str, mime: Option<&str>, kind: AttachmentKind) -> Self {
        if kind == AttachmentKind::EmbeddedMessage {
            return Self::Email;
        }
        let by_ext = match extension {
            "jpg" | "jpeg" | "jpe" | "png" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "heic" | "heif" | "ico"
            | "svg" | "emf" | "wmf" | "pcx" => Some(Self::Image),
            "pdf" => Some(Self::Pdf),
            "doc" | "docx" | "docm" | "dot" | "dotx" | "odt" | "rtf" | "pages" | "wpd" | "pub" => Some(Self::Document),
            "xls" | "xlsx" | "xlsm" | "xlsb" | "xlt" | "xltx" | "ods" | "csv" | "numbers" => Some(Self::Spreadsheet),
            "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "pot" | "potx" | "odp" | "key" => Some(Self::Presentation),
            "zip" | "7z" | "rar" | "gz" | "tgz" | "tar" | "bz2" | "xz" | "cab" | "iso" => Some(Self::Archive),
            "eml" | "msg" | "emlx" | "mbox" => Some(Self::Email),
            "ics" | "vcs" => Some(Self::Calendar),
            "mp3" | "wav" | "m4a" | "aac" | "wma" | "ogg" | "flac" | "amr" => Some(Self::Audio),
            "mp4" | "mov" | "avi" | "wmv" | "mkv" | "m4v" | "webm" | "3gp" | "mpg" | "mpeg" | "mpe" => {
                Some(Self::Video)
            }
            "txt" | "log" | "md" | "xml" | "json" | "htm" | "html" | "vcf" => Some(Self::Text),
            _ => None,
        };
        if let Some(c) = by_ext {
            return c;
        }
        let mime = mime_essence(mime);
        match mime.split('/').next().unwrap_or("") {
            "image" => Self::Image,
            "audio" => Self::Audio,
            "video" => Self::Video,
            "text" if mime == "text/calendar" => Self::Calendar,
            "text" => Self::Text,
            "message" => Self::Email,
            _ if mime == "application/pdf" => Self::Pdf,
            _ if mime == "application/ms-tnef" || kind == AttachmentKind::Tnef => Self::Email,
            _ => Self::Other,
        }
    }
}

/// Column the attachment listing is ordered by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentSortKey {
    /// File name, case-insensitive.
    Name,
    Size,
    /// Date of the message carrying the attachment.
    #[default]
    Date,
    /// File extension, then name.
    Type,
    /// Sender of the message carrying the attachment.
    From,
}

/// Sort column and direction of the attachment listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttachmentSort {
    pub key: AttachmentSortKey,
    pub descending: bool,
}

/// Filters, sort and page of the attachment listing. Every field is optional in JSON.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AttachmentQuery {
    /// Restrict to these sources; empty = all.
    pub sources: Vec<SourceId>,
    /// Words matched against file name, sender and message subject; every word must
    /// prefix-match (so results narrow while typing: `rep` finds `report.pdf`). A word
    /// starting with `-` excludes attachments matching it (`-image0 -logo`). Empty = everything.
    pub text: String,
    /// Restrict to these categories; empty = all.
    pub categories: Vec<AttachmentCategory>,
    /// Hide these categories (applied after `categories`).
    pub exclude_categories: Vec<AttachmentCategory>,
    /// Hide these extensions (case-insensitive, leading dot optional: `"png"`, `".ics"`).
    pub exclude_extensions: Vec<String>,
    /// Only attachments of at least / at most this many bytes.
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    /// Include inline parts (signature logos, `cid:` images). Default false.
    pub include_inline: bool,
    pub sort: AttachmentSort,
    /// Rows to skip (paging).
    pub offset: usize,
    /// Rows to return; 0 = 100.
    pub limit: usize,
}

/// One row of the attachment listing (stored with the index, rendered without opening the source).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttachmentRow {
    pub source: SourceId,
    /// The message carrying the attachment (open it with `message`).
    pub message: Locator,
    /// Attachment index within that message (for `attachment` / open / save).
    pub index: u32,
    /// File name, or a generated `attachment-<n>` / embedded subject when missing.
    pub filename: String,
    pub mime_type: Option<String>,
    /// Lowercase extension without the dot; empty when none.
    pub extension: String,
    pub category: AttachmentCategory,
    pub kind: AttachmentKind,
    pub size: u64,
    pub inline: bool,
    /// Date of the carrying message, UTC ms.
    pub date: Option<i64>,
    /// Sender, subject and folder of the carrying message.
    pub from: Option<Address>,
    pub subject: String,
    pub folder: Option<FolderId>,
}

impl AttachmentRow {
    /// Listing row for attachment `meta` of the message described by `header` (which must
    /// carry its locator). `embedded_subject` names an embedded message without a file
    /// name. Link attachments (nothing to open) yield `None`.
    pub fn new(
        source: SourceId,
        header: &Header,
        meta: &AttachmentMeta,
        embedded_subject: Option<&str>,
    ) -> Option<Self> {
        let message = header.locator.clone()?;
        if meta.kind == AttachmentKind::Link {
            return None;
        }
        let ext = attachment_extension(meta.filename.as_deref(), meta.mime_type.as_deref(), meta.kind);
        let filename = match meta.filename.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => match embedded_subject.filter(|s| !s.trim().is_empty()) {
                Some(s) => format!("{s}.{}", if ext.is_empty() { "msg" } else { &ext }),
                None if ext.is_empty() => format!("attachment-{}", meta.index + 1),
                None => format!("attachment-{}.{ext}", meta.index + 1),
            },
        };
        Some(AttachmentRow {
            source,
            message,
            index: meta.index,
            filename,
            mime_type: meta.mime_type.clone(),
            category: AttachmentCategory::classify(&ext, meta.mime_type.as_deref(), meta.kind),
            extension: ext,
            kind: meta.kind,
            size: meta.size,
            inline: meta.inline,
            date: header.date,
            from: header.from.clone(),
            subject: header.subject.clone(),
            folder: header.folder.clone(),
        })
    }
}

/// Lowercase extension of `name` (no dot), or one derived from the MIME type.
pub fn attachment_extension(name: Option<&str>, mime: Option<&str>, kind: AttachmentKind) -> String {
    if let Some(ext) = name.and_then(|n| n.rsplit_once('.')).map(|(_, e)| e.trim().to_lowercase()) {
        if !ext.is_empty() && ext.len() <= 10 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return ext;
        }
    }
    if kind == AttachmentKind::EmbeddedMessage {
        return "msg".into();
    }
    match mime_essence(mime).as_str() {
        "image/jpeg" | "image/jpg" | "image/pjpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "message/rfc822" => "eml",
        "text/calendar" => "ics",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/ms-tnef" => "dat",
        _ => "",
    }
    .to_string()
}

/// Lowercase `type/subtype` of a MIME type, without parameters (`; name=…`, `; charset=…`).
fn mime_essence(mime: Option<&str>) -> String {
    mime.unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AttachmentPage {
    /// Rows matching the whole query.
    pub total: u64,
    pub rows: Vec<AttachmentRow>,
    /// Per-category counts for the query ignoring its `categories` filter (for filter chips).
    pub category_counts: Vec<(AttachmentCategory, u64)>,
    pub took_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(index: u32, filename: Option<&str>, mime: Option<&str>, kind: AttachmentKind) -> AttachmentMeta {
        AttachmentMeta {
            index,
            filename: filename.map(Into::into),
            mime_type: mime.map(Into::into),
            kind,
            ..Default::default()
        }
    }

    #[test]
    fn calendar_days_convert_only_when_valid() {
        let ms = |d: &str| When::Date(d.into()).to_ms();
        assert_eq!(ms("1970-01-02"), Some(86_400_000));
        assert_eq!(ms("2024-02-29"), Some(19_782 * 86_400_000));
        for bad in
            ["2023-02-29", "2026-13-01", "2026-00-10", "2026-04-31", "2026-1-1", "+202-01-01", "2026-01-01T00", ""]
        {
            assert_eq!(ms(bad), None, "{bad:?}");
        }
        assert_eq!(ms("99999999999999999-01-01"), None, "no overflow");
        assert_eq!(When::Time(-5).to_ms(), Some(-5));
    }

    #[test]
    fn start_is_item_time_else_date() {
        let mut h = Header { date: Some(7), ..Header::default() };
        assert_eq!(h.start_ms(), Some(7));
        h.item = Some(ItemDetails::Task(Task { due: Some(When::Date("1970-01-03".into())), ..Task::default() }));
        assert_eq!(h.start_ms(), Some(2 * 86_400_000));
        h.item = Some(ItemDetails::Task(Task { due: Some(When::Date("1970-02-30".into())), ..Task::default() }));
        assert_eq!(h.start_ms(), Some(7), "invalid day falls back");
    }

    #[test]
    fn classification_uses_extension_then_mime_essence() {
        use AttachmentCategory as C;
        let file = AttachmentKind::File;
        assert_eq!(C::classify("pdf", Some("application/octet-stream"), file), C::Pdf);
        assert_eq!(C::classify("", Some("Text/Calendar; method=REQUEST"), file), C::Calendar);
        assert_eq!(C::classify("", Some("application/pdf; name=\"a.pdf\""), file), C::Pdf);
        assert_eq!(C::classify("", Some("image/png"), file), C::Image);
        assert_eq!(C::classify("", None, AttachmentKind::EmbeddedMessage), C::Email);
        assert_eq!(C::classify("dat", None, AttachmentKind::Tnef), C::Email);
        assert_eq!(C::classify("bin", None, file), C::Other);

        assert_eq!(attachment_extension(Some("Report.PDF"), None, file), "pdf");
        assert_eq!(attachment_extension(Some("noext"), Some("image/jpeg; name=x"), file), "jpg");
        assert_eq!(attachment_extension(Some("weird.ext with space"), Some("text/plain"), file), "txt");
        assert_eq!(attachment_extension(None, None, AttachmentKind::EmbeddedMessage), "msg");
        assert_eq!(attachment_extension(None, Some("application/x-unknown"), file), "");
    }

    #[test]
    fn attachment_rows_name_unnamed_parts() {
        let header = Header { locator: Some(Locator::Pst { nid: 7 }), subject: "Hi".into(), ..Default::default() };
        let row = |m: AttachmentMeta, embedded: Option<&str>| AttachmentRow::new(SourceId(1), &header, &m, embedded);
        let r = row(meta(0, Some("  a.txt "), None, AttachmentKind::File), None).unwrap();
        assert_eq!((r.filename.as_str(), r.extension.as_str(), r.category), ("a.txt", "txt", AttachmentCategory::Text));
        assert_eq!(
            row(meta(2, None, Some("image/png"), AttachmentKind::File), None).unwrap().filename,
            "attachment-3.png"
        );
        assert_eq!(row(meta(0, None, None, AttachmentKind::File), None).unwrap().filename, "attachment-1");
        let r = row(meta(1, None, None, AttachmentKind::EmbeddedMessage), Some("Minutes")).unwrap();
        assert_eq!((r.filename.as_str(), r.category), ("Minutes.msg", AttachmentCategory::Email));
        assert!(row(meta(0, Some("x.url"), None, AttachmentKind::Link), None).is_none());
        let no_locator = Header::default();
        assert!(AttachmentRow::new(SourceId(1), &no_locator, &meta(0, Some("a"), None, AttachmentKind::File), None)
            .is_none());
    }

    /// The JSON shapes are a contract with the frontend (`app/src/lib/types.ts`) and the
    /// persisted index: changing them is a breaking change.
    #[test]
    fn serde_shapes() {
        let loc = Locator::Part { part: 1, inner: Box::new(Locator::Mbox { file: "a".into(), offset: 2, len: 3 }) };
        assert_eq!(
            serde_json::to_string(&loc).unwrap(),
            r#"{"t":"part","part":1,"inner":{"t":"mbox","file":"a","offset":2,"len":3}}"#
        );
        assert_eq!(serde_json::to_string(&AttachmentKind::EmbeddedMessage).unwrap(), r#""embedded_message""#);
        assert_eq!(serde_json::to_string(&SourceKind::AppleMail).unwrap(), r#""apple_mail""#);
        // Every `AttachmentQuery` field is optional on input.
        let q: AttachmentQuery = serde_json::from_str(r#"{"text":"inv","categories":["pdf"]}"#).unwrap();
        assert_eq!(q.text, "inv");
        assert_eq!(q.categories, [AttachmentCategory::Pdf]);
        assert_eq!(q.sort, AttachmentSort { key: AttachmentSortKey::Date, descending: false });
        for c in AttachmentCategory::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
    }
}
