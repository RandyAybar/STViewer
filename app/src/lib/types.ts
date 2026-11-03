// Mirrors of the serde shapes in crates/mail-model and crates/library.

export type SourceId = number;

export type SourceKind = "pst" | "ost" | "mbox" | "thunderbird" | "eml" | "apple_mail" | "msg" | "tnef";

/** `FolderId(String)` serializes as a bare string. */
export type FolderId = string;

export type Locator =
  | { t: "pst"; nid: number }
  | { t: "mbox"; file: string; offset: number; len: number }
  | { t: "file"; path: string }
  | { t: "embedded"; parent: Locator; attachment: number }
  | { t: "part"; part: number; inner: Locator };

export type FolderKind =
  "mail" | "calendar" | "contacts" | "tasks" | "notes" | "journal" | "label" | "deleted" | "other";

export interface Folder {
  id: FolderId;
  parent: FolderId | null;
  name: string;
  kind: FolderKind;
  message_count: number | null;
  unread_count: number | null;
}

export interface Address {
  name: string | null;
  email: string | null;
}

export interface MessageFlags {
  read: boolean;
  flagged: boolean;
  draft: boolean;
  deleted: boolean;
  encrypted: boolean;
  signed: boolean;
}

export interface Header {
  locator: Locator | null;
  folder: FolderId | null;
  subject: string;
  from: Address | null;
  to: Address[];
  cc: Address[];
  bcc: Address[];
  date: number | null;
  size: number;
  has_attachments: boolean;
  flags: MessageFlags;
  message_id: string | null;
  in_reply_to: string | null;
  references: string[];
  conversation: string | null;
  labels: string[];
  message_class: string | null;
  /** Calendar, contact or task details; absent on plain mail. List rows carry no attendees. */
  item?: ItemDetails;
}

/** A point in time (UTC ms), or a calendar day shown as that date in every zone. */
export type When = { kind: "time"; value: number } | { kind: "date"; value: string };

export type ItemDetails =
  ({ type: "appointment" } & Appointment) | ({ type: "contact" } & Contact) | ({ type: "task" } & Task);

export type BusyStatus = "free" | "tentative" | "busy" | "out_of_office" | "working_elsewhere";

/** `counter`: an attendee proposes a new time (the item's start/end are the proposal). */
export type MeetingKind = "request" | "cancellation" | "accepted" | "tentative" | "declined" | "counter";

export interface Appointment {
  /** `date` for all-day items, whose `end` is exclusive. */
  start: When | null;
  end: When | null;
  all_day: boolean;
  location: string | null;
  organizer: Address | null;
  required: Address[];
  optional: Address[];
  resources: Address[];
  busy_status: BusyStatus | null;
  recurring: boolean;
  /** Human-readable pattern as stored (Outlook text or an iCalendar `RRULE`). */
  recurrence: string | null;
  /** Set when the item is a meeting message rather than the calendar entry itself. */
  meeting: MeetingKind | null;
  uid: string | null;
}

/** A value with its lowercase label (`business`, `home`, `mobile`, `fax`, `other`, …). */
export interface Labeled {
  label: string;
  value: string;
}

export interface Contact {
  display_name: string | null;
  given_name: string | null;
  surname: string | null;
  company: string | null;
  job_title: string | null;
  department: string | null;
  emails: Address[];
  phones: Labeled[];
  /** Postal addresses, one multi-line string each. */
  addresses: Labeled[];
  birthday: When | null;
  web_page: string | null;
}

export type TaskStatus = "not_started" | "in_progress" | "complete" | "waiting" | "deferred";

export interface Task {
  status: TaskStatus | null;
  /** 0–100. */
  percent_complete: number | null;
  start: When | null;
  due: When | null;
  completed: When | null;
  owner: string | null;
  /** iCalendar `UID` or Outlook task global id (hex); omitted when unknown. */
  uid?: string | null;
}

export interface Body {
  text: string | null;
  html: string | null;
  rtf: string | null;
}

export type AttachmentKind = "file" | "embedded_message" | "tnef" | "link" | "ole";

export interface AttachmentMeta {
  index: number;
  filename: string | null;
  mime_type: string | null;
  size: number;
  content_id: string | null;
  inline: boolean;
  kind: AttachmentKind;
}

export interface Message {
  header: Header;
  body: Body;
  attachments: AttachmentMeta[];
  raw_headers: string | null;
}

/** `start`: appointment start / task due, else the date. */
export type SortKey = "date" | "from" | "subject" | "size" | "native" | "start";

export interface Sort {
  key: SortKey;
  descending: boolean;
}

export interface SearchOptions {
  offset: number;
  limit: number;
  sources: SourceId[];
  folder: FolderId | null;
  /** `null` = relevance */
  sort: Sort | null;
  facets: boolean;
}

export interface SearchHit {
  source: SourceId;
  header: Header;
  score: number;
  /** HTML-escaped snippet with matches wrapped in `<b>`. */
  snippet: string;
}

export interface FacetCount {
  value: string;
  count: number;
}

export interface SearchResults {
  total: number;
  hits: SearchHit[];
  took_ms: number;
  facets_from: FacetCount[];
  facets_year: FacetCount[];
  facets_source: FacetCount[];
}

export interface IndexProgress {
  source: SourceId | null;
  indexed: number;
  /** `null` until the total is known. */
  total: number | null;
  done: boolean;
  /** Messages that could not be read and were indexed from their header only. */
  read_errors: number;
  /** Fatal: the sync failed or the source is missing. */
  error: string | null;
}

export interface SourceInfo {
  id: SourceId;
  kind: SourceKind;
  path: string;
  display_name: string;
  index: IndexProgress;
}

export type ExportFormat = "eml" | "mbox" | "html" | "text" | "csv" | "attachments" | "vcf" | "ics";

export interface ExportReport {
  written: number;
  failed: [Locator, string][];
  /** `vcf`/`ics`: items the format does not apply to, left out (not failures). */
  skipped: number;
  /** `ics`: items left out because a component with their UID was already written. */
  duplicates: number;
}

export interface AddResult {
  added: SourceInfo[];
  errors: [string, string][];
}

export interface SaveAllReport {
  written: string[];
  failed: [string, string][];
}

/** One row of the message list: a folder listing row or a search hit. */
export interface Row {
  source: SourceId;
  header: Header;
  snippet?: string;
}

export type AttachmentCategory =
  | "image"
  | "pdf"
  | "document"
  | "spreadsheet"
  | "presentation"
  | "archive"
  | "email"
  | "calendar"
  | "audio"
  | "video"
  | "text"
  | "other";

export type AttachmentSortKey = "name" | "size" | "date" | "type" | "from";

export interface AttachmentSort {
  key: AttachmentSortKey;
  descending: boolean;
}

export interface AttachmentQuery {
  /** Empty = all sources. */
  sources: SourceId[];
  /** Words matched against file name, sender and subject; `-word` excludes. */
  text: string;
  /** Empty = all categories. */
  categories: AttachmentCategory[];
  exclude_categories: AttachmentCategory[];
  /** Lowercase, no dot. */
  exclude_extensions: string[];
  /** Bytes. */
  min_size: number | null;
  max_size: number | null;
  include_inline: boolean;
  sort: AttachmentSort;
  offset: number;
  limit: number;
}

export interface AttachmentRow {
  source: SourceId;
  /** The message carrying the attachment. */
  message: Locator;
  /** Attachment index within that message. */
  index: number;
  filename: string;
  mime_type: string | null;
  /** Lowercase, no dot; empty when none. */
  extension: string;
  category: AttachmentCategory;
  kind: AttachmentKind;
  size: number;
  inline: boolean;
  /** Date of the carrying message, UTC ms. */
  date: number | null;
  from: Address | null;
  subject: string;
  folder: FolderId | null;
}

export interface AttachmentPage {
  total: number;
  rows: AttachmentRow[];
  /** Per-category counts ignoring the query's `categories` filter. */
  category_counts: [AttachmentCategory, number][];
  took_ms: number;
}

/** What a tag is attached to: an email (`attachment` null) or one of its attachments. */
export interface TagTarget {
  source: SourceId;
  message: Locator;
  attachment: number | null;
}

export interface Tag {
  name: string;
  /** `#rrggbb` */
  color: string;
  /** Tagged items across all sources. */
  count: number;
}

export interface TaggedItem {
  target: TagTarget;
  tags: string[];
  /** When the first tag was added, ms. */
  added: number;
  /** The email (or the email carrying the attachment). */
  header: Header;
  attachment: AttachmentRow | null;
}

/** One email of a conversation (copies in several folders merged), oldest first. */
export interface ConversationItem {
  source: SourceId;
  header: Header;
  /** The email's own text, quoted history stripped, ≤ 280 chars. */
  preview: string;
  /** Display names of other folders holding a copy. */
  also_in: string[];
  /** This is the opened email. */
  current: boolean;
}

/** MCP server that lets a local coding agent read the shared mailboxes. */
export interface AgentAccess {
  enabled: boolean;
  running: boolean;
  port: number;
  token: string;
  /** `http://127.0.0.1:<port>/mcp` */
  url: string;
  allowed_sources: SourceId[];
  error: string | null;
}
