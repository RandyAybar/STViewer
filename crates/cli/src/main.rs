//! `stv`: command-line front end for STViewer's mail library.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use mail_model::time::{civil_from_days, MS_PER_DAY};
use mail_model::{
    AttachmentKind, AttachmentQuery, AttachmentSort, AttachmentSortKey, Error, Folder, FolderId, Header, ItemDetails,
    Locator, Result, SearchOptions, Sort, SortKey, SourceId, When,
};
use serde::Serialize;
use stv_library::text::{body_text, format_address, format_addresses};
use stv_library::{dates, ExportFormat, Library, SourceInfo};

#[derive(Parser)]
#[command(
    name = "stv",
    version,
    about = "STViewer: browse, search and export local email archives",
    after_help = "Exit status: 0 on success; 1 on error, including `add` when any path could not be added \
                  and `--wait` when a source failed to index (unreadable single messages do not count); \
                  2 on invalid usage."
)]
struct Cli {
    /// Library directory (registry + search index). Default: the per-user STViewer data dir.
    #[arg(long, global = true, env = "STVIEWER_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum SortArg {
    Date,
    From,
    Subject,
    Size,
    Native,
    /// Appointment start / task due date (else the date).
    Start,
}

impl From<SortArg> for SortKey {
    fn from(s: SortArg) -> SortKey {
        match s {
            SortArg::Date => SortKey::Date,
            SortArg::Start => SortKey::Start,
            SortArg::From => SortKey::From,
            SortArg::Subject => SortKey::Subject,
            SortArg::Size => SortKey::Size,
            SortArg::Native => SortKey::Native,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum AttSortArg {
    Date,
    Size,
    Name,
    Type,
    From,
}

impl From<AttSortArg> for AttachmentSortKey {
    fn from(s: AttSortArg) -> AttachmentSortKey {
        match s {
            AttSortArg::Date => AttachmentSortKey::Date,
            AttSortArg::Size => AttachmentSortKey::Size,
            AttSortArg::Name => AttachmentSortKey::Name,
            AttSortArg::Type => AttachmentSortKey::Type,
            AttSortArg::From => AttachmentSortKey::From,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum FormatArg {
    Eml,
    Mbox,
    Html,
    Text,
    Csv,
    Attachments,
    /// Contacts as vCards (`export.vcf`).
    Vcf,
    /// Appointments and tasks as iCalendar (`export.ics`).
    Ics,
}

impl From<FormatArg> for ExportFormat {
    fn from(f: FormatArg) -> ExportFormat {
        match f {
            FormatArg::Eml => ExportFormat::Eml,
            FormatArg::Mbox => ExportFormat::Mbox,
            FormatArg::Html => ExportFormat::Html,
            FormatArg::Text => ExportFormat::Text,
            FormatArg::Csv => ExportFormat::Csv,
            FormatArg::Attachments => ExportFormat::Attachments,
            FormatArg::Vcf => ExportFormat::Vcf,
            FormatArg::Ics => ExportFormat::Ics,
        }
    }
}

/// How to pick a message: a locator (JSON as printed by `ls`/`search --json`) or `#N`,
/// the N-th row (1-based) of `stv ls` with the same folder/sort options.
#[derive(clap::Args)]
struct MessageRef {
    /// Source id, display name or path.
    source: String,
    /// Locator JSON (e.g. '{"t":"pst","nid":2097188}') or #ROW from `stv ls`.
    message: String,
    /// Folder for #ROW (default: same as `stv ls`).
    #[arg(long)]
    folder: Option<String>,
    /// Sort order for #ROW.
    #[arg(long, value_enum, default_value = "date")]
    sort: SortArg,
    /// Descending order for #ROW.
    #[arg(long)]
    desc: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Register mail archives (files or directories) and start indexing them.
    Add {
        /// Files or directories (directories are scanned for every archive they contain).
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Wait until the new sources are fully indexed.
        #[arg(long)]
        wait: bool,
    },
    /// Unregister a source and drop its index.
    Remove {
        /// Source id, display name or path.
        source: String,
    },
    /// List registered sources.
    Sources,
    /// Show a source's folder tree.
    Folders {
        /// Source id, display name or path.
        source: String,
    },
    /// List messages in a folder.
    Ls {
        /// Source id, display name or path.
        source: String,
        /// Folder id, path (`Inbox/Projects`) or name. Default: Inbox, else the first non-empty folder.
        folder: Option<String>,
        #[arg(long, value_enum, default_value = "date")]
        sort: SortArg,
        /// Descending order.
        #[arg(long)]
        desc: bool,
        /// Rows to skip.
        #[arg(long, default_value_t = 0)]
        offset: u64,
        /// Rows per page.
        #[arg(long, default_value_t = 50)]
        limit: u64,
    },
    /// Show one message: headers, body as text, attachments.
    Show {
        #[command(flatten)]
        msg: MessageRef,
        /// Also print transport headers.
        #[arg(long)]
        headers: bool,
        /// Also print raw properties (MAPI tags / MIME headers).
        #[arg(long)]
        props: bool,
    },
    /// Full-text search across all sources (see README for the query syntax).
    Search {
        /// Query words. Exclude with `-word`: quote the whole query ("budget -draft") or put
        /// it after `--`.
        #[arg(required = true)]
        query: Vec<String>,
        /// Hits per page.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Hits to skip.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Restrict to a source (repeatable).
        #[arg(long)]
        source: Vec<String>,
    },
    /// List attachments (recorded while indexing), with filters and sorting.
    Attachments {
        /// Words matched against file name, sender, subject. Exclude with `-word`: quote the
        /// words ("invoice -draft") or put them after `--`.
        text: Vec<String>,
        /// Restrict to a source (repeatable).
        #[arg(long)]
        source: Vec<String>,
        #[arg(long, value_enum, default_value = "date")]
        sort: AttSortArg,
        /// Ascending order (default: descending).
        #[arg(long)]
        asc: bool,
        /// Hide these extensions (repeatable, e.g. --hide-ext png).
        #[arg(long = "hide-ext")]
        hide_ext: Vec<String>,
        /// Minimum size in bytes.
        #[arg(long)]
        min_size: Option<u64>,
        /// Include inline attachments (images embedded in the body).
        #[arg(long)]
        inline: bool,
        /// Rows per page.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Rows to skip.
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Show indexing progress; with --wait, block until every source is indexed.
    Index {
        /// Block until every source is indexed.
        #[arg(long)]
        wait: bool,
        /// Rebuild this source's index from scratch (implies --wait).
        #[arg(long)]
        reindex: Option<String>,
    },
    /// Show the conversation a message belongs to (across all sources).
    Thread {
        #[command(flatten)]
        msg: MessageRef,
    },
    /// Export messages matching a query, or a whole source/folder.
    Export {
        /// Search query selecting the messages (optional with --source).
        query: Vec<String>,
        /// Source id, display name or path: restricts the query, or exports the whole source.
        #[arg(long)]
        source: Option<String>,
        /// Folder of --source (id, path or name).
        #[arg(long, requires = "source")]
        folder: Option<String>,
        #[arg(long, value_enum)]
        format: FormatArg,
        /// Destination directory (created if missing); existing files are never overwritten.
        #[arg(long)]
        out: PathBuf,
    },
    /// Measure open / folder / list / header-iteration speed of one archive (no library needed).
    Bench {
        /// Archive file or directory.
        path: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e @ Error::InUse { .. }) => {
            eprintln!("error: {e}. Close it, or use --data-dir (or STVIEWER_DATA_DIR) to choose a different library.");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    if let Cmd::Bench { path } = &cli.cmd {
        return bench(path, json);
    }
    let data_dir = cli.data_dir.unwrap_or_else(stv_library::default_data_dir);
    let lib = Library::open(&data_dir)?;
    match cli.cmd {
        Cmd::Add { paths, wait } => {
            let outcome = lib.add_paths(&paths);
            for (p, e) in &outcome.errors {
                eprintln!("{}: {e}", p.display());
            }
            let mut added = outcome.added;
            if added.is_empty() {
                return Err(Error::Unsupported("no mail archives were added".into()));
            }
            if wait {
                let ids: Vec<SourceId> = added.iter().map(|s| s.id).collect();
                wait_for_index(&lib, &ids, json)?;
                let fresh = lib.sources();
                added = added.into_iter().filter_map(|a| fresh.iter().find(|s| s.id == a.id).cloned()).collect();
            }
            if json {
                print_json(&added)?;
            } else {
                print_sources(&added);
                if !wait {
                    println!("\nIndexing runs in the background while stv runs; use `stv index --wait` to finish it.");
                }
            }
            if !outcome.errors.is_empty() {
                return Err(Error::Other(format!(
                    "{} of {} path(s) could not be added; the others were registered",
                    outcome.errors.len(),
                    paths.len()
                )));
            }
            if wait {
                index_failures(&added)?;
            }
        }
        Cmd::Remove { source } => {
            let id = resolve_source(&lib, &source)?;
            lib.remove_source(id)?;
            if json {
                return print_json(&serde_json::json!({ "removed": id }));
            }
            println!("removed source {}", id.0);
        }
        Cmd::Sources => {
            let s = lib.sources();
            if json {
                return print_json(&s);
            }
            if s.is_empty() {
                println!("no sources; add one with `stv add <path>`");
            }
            print_sources(&s);
        }
        Cmd::Folders { source } => {
            let id = resolve_source(&lib, &source)?;
            let folders = lib.folders(id)?;
            if json {
                return print_json(&folders);
            }
            let paths = folder_paths(&folders);
            let rows = folders
                .iter()
                .zip(&paths)
                .map(|(f, p)| {
                    let depth = p.matches('/').count();
                    vec![
                        format!("{}{}", "  ".repeat(depth), f.name),
                        f.message_count.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        f.unread_count.map(|n| n.to_string()).unwrap_or_default(),
                        format!("{:?}", f.kind).to_lowercase(),
                        f.id.0.clone(),
                    ]
                })
                .collect();
            print_table(&["folder", "messages", "unread", "kind", "id"], rows, &[50, 9, 7, 9, 40]);
        }
        Cmd::Ls { source, folder, sort, desc, offset, limit } => {
            let id = resolve_source(&lib, &source)?;
            let folder = resolve_folder(&lib, id, folder.as_deref())?;
            let sort = Sort { key: sort.into(), descending: desc };
            let total = lib.count(id, &folder)?;
            let start = offset.min(total);
            let rows = lib.list(id, &folder, start..start.saturating_add(limit).min(total), sort)?;
            if json {
                return print_json(
                    &serde_json::json!({ "folder": folder, "total": total, "offset": offset, "rows": rows }),
                );
            }
            println!("{} messages in {}", total, folder.0);
            // Sorted by start, the time column shows the start (due date for tasks).
            let by_start = sort.key == SortKey::Start;
            let table = rows
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let mut r = vec![format!("#{}", offset + i as u64 + 1)];
                    r.extend(header_cells(h, by_start));
                    r
                })
                .collect();
            let time = if by_start { "start" } else { "date" };
            print_table(&["row", time, "from", "subject", "size", "flags"], table, &[7, 16, 28, 60, 8, 5]);
        }
        Cmd::Show { msg, headers, props } => {
            let (id, loc) = resolve_message(&lib, &msg)?;
            let m = lib.message(id, &loc)?;
            let properties = if props { lib.properties(id, &loc)? } else { Vec::new() };
            if json {
                // `--props` adds the inspector rows as a `properties` field next to the message's.
                let mut v = serde_json::to_value(&m).map_err(|e| Error::Other(e.to_string()))?;
                if props {
                    v["properties"] = serde_json::to_value(&properties).map_err(|e| Error::Other(e.to_string()))?;
                }
                return print_json(&v);
            }
            let h = &m.header;
            let field = |k: &str, v: String| {
                if !v.is_empty() {
                    println!("{k:>9}: {v}");
                }
            };
            field("From", h.from.as_ref().map(format_address).unwrap_or_default());
            field("To", format_addresses(&h.to));
            field("Cc", format_addresses(&h.cc));
            field("Bcc", format_addresses(&h.bcc));
            field("Date", h.date.map(dates::format_rfc2822).unwrap_or_default());
            field("Subject", h.subject.clone());
            field("Folder", h.folder.as_ref().map(|f| f.0.clone()).unwrap_or_default());
            field("Locator", serde_json::to_string(&loc).unwrap_or_default());
            for (k, v) in h.item.as_ref().map(item_fields).unwrap_or_default() {
                field(k, v);
            }
            if headers {
                if let Some(raw) = &m.raw_headers {
                    println!("\n{}", raw.trim_end());
                }
            }
            println!("\n{}\n", body_text(&m.body).trim_end());
            if !m.attachments.is_empty() {
                let rows = m
                    .attachments
                    .iter()
                    .map(|a| {
                        vec![
                            a.index.to_string(),
                            a.filename.clone().unwrap_or_default(),
                            a.mime_type.clone().unwrap_or_default(),
                            a.size.to_string(),
                            match a.kind {
                                AttachmentKind::File if a.inline => "inline".into(),
                                k => format!("{k:?}").to_lowercase(),
                            },
                        ]
                    })
                    .collect();
                print_table(&["idx", "attachment", "type", "bytes", "kind"], rows, &[4, 50, 30, 10, 16]);
            }
            if props {
                let rows = properties
                    .iter()
                    .map(|p| {
                        vec![p.tag.clone(), p.name.clone(), p.type_name.clone(), p.value.replace(['\r', '\n'], " ")]
                    })
                    .collect();
                println!();
                print_table(&["tag", "name", "type", "value"], rows, &[12, 40, 12, 80]);
            }
        }
        Cmd::Search { query, limit, offset, source } => {
            let sources = source.iter().map(|s| resolve_source(&lib, s)).collect::<Result<Vec<_>>>()?;
            let opts = SearchOptions { offset, limit, sources, ..Default::default() };
            let res = lib.search(&query.join(" "), &opts)?;
            if json {
                return print_json(&res);
            }
            println!("{} hits ({} ms)", res.total, res.took_ms);
            for (i, hit) in res.hits.iter().enumerate() {
                let h = &hit.header;
                println!(
                    "{:>4}. [{}] {}  {}  {}",
                    offset + i + 1,
                    hit.source.0,
                    h.date.map(dates::format_short).unwrap_or_else(|| "-".repeat(16)),
                    clip(&h.from.as_ref().map(format_address).unwrap_or_default(), 30),
                    clip(&h.subject, 70)
                );
                let snippet = strip_tags(&hit.snippet);
                if !snippet.trim().is_empty() {
                    println!("      {}", clip(snippet.trim(), 110));
                }
                if let Some(item) = &h.item {
                    let parts: Vec<String> = item_fields(item)
                        .into_iter()
                        .filter(|(_, v)| !v.is_empty())
                        .take(4)
                        .map(|(k, v)| format!("{}: {v}", k.to_lowercase()))
                        .collect();
                    if !parts.is_empty() {
                        println!("      {}", clip(&parts.join("  "), 110));
                    }
                }
                if let Some(l) = &h.locator {
                    println!("      loc: {}", serde_json::to_string(l).unwrap_or_default());
                }
            }
        }
        Cmd::Attachments { text, source, sort, asc, hide_ext, min_size, inline, limit, offset } => {
            let sources = source.iter().map(|s| resolve_source(&lib, s)).collect::<Result<Vec<_>>>()?;
            let q = AttachmentQuery {
                sources,
                text: text.join(" "),
                exclude_extensions: hide_ext,
                min_size,
                include_inline: inline,
                sort: AttachmentSort { key: sort.into(), descending: !asc },
                offset,
                limit,
                ..Default::default()
            };
            let page = lib.attachments(&q)?;
            if json {
                return print_json(&page);
            }
            let counts: Vec<String> = page.category_counts.iter().map(|(c, n)| format!("{} {n}", c.as_str())).collect();
            println!("{} attachments ({} ms)  [{}]", page.total, page.took_ms, counts.join(", "));
            for (i, r) in page.rows.iter().enumerate() {
                println!(
                    "{:>5}. {:>8}  {}  {:<12} {}  — {}",
                    offset + i + 1,
                    human_size(r.size),
                    r.date.map(dates::format_short).unwrap_or_else(|| "-".repeat(16)),
                    r.category.as_str(),
                    clip(&r.filename, 50),
                    clip(&r.subject, 40)
                );
            }
        }
        Cmd::Index { wait, reindex } => {
            let waited: Option<Vec<SourceId>> = if let Some(s) = reindex {
                let id = resolve_source(&lib, &s)?;
                lib.reindex(id)?;
                Some(vec![id])
            } else if wait {
                Some(lib.sources().iter().map(|s| s.id).collect())
            } else {
                None
            };
            if let Some(ids) = &waited {
                wait_for_index(&lib, ids, json)?;
            }
            let s = lib.sources();
            if json {
                print_json(&s.iter().map(|s| &s.index).collect::<Vec<_>>())?;
            } else {
                print_sources(&s);
            }
            if let Some(ids) = waited {
                let watched: Vec<SourceInfo> = s.into_iter().filter(|s| ids.contains(&s.id)).collect();
                index_failures(&watched)?;
            }
        }
        Cmd::Thread { msg } => {
            let (id, loc) = resolve_message(&lib, &msg)?;
            let items = lib.conversation(id, &loc, None)?;
            if json {
                return print_json(&items);
            }
            println!("{} emails in conversation", items.len());
            for c in &items {
                let h = &c.header;
                println!(
                    "{}[{}] {}  {}  — {}",
                    if c.current { "*" } else { " " },
                    c.source.0,
                    h.date.map(dates::format_short).unwrap_or_else(|| "-".repeat(16)),
                    clip(&h.from.as_ref().map(format_address).unwrap_or_default(), 30),
                    clip(&h.subject, 60)
                );
                if !c.preview.is_empty() {
                    println!("      {}", clip(&c.preview, 110));
                }
                if !c.also_in.is_empty() {
                    println!("      also in: {}", c.also_in.join(", "));
                }
            }
        }
        Cmd::Export { query, source, folder, format, out } => {
            let items = export_selection(&lib, &query.join(" "), source.as_deref(), folder.as_deref())?;
            let report = lib.export(&items, format.into(), &out)?;
            if json {
                return print_json(&report);
            }
            println!("exported {} item(s) from {} message(s) to {}", report.written, items.len(), out.display());
            if report.skipped > 0 {
                println!("  skipped {} item(s) this format does not apply to", report.skipped);
            }
            if report.duplicates > 0 {
                println!(
                    "  skipped {} repeat(s) of an already exported UID (meeting replies, copies)",
                    report.duplicates
                );
            }
            for (l, e) in &report.failed {
                println!("  failed {}: {e}", serde_json::to_string(l).unwrap_or_default());
            }
        }
        Cmd::Bench { .. } => unreachable!("handled before opening the library"),
    }
    Ok(())
}

fn print_json<T: Serialize + ?Sized>(v: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(v).map_err(|e| Error::Other(e.to_string()))?;
    println!("{s}");
    Ok(())
}

fn print_sources(s: &[SourceInfo]) {
    let rows = s
        .iter()
        .map(|s| {
            let mut status = match (&s.index.error, s.index.done, s.index.total) {
                (Some(e), _, _) => format!("error: {e}"),
                (None, true, _) => "indexed".into(),
                (None, false, None) => "pending".into(),
                (None, false, Some(_)) => "indexing".into(),
            };
            if s.index.read_errors > 0 {
                status.push_str(&format!(" ({} unreadable)", s.index.read_errors));
            }
            let progress = match s.index.total {
                Some(t) => format!("{}/{}", s.index.indexed, t),
                None => s.index.indexed.to_string(),
            };
            vec![
                s.id.0.to_string(),
                format!("{:?}", s.kind).to_lowercase(),
                s.display_name.clone(),
                progress,
                status,
                s.path.display().to_string(),
            ]
        })
        .collect();
    print_table(&["id", "kind", "name", "docs", "index", "path"], rows, &[4, 11, 30, 13, 30, 70]);
}

/// Fails if any of `sources` stopped indexing with a fatal error.
fn index_failures(sources: &[SourceInfo]) -> Result<()> {
    let failed = sources.iter().filter(|s| s.index.error.is_some()).count();
    if failed > 0 {
        return Err(Error::Other(format!("indexing failed for {failed} source(s)")));
    }
    Ok(())
}

/// `stv ls` cells; `by_start` shows [`Header::start_ms`] in the time column.
fn header_cells(h: &Header, by_start: bool) -> Vec<String> {
    let mut flags = String::new();
    if !h.flags.read {
        flags.push('U');
    }
    if h.flags.flagged {
        flags.push('F');
    }
    if h.has_attachments {
        flags.push('A');
    }
    let time = if by_start { h.start_ms() } else { h.date };
    vec![
        time.map(dates::format_short).unwrap_or_default(),
        h.from.as_ref().map(format_address).unwrap_or_default(),
        h.subject.clone(),
        human_size(h.size),
        flags,
    ]
}

/// `stv show` lines for calendar, contact and task details (empty values are not printed).
fn item_fields(item: &ItemDetails) -> Vec<(&'static str, String)> {
    fn when(w: Option<&When>) -> String {
        match w {
            Some(When::Time(ms)) => dates::format_rfc2822(*ms),
            Some(When::Date(d)) => d.clone(),
            None => String::new(),
        }
    }
    /// A serde enum's name as words (`out_of_office` → `out of office`).
    fn word(v: impl Serialize) -> String {
        serde_json::to_value(v).ok().and_then(|v| v.as_str().map(|s| s.replace('_', " "))).unwrap_or_default()
    }
    let text = |s: &Option<String>| s.as_deref().unwrap_or("").replace(['\r', '\n'], " ");
    let mut out = Vec::new();
    match item {
        ItemDetails::Appointment(a) => {
            out.push(("Meeting", a.meeting.map(word).unwrap_or_default()));
            out.push(("Start", when(a.start.as_ref())));
            // All-day ends are stored exclusive: show the last day, and none for one day.
            let end = match (&a.start, &a.end) {
                (Some(s @ When::Date(_)), Some(e @ When::Date(_))) => match (s.to_ms(), e.to_ms()) {
                    (Some(s), Some(e)) if e - s > MS_PER_DAY => {
                        let (y, m, d) = civil_from_days((e - MS_PER_DAY).div_euclid(MS_PER_DAY));
                        format!("{y:04}-{m:02}-{d:02}")
                    }
                    (Some(_), Some(_)) => String::new(),
                    _ => when(Some(e)),
                },
                (_, e) => when(e.as_ref()),
            };
            out.push(("End", end));
            out.push(("All day", if a.all_day { "yes".into() } else { String::new() }));
            out.push(("Location", text(&a.location)));
            out.push(("Organizer", a.organizer.as_ref().map(format_address).unwrap_or_default()));
            out.push(("Required", format_addresses(&a.required)));
            out.push(("Optional", format_addresses(&a.optional)));
            out.push(("Resources", format_addresses(&a.resources)));
            out.push(("Show as", a.busy_status.map(word).unwrap_or_default()));
            let recurs = a.recurrence.as_deref().map(|r| r.replace(['\r', '\n'], " "));
            out.push(("Recurs", recurs.unwrap_or_else(|| if a.recurring { "yes".into() } else { String::new() })));
        }
        ItemDetails::Contact(c) => {
            out.push(("Name", text(&c.display_name)));
            out.push(("Company", text(&c.company)));
            out.push(("Title", text(&c.job_title)));
            out.push(("Dept", text(&c.department)));
            out.extend(c.emails.iter().map(|e| ("Email", format_address(e))));
            out.extend(c.phones.iter().map(|p| ("Phone", format!("{} ({})", p.value.trim(), p.label))));
            out.extend(c.addresses.iter().map(|a| {
                let lines: Vec<&str> = a.value.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                ("Address", format!("{} ({})", lines.join(", "), a.label))
            }));
            out.push(("Birthday", when(c.birthday.as_ref())));
            out.push(("Web", text(&c.web_page)));
        }
        ItemDetails::Task(t) => {
            out.push(("Status", t.status.map(word).unwrap_or_default()));
            out.push(("Done", t.percent_complete.map(|p| format!("{p}%")).unwrap_or_default()));
            out.push(("Start", when(t.start.as_ref())));
            out.push(("Due", when(t.due.as_ref())));
            out.push(("Completed", when(t.completed.as_ref())));
            out.push(("Owner", text(&t.owner)));
        }
    }
    out
}

fn human_size(n: u64) -> String {
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.0} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// Truncate to `max` chars with an ellipsis; control chars become spaces.
fn clip(s: &str, max: usize) -> String {
    let clean: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if clean.chars().count() <= max {
        return clean;
    }
    let mut out: String = clean.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    html_escape::decode_html_entities(&out).into_owned()
}

fn print_table(headers: &[&str], rows: Vec<Vec<String>>, max: &[usize]) {
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|r| r.iter().enumerate().map(|(i, c)| clip(c, max.get(i).copied().unwrap_or(40))).collect())
        .collect();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: &[String]| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s.push_str(c);
            } else {
                s.push_str(c);
                s.push_str(&" ".repeat(widths[i] - c.chars().count() + 2));
            }
        }
        println!("{}", s.trim_end());
    };
    line(&headers.iter().map(|h| h.to_uppercase()).collect::<Vec<_>>());
    for r in &rows {
        line(r);
    }
}

fn resolve_source(lib: &Library, s: &str) -> Result<SourceId> {
    let sources = lib.sources();
    if let Ok(n) = s.parse::<u32>() {
        if let Some(src) = sources.iter().find(|x| x.id.0 == n) {
            return Ok(src.id);
        }
    }
    let lower = s.to_lowercase();
    let exact: Vec<&SourceInfo> = sources
        .iter()
        .filter(|x| x.display_name.to_lowercase() == lower || x.path.to_string_lossy().to_lowercase() == lower)
        .collect();
    if let [one] = exact.as_slice() {
        return Ok(one.id);
    }
    let partial: Vec<&SourceInfo> = sources
        .iter()
        .filter(|x| {
            x.display_name.to_lowercase().contains(&lower) || x.path.to_string_lossy().to_lowercase().contains(&lower)
        })
        .collect();
    match partial.as_slice() {
        [one] => Ok(one.id),
        [] => Err(Error::NotFound(format!("no source matches {s:?} (see `stv sources`)"))),
        many => Err(Error::Other(format!(
            "{s:?} matches several sources: {}",
            many.iter().map(|x| x.id.0.to_string()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Full `/`-joined name path of each folder (parents precede children).
fn folder_paths(folders: &[Folder]) -> Vec<String> {
    let mut by_id: std::collections::HashMap<&FolderId, String> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(folders.len());
    for f in folders {
        let p = match f.parent.as_ref().and_then(|p| by_id.get(p)) {
            Some(parent) => format!("{parent}/{}", f.name),
            None => f.name.clone(),
        };
        by_id.insert(&f.id, p.clone());
        out.push(p);
    }
    out
}

fn resolve_folder(lib: &Library, id: SourceId, wanted: Option<&str>) -> Result<FolderId> {
    let folders = lib.folders(id)?;
    let Some(w) = wanted else {
        let inbox = folders.iter().find(|f| f.name.eq_ignore_ascii_case("inbox"));
        let first_full = folders.iter().find(|f| f.message_count.unwrap_or(0) > 0);
        return inbox
            .or(first_full)
            .or(folders.first())
            .map(|f| f.id.clone())
            .ok_or_else(|| Error::NotFound("source has no folders".into()));
    };
    if let Some(f) = folders.iter().find(|f| f.id.0 == w) {
        return Ok(f.id.clone());
    }
    let paths = folder_paths(&folders);
    let wl = w.trim_matches('/').to_lowercase();
    if let Some(i) = paths.iter().position(|p| p.to_lowercase() == wl || p.to_lowercase().ends_with(&format!("/{wl}")))
    {
        return Ok(folders[i].id.clone());
    }
    let named: Vec<&Folder> = folders.iter().filter(|f| f.name.to_lowercase() == wl).collect();
    match named.as_slice() {
        [one] => Ok(one.id.clone()),
        _ => Err(Error::NotFound(format!("no folder {w:?} (see `stv folders`)"))),
    }
}

fn resolve_message(lib: &Library, m: &MessageRef) -> Result<(SourceId, Locator)> {
    let id = resolve_source(lib, &m.source)?;
    let s = m.message.trim();
    if let Some(row) = s.strip_prefix('#') {
        let n: u64 = row.parse().map_err(|_| Error::Other(format!("bad row {s:?}; use #1, #2, ...")))?;
        if n == 0 {
            return Err(Error::Other("rows are numbered from #1".into()));
        }
        let folder = resolve_folder(lib, id, m.folder.as_deref())?;
        let sort = Sort { key: m.sort.into(), descending: m.desc };
        let h = lib
            .list(id, &folder, n - 1..n, sort)?
            .into_iter()
            .next()
            .ok_or_else(|| Error::NotFound(format!("row #{n} in {}", folder.0)))?;
        return h.locator.map(|l| (id, l)).ok_or_else(|| Error::Other("listed message has no locator".into()));
    }
    let loc: Locator =
        serde_json::from_str(s).map_err(|e| Error::Other(format!("message must be #ROW or locator JSON ({e})")))?;
    Ok((id, loc))
}

fn wait_for_index(lib: &Library, ids: &[SourceId], json: bool) -> Result<()> {
    let start = Instant::now();
    let mut last_line = String::new();
    loop {
        let sources = lib.sources();
        let watched: Vec<&SourceInfo> = sources.iter().filter(|s| ids.contains(&s.id)).collect();
        let done = watched.iter().all(|s| s.index.done) && !lib.is_indexing();
        if !json {
            let indexed: u64 = watched.iter().map(|s| s.index.indexed).sum();
            let total: Option<u64> = watched.iter().map(|s| s.index.total).sum();
            let line = match total {
                Some(t) => format!("indexed {indexed}/{t} messages"),
                None => format!("indexed {indexed} messages"),
            };
            if line != last_line {
                eprintln!("[{:>6.1}s] {line}", start.elapsed().as_secs_f64());
                last_line = line;
            }
        }
        if done {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn export_selection(
    lib: &Library,
    query: &str,
    source: Option<&str>,
    folder: Option<&str>,
) -> Result<Vec<(SourceId, Locator)>> {
    let source = source.map(|s| resolve_source(lib, s)).transpose()?;
    if query.trim().is_empty() {
        let Some(id) = source else {
            return Err(Error::Other("give a query, or --source (and optionally --folder)".into()));
        };
        let folders: Vec<FolderId> = match folder {
            Some(f) => vec![resolve_folder(lib, id, Some(f))?],
            None => lib.folders(id)?.into_iter().map(|f| f.id).collect(),
        };
        let mut items = Vec::new();
        const PAGE: u64 = 1000;
        for f in folders {
            let total = lib.count(id, &f)?;
            let mut off = 0;
            while off < total {
                let page =
                    lib.list(id, &f, off..(off + PAGE).min(total), Sort { key: SortKey::Native, descending: false })?;
                if page.is_empty() {
                    break;
                }
                off += page.len() as u64;
                items.extend(page.into_iter().filter_map(|h| h.locator.map(|l| (id, l))));
            }
        }
        return Ok(items);
    }
    let folder = match (source, folder) {
        (Some(id), Some(f)) => Some(resolve_folder(lib, id, Some(f))?),
        _ => None,
    };
    let mut opts =
        SearchOptions { offset: 0, limit: 1000, sources: source.into_iter().collect(), folder, ..Default::default() };
    let mut items = Vec::new();
    loop {
        let res = lib.search(query, &opts)?;
        let n = res.hits.len();
        items.extend(res.hits.into_iter().filter_map(|h| h.header.locator.map(|l| (h.source, l))));
        opts.offset += n;
        if n == 0 || opts.offset as u64 >= res.total {
            break;
        }
    }
    Ok(items)
}

fn bench(path: &Path, json: bool) -> Result<()> {
    #[derive(Serialize)]
    struct Report {
        path: PathBuf,
        kind: String,
        open_ms: f64,
        folders: usize,
        folders_ms: f64,
        largest_folder: String,
        largest_count: u64,
        count_ms: f64,
        first_page_ms: f64,
        last_page_ms: f64,
        headers: u64,
        header_errors: u64,
        iter_ms: f64,
        headers_per_sec: f64,
    }
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let src = stv_library::open_source(path)?;
    let open_ms = ms(t);

    let t = Instant::now();
    let folders = src.folders()?;
    let folders_ms = ms(t);

    let t = Instant::now();
    let mut largest: Option<(FolderId, u64)> = None;
    for f in &folders {
        let n = src.count(&f.id)?;
        if largest.as_ref().is_none_or(|(_, m)| n > *m) {
            largest = Some((f.id.clone(), n));
        }
    }
    let count_ms = ms(t);
    let (largest_folder, largest_count) = largest.unwrap_or((FolderId(String::new()), 0));

    let sort = Sort { key: SortKey::Date, descending: true };
    let t = Instant::now();
    if largest_count > 0 {
        src.list(&largest_folder, 0..largest_count.min(100), sort)?;
    }
    let first_page_ms = ms(t);
    let t = Instant::now();
    if largest_count > 0 {
        src.list(&largest_folder, largest_count.saturating_sub(100)..largest_count, sort)?;
    }
    let last_page_ms = ms(t);

    let t = Instant::now();
    let (mut headers, mut header_errors) = (0u64, 0u64);
    for h in src.iter_headers() {
        match h {
            Ok(_) => headers += 1,
            Err(_) => header_errors += 1,
        }
    }
    let iter_ms = ms(t);

    let r = Report {
        path: path.to_path_buf(),
        kind: format!("{:?}", src.kind()).to_lowercase(),
        open_ms,
        folders: folders.len(),
        folders_ms,
        largest_folder: largest_folder.0,
        largest_count,
        count_ms,
        first_page_ms,
        last_page_ms,
        headers,
        header_errors,
        iter_ms,
        headers_per_sec: if iter_ms > 0.0 { headers as f64 / (iter_ms / 1000.0) } else { 0.0 },
    };
    if json {
        return print_json(&r);
    }
    println!("source          {} ({})", r.path.display(), r.kind);
    println!("open            {:>10.2} ms", r.open_ms);
    println!("folders         {:>10.2} ms  ({} folders)", r.folders_ms, r.folders);
    println!(
        "count all       {:>10.2} ms  (largest: {} with {} messages)",
        r.count_ms, r.largest_folder, r.largest_count
    );
    println!("first 100 rows  {:>10.2} ms  (sorted by date desc)", r.first_page_ms);
    println!("last 100 rows   {:>10.2} ms", r.last_page_ms);
    println!(
        "iter_headers    {:>10.2} ms  ({} headers, {} errors, {:.0} headers/s)",
        r.iter_ms, r.headers, r.header_errors, r.headers_per_sec
    );
    Ok(())
}
