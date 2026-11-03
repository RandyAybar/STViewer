//! Read-only MCP (Model Context Protocol) server over an STViewer [`Library`], so coding
//! agents (Claude Code, Cursor, Codex, ...) can search and read mail the user shares.
//!
//! Served over MCP streamable HTTP on `127.0.0.1` only, behind a bearer token. Requests
//! carrying an `Origin` header (browsers) or a foreign `Host` are refused, which blocks
//! web pages and DNS-rebinding from reaching it. Only mailboxes in the live
//! [`Shared`] set are visible; every tool call is reported to the [`AccessLog`].

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use base64::Engine as _;
use mail_model::{
    Address, AttachmentCategory, AttachmentKind, AttachmentMeta, AttachmentQuery, AttachmentSort, AttachmentSortKey,
    FolderId, Header, Locator, SearchOptions, Sort, SortKey, SourceId,
};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::StreamableHttpService;
use rmcp::transport::StreamableHttpServerConfig;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use stv_library::Library;
use tokio_util::sync::CancellationToken;

mod links;
mod transport;

/// Most links `read_message` lists.
const MAX_LINKS: usize = 200;
/// Most rows any listing tool returns per call.
const MAX_LIMIT: usize = 100;
const DEFAULT_LIMIT: usize = 25;
/// Deepest page offset accepted. Ranked search keeps `offset + limit` candidates in
/// memory, so an unbounded offset would let one call exhaust it; agents should narrow
/// the query instead of paging this far.
const MAX_OFFSET: usize = 100_000;
/// Default / maximum characters of message or attachment text per call.
const DEFAULT_CHARS: usize = 20_000;
const MAX_CHARS: usize = 100_000;
/// MCP requests are small JSON-RPC calls; anything larger is refused with 413.
const MAX_REQUEST_BYTES: usize = 64 << 10;
/// Longest argument summary / error text kept per access-log entry.
const MAX_LOG_FIELD_CHARS: usize = 300;
/// The access log is moved to `<path>.1` (replacing the previous one) past this size.
const LOG_ROTATE_BYTES: u64 = 4 << 20;

/// Mailboxes agents may see. Updated live by the app; empty = nothing visible.
pub type Shared = Arc<RwLock<HashSet<SourceId>>>;

/// One agent tool call, for the user's activity view.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessLogEntry {
    /// UTC ms.
    pub at: i64,
    /// Agent name from the MCP `initialize` handshake.
    pub client: Option<String>,
    /// Tool name, e.g. `search`.
    pub tool: String,
    /// Short human-readable summary of the arguments.
    pub args: String,
    /// Whether the call succeeded.
    pub ok: bool,
    /// Rows / messages / characters returned (tool-dependent).
    pub items: u64,
    /// Error shown to the agent when `ok` is false.
    pub error: Option<String>,
}

/// Appends entries as JSON lines to a file (rotated past a few MB) and forwards them to
/// a callback.
#[derive(Clone)]
pub struct AccessLog {
    path: PathBuf,
    notify: Arc<dyn Fn(&AccessLogEntry) + Send + Sync>,
    /// Serialises append + rotation across concurrent tool calls.
    write: Arc<Mutex<()>>,
}

impl AccessLog {
    /// Log to `path`; `notify` is called (on a blocking-pool thread) for every entry.
    pub fn new(path: PathBuf, notify: impl Fn(&AccessLogEntry) + Send + Sync + 'static) -> Self {
        AccessLog { path, notify: Arc::new(notify), write: Arc::default() }
    }

    fn record(&self, e: &AccessLogEntry) {
        if let Err(err) = self.append(e) {
            tracing::warn!("agent access log {}: {err}", self.path.display());
        }
        (self.notify)(e);
    }

    fn append(&self, e: &AccessLogEntry) -> std::io::Result<()> {
        use std::io::Write;
        let line = serde_json::to_string(e)?;
        let _guard = self.write.lock().unwrap_or_else(|p| p.into_inner());
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= LOG_ROTATE_BYTES) {
            std::fs::rename(&self.path, rotated(&self.path))?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{line}")
    }
}

fn rotated(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".1");
    PathBuf::from(p)
}

/// At most `max` chars of `s`, with an ellipsis when cut.
fn clip(mut s: String, max: usize) -> String {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
        s.push('…');
    }
    s
}

/// A random 256-bit token, hex-encoded.
pub fn new_token() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("OS random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Running server. [`ServerHandle::shutdown`] waits until the port is released; dropping
/// the handle only signals the server to stop.
pub struct ServerHandle {
    /// Bound address (`127.0.0.1:<port>`).
    pub addr: SocketAddr,
    cancel: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// Grace period for in-flight tool calls before the server task is aborted.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

impl ServerHandle {
    /// Close agent sessions and stop accepting connections, give in-flight requests
    /// [`SHUTDOWN_GRACE`], then abort; returns once the listener is closed, so the port
    /// can be bound again immediately.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(mut task) = self.task.take() {
            if tokio::time::timeout(SHUTDOWN_GRACE, &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Shortest bearer token [`start`] accepts (128 bits as hex); [`new_token`] makes 64 chars.
pub const MIN_TOKEN_LEN: usize = 32;

/// Binds `127.0.0.1:port` (0 = any free port) and serves MCP at `/mcp` on the current
/// tokio runtime. Fails if `token` is shorter than [`MIN_TOKEN_LEN`] (an empty token
/// would let anyone in) or the port stays taken (a just-stopped server gets a moment to
/// release it).
pub async fn start(
    lib: Library,
    shared: Shared,
    log: AccessLog,
    port: u16,
    token: String,
) -> std::io::Result<ServerHandle> {
    if token.len() < MIN_TOKEN_LEN {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "agent access token is too short"));
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut attempt = 0;
    let listener = loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => break l,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && attempt < 10 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => return Err(e),
        }
    };
    let addr = listener.local_addr()?;
    let cancel = CancellationToken::new();
    let mcp = StreamableHttpService::new(
        move || Ok(StvMcp { lib: lib.clone(), shared: shared.clone(), log: log.clone() }),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_max_request_body_bytes(MAX_REQUEST_BYTES)
            .with_cancellation_token(cancel.child_token()),
    );
    let guard =
        Arc::new(Guard { token, hosts: [format!("127.0.0.1:{}", addr.port()), format!("localhost:{}", addr.port())] });
    let app = Router::new().nest_service("/mcp", mcp).layer(middleware::from_fn_with_state(guard, check_request));
    let stop = cancel.clone();
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(stop.cancelled_owned()).await {
            tracing::warn!("agent access server stopped: {e}");
        }
    });
    Ok(ServerHandle { addr, cancel, task: Some(task) })
}

struct Guard {
    token: String,
    /// Accepted `Host` values: the loopback names with the bound port.
    hosts: [String; 2],
}

/// Bearer token, loopback `Host`, and no `Origin` (browsers always send one).
async fn check_request(State(g): State<Arc<Guard>>, req: Request, next: Next) -> Result<Response, StatusCode> {
    let h = req.headers();
    if h.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    let host_ok =
        h.get(header::HOST).and_then(|v| v.to_str().ok()).is_some_and(|host| g.hosts.iter().any(|a| a == host));
    if !host_ok {
        return Err(StatusCode::FORBIDDEN);
    }
    let presented = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map_or("", |(_, t)| t.trim());
    if !constant_time_eq(presented.as_bytes(), g.token.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

/// Compares without an early exit on the first differing byte. Only the length can
/// leak, and the token length is public (64 hex chars).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---- message ids ----

/// Opaque id handed to agents: `<source>:<base64url(locator JSON)>`.
fn message_id(source: SourceId, loc: &Locator) -> String {
    let json = serde_json::to_vec(loc).expect("locator serialises");
    format!("{}:{}", source.0, base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json))
}

fn parse_message_id(id: &str) -> Result<(SourceId, Locator), String> {
    const BAD: &str = "malformed message id; use an id returned by search or list_messages";
    let (s, b64) = id.split_once(':').ok_or(BAD)?;
    let source = SourceId(s.parse().map_err(|_| BAD)?);
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(b64).map_err(|_| BAD)?;
    Ok((source, serde_json::from_slice(&json).map_err(|_| BAD)?))
}

fn address(a: Option<&Address>) -> Value {
    a.map_or(Value::Null, |a| json!({ "name": a.name, "email": a.email }))
}

fn addresses(v: &[Address]) -> Value {
    v.iter().map(|a| address(Some(a))).collect()
}

/// Calendar/contact/task details for agents (see `read_message`): the item's fields with
/// `type` = `appointment` | `contact` | `task`, times as ISO 8601 UTC, calendar days as
/// `YYYY-MM-DD`, addresses as `{name, email}`; unset fields (null, empty lists, `false`)
/// are left out.
fn item_json(item: &mail_model::ItemDetails) -> Value {
    /// Replaces the model's `{"kind": "time"|"date", "value": …}` wrapper with its value.
    fn flatten(v: &mut Value) {
        match v {
            Value::Object(m) => {
                match (m.get("kind").and_then(Value::as_str), m.get("value")) {
                    (Some("time"), Some(Value::Number(n))) if m.len() == 2 => {
                        *v = iso(n.as_i64());
                        return;
                    }
                    (Some("date"), Some(Value::String(d))) if m.len() == 2 => {
                        *v = Value::String(d.clone());
                        return;
                    }
                    _ => {}
                }
                m.values_mut().for_each(flatten);
            }
            Value::Array(a) => a.iter_mut().for_each(flatten),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(item).unwrap_or(Value::Null);
    flatten(&mut v);
    if let Value::Object(m) = &mut v {
        m.retain(|_, f| match f {
            Value::Null | Value::Bool(false) => false,
            Value::Array(a) => !a.is_empty(),
            _ => true,
        });
    }
    v
}

fn iso(ms: Option<i64>) -> Value {
    ms.map_or(Value::Null, |ms| Value::String(stv_library::dates::format_rfc3339(ms)))
}

fn header_row(source: SourceId, h: &Header, folder_name: Option<String>) -> Value {
    json!({
        "id": h.locator.as_ref().map(|l| message_id(source, l)),
        "mailbox": source.0,
        "date": iso(h.date),
        "from": address(h.from.as_ref()),
        "subject": h.subject,
        "folder": folder_name.or_else(|| h.folder.as_ref().map(|f| f.0.clone())),
        "has_attachments": h.has_attachments,
        "size": message_size(h),
    })
}

/// Message size in bytes, `null` when the container does not record it (stored as 0).
fn message_size(h: &Header) -> Value {
    if h.size == 0 {
        Value::Null
    } else {
        json!(h.size)
    }
}

/// Attachment size; `null` for a non-file attachment (embedded message, TNEF) whose
/// container records none, rather than a misleading 0.
fn attachment_size(t: &AttachmentMeta) -> Value {
    if t.size == 0 && !matches!(t.kind, AttachmentKind::File) {
        Value::Null
    } else {
        json!(t.size)
    }
}

/// Locator of the message carried by attachment `index` of `parent`, if it carries one.
fn embedded_locator(parent: &Locator, t: &AttachmentMeta) -> Option<Locator> {
    matches!(t.kind, AttachmentKind::EmbeddedMessage | AttachmentKind::Tnef)
        .then(|| Locator::Embedded { parent: Box::new(parent.clone()), attachment: t.index })
}

/// `{message_id, attachment}` of the message an embedded id is attached to.
fn embedded_in(source: SourceId, loc: &Locator) -> Value {
    match loc {
        Locator::Embedded { parent, attachment } => {
            json!({ "message_id": message_id(source, parent), "attachment": attachment })
        }
        _ => Value::Null,
    }
}

/// `max` chars of `text` starting at char `offset`, with continuation info (null when
/// nothing follows).
fn window(text: &str, offset: usize, max: usize) -> (String, Value) {
    let total = text.chars().count();
    let part: String = text.chars().skip(offset).take(max).collect();
    let end = offset.min(total) + part.chars().count();
    let more = if end < total { json!({ "next_offset": end, "total_chars": total }) } else { Value::Null };
    (part, more)
}

fn chars_of(max: Option<usize>) -> usize {
    max.unwrap_or(DEFAULT_CHARS).clamp(1, MAX_CHARS)
}

fn limit_of(l: Option<usize>) -> usize {
    l.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

fn offset_of(o: Option<usize>) -> Result<usize, String> {
    match o.unwrap_or(0) {
        o if o > MAX_OFFSET => Err(format!("offset is limited to {MAX_OFFSET}; narrow the query instead")),
        o => Ok(o),
    }
}

// ---- tool arguments ----

/// Message order for listings.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum MessageSort {
    #[default]
    Newest,
    Oldest,
    Largest,
    Smallest,
}

impl From<MessageSort> for Sort {
    fn from(s: MessageSort) -> Sort {
        let (key, descending) = match s {
            MessageSort::Newest => (SortKey::Date, true),
            MessageSort::Oldest => (SortKey::Date, false),
            MessageSort::Largest => (SortKey::Size, true),
            MessageSort::Smallest => (SortKey::Size, false),
        };
        Sort { key, descending }
    }
}

/// Search result order.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SearchSort {
    #[default]
    Relevance,
    Newest,
    Oldest,
    Largest,
    Smallest,
}

/// Attachment order.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum AttachmentOrder {
    #[default]
    Newest,
    Oldest,
    Largest,
    Smallest,
    Name,
}

impl From<AttachmentOrder> for AttachmentSort {
    fn from(s: AttachmentOrder) -> AttachmentSort {
        let (key, descending) = match s {
            AttachmentOrder::Newest => (AttachmentSortKey::Date, true),
            AttachmentOrder::Oldest => (AttachmentSortKey::Date, false),
            AttachmentOrder::Largest => (AttachmentSortKey::Size, true),
            AttachmentOrder::Smallest => (AttachmentSortKey::Size, false),
            AttachmentOrder::Name => (AttachmentSortKey::Name, false),
        };
        AttachmentSort { key, descending }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MailboxArgs {
    /// Mailbox id from list_mailboxes.
    mailbox: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchArgs {
    /// Search query. Free words and "exact phrases", plus operators: from: to: cc: subject: body:
    /// attachment: has:attachment is:unread is:flagged is:appointment is:meeting is:contact is:task
    /// label: in:"folder" before:YYYY-MM-DD after:YYYY-MM-DD (appointments by start, tasks by due
    /// date) larger:5M smaller:100K, OR, -word (exclude), word~ (fuzzy). Empty = everything.
    query: String,
    /// Restrict to one mailbox; default: all shared mailboxes.
    mailbox: Option<u32>,
    /// Result order (default relevance).
    sort: Option<SearchSort>,
    /// Rows to skip (from the previous page's offset + row count).
    offset: Option<usize>,
    /// Rows per page: default 25, max 100.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListMessagesArgs {
    /// Mailbox id from list_mailboxes.
    mailbox: u32,
    /// Folder id from list_folders; omit for all mail in the mailbox.
    folder: Option<String>,
    /// Message order (default newest).
    sort: Option<MessageSort>,
    /// Rows to skip.
    offset: Option<usize>,
    /// Rows per page: default 25, max 100.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadMessageArgs {
    /// Message id from search / list_messages.
    id: String,
    /// Character offset into the body (for long messages; see body_continues.next_offset).
    offset: Option<usize>,
    /// Max body characters to return (default 20000, max 100000).
    max_chars: Option<usize>,
    /// Include raw transport headers (default false).
    raw_headers: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReadAttachmentArgs {
    /// Id of the message carrying the attachment.
    id: String,
    /// Attachment index from read_message or list_attachments.
    index: u32,
    /// Character offset into the extracted text (see text_continues.next_offset).
    offset: Option<usize>,
    /// Max characters to return (default 20000, max 100000).
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ThreadArgs {
    /// Id of any message in the conversation.
    id: String,
    /// Entries to skip (oldest first).
    offset: Option<usize>,
    /// Entries per page: default 25, max 100.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListAttachmentsArgs {
    /// Mailbox id from list_mailboxes.
    mailbox: u32,
    /// Words matched against file name, sender and subject; -word excludes.
    text: Option<String>,
    /// One of: image, pdf, document, spreadsheet, presentation, archive, email, calendar, audio, video, text, other.
    category: Option<String>,
    /// Attachment order (default newest).
    sort: Option<AttachmentOrder>,
    /// Include inline images such as signature logos (default false).
    include_inline: Option<bool>,
    /// Rows to skip.
    offset: Option<usize>,
    /// Rows per page: default 25, max 100.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListTaggedArgs {
    /// Restrict to one mailbox; default all shared mailboxes.
    mailbox: Option<u32>,
    /// Restrict to one tag name.
    tag: Option<String>,
    /// Rows to skip.
    offset: Option<usize>,
    /// Rows per page: default 25, max 100.
    limit: Option<usize>,
}

// ---- server ----

/// One MCP session's view of the library.
#[derive(Clone)]
struct StvMcp {
    lib: Library,
    shared: Shared,
    log: AccessLog,
}

type ToolOut = Result<CallToolResult, McpError>;

impl StvMcp {
    fn shared_ids(&self) -> HashSet<SourceId> {
        self.shared.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn check(&self, source: SourceId) -> Result<(), String> {
        if self.shared.read().unwrap_or_else(|p| p.into_inner()).contains(&source) {
            Ok(())
        } else {
            Err(format!("mailbox {} is not shared with agents", source.0))
        }
    }

    /// Runs blocking library work and the access-log write on the blocking pool, then
    /// wraps the result. `args` is the summary shown in the activity log.
    async fn run(
        &self,
        ctx: &RequestContext<RoleServer>,
        tool: &'static str,
        args: String,
        work: impl FnOnce(&StvMcp) -> Result<(Value, u64), String> + Send + 'static,
    ) -> ToolOut {
        let client = ctx.peer.peer_info().map(|i| clip(i.client_info.name.clone(), 100));
        let me = self.clone();
        let out = tokio::task::spawn_blocking(move || {
            let out = work(&me);
            let (ok, items, error) = match &out {
                Ok((_, n)) => (true, *n, None),
                Err(e) => (false, 0, Some(clip(e.clone(), MAX_LOG_FIELD_CHARS))),
            };
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            let args = clip(args, MAX_LOG_FIELD_CHARS);
            me.log.record(&AccessLogEntry { at, client, tool: tool.into(), args, ok, items, error });
            out
        })
        .await
        .unwrap_or_else(|e| Err(format!("internal error: {e}")));
        Ok(match out {
            // Compact JSON: agents pay per token for whitespace.
            Ok((v, _)) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        })
    }

    fn folder_names(&self, source: SourceId) -> HashMap<FolderId, String> {
        self.lib.folders(source).map(|v| v.into_iter().map(|f| (f.id, f.name)).collect()).unwrap_or_default()
    }
}

#[tool_router]
impl StvMcp {
    #[tool(
        description = "List the mailboxes shared with agents (id, name, kind, message count, whether indexing finished, messages that could only be indexed from their headers). Start here."
    )]
    async fn list_mailboxes(&self, ctx: RequestContext<RoleServer>) -> ToolOut {
        self.run(&ctx, "list_mailboxes", String::new(), |me| {
            let shared = me.shared_ids();
            let rows: Vec<Value> = me
                .lib
                .sources()
                .into_iter()
                .filter(|s| shared.contains(&s.id))
                .map(|s| {
                    json!({
                        "mailbox": s.id.0,
                        "name": s.display_name,
                        "kind": s.kind,
                        "messages": s.index.total.unwrap_or(s.index.indexed),
                        "indexing_complete": s.index.done,
                        "index_error": s.index.error,
                        "unreadable_messages": s.index.read_errors,
                    })
                })
                .collect();
            let n = rows.len() as u64;
            Ok((json!({ "mailboxes": rows }), n))
        })
        .await
    }

    #[tool(description = "Folder tree of a mailbox: folder id, name, parent folder id, message count, kind.")]
    async fn list_folders(&self, Parameters(a): Parameters<MailboxArgs>, ctx: RequestContext<RoleServer>) -> ToolOut {
        let args = format!("mailbox {}", a.mailbox);
        self.run(&ctx, "list_folders", args, move |me| {
            let source = SourceId(a.mailbox);
            me.check(source)?;
            let folders = me.lib.folders(source).map_err(|e| e.to_string())?;
            let rows: Vec<Value> = folders
                .iter()
                .map(|f| {
                    json!({
                        "folder": f.id.0,
                        "name": f.name,
                        "parent": f.parent.as_ref().map(|p| &p.0),
                        "messages": f.message_count,
                        "kind": f.kind,
                    })
                })
                .collect();
            let n = rows.len() as u64;
            Ok((json!({ "folders": rows }), n))
        })
        .await
    }

    #[tool(
        description = "Full-text search over message headers, bodies and attachment text in shared mailboxes. Returns message ids with date, sender, subject, folder and a snippet, plus the total hit count. Use read_message for the full text. Paged: offset/limit."
    )]
    async fn search(&self, Parameters(a): Parameters<SearchArgs>, ctx: RequestContext<RoleServer>) -> ToolOut {
        let args = format!("{:?}{}", a.query, a.mailbox.map(|m| format!(" in mailbox {m}")).unwrap_or_default());
        self.run(&ctx, "search", args, move |me| {
            let sources: Vec<SourceId> = match a.mailbox {
                Some(m) => {
                    me.check(SourceId(m))?;
                    vec![SourceId(m)]
                }
                None => me.shared_ids().into_iter().collect(),
            };
            if sources.is_empty() {
                return Err("no mailboxes are shared with agents".into());
            }
            let sort = match a.sort.unwrap_or_default() {
                SearchSort::Relevance => None,
                SearchSort::Newest => Some(MessageSort::Newest.into()),
                SearchSort::Oldest => Some(MessageSort::Oldest.into()),
                SearchSort::Largest => Some(MessageSort::Largest.into()),
                SearchSort::Smallest => Some(MessageSort::Smallest.into()),
            };
            let opts = SearchOptions {
                offset: offset_of(a.offset)?,
                limit: limit_of(a.limit),
                sources,
                sort,
                ..Default::default()
            };
            let res = me.lib.search(&a.query, &opts).map_err(|e| e.to_string())?;
            let hits: Vec<Value> = res
                .hits
                .iter()
                .map(|h| {
                    let mut row = header_row(h.source, &h.header, None);
                    row["snippet"] = Value::String(strip_snippet(&h.snippet));
                    row
                })
                .collect();
            let n = hits.len() as u64;
            Ok((json!({ "total": res.total, "offset": opts.offset, "results": hits }), n))
        })
        .await
    }

    #[tool(
        description = "List messages of a mailbox folder (or all mail when folder is omitted), newest first by default, with the total count. Paged: offset/limit."
    )]
    async fn list_messages(
        &self,
        Parameters(a): Parameters<ListMessagesArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolOut {
        let args = format!("mailbox {} folder {}", a.mailbox, a.folder.as_deref().unwrap_or("(all)"));
        self.run(&ctx, "list_messages", args, move |me| {
            let source = SourceId(a.mailbox);
            me.check(source)?;
            let offset = offset_of(a.offset)?;
            let limit = limit_of(a.limit);
            let sort = Sort::from(a.sort.unwrap_or_default());
            let (total, headers) = match a.folder {
                Some(f) => {
                    let fid = FolderId(f);
                    let total = me.lib.count(source, &fid).map_err(|e| e.to_string())?;
                    let range = offset as u64..(offset + limit) as u64;
                    (total, me.lib.list(source, &fid, range, sort).map_err(|e| e.to_string())?)
                }
                None => {
                    let opts =
                        SearchOptions { offset, limit, sources: vec![source], sort: Some(sort), ..Default::default() };
                    let res = me.lib.search("", &opts).map_err(|e| e.to_string())?;
                    (res.total, res.hits.into_iter().map(|h| h.header).collect())
                }
            };
            let names = me.folder_names(source);
            let rows: Vec<Value> = headers
                .iter()
                .map(|h| header_row(source, h, h.folder.as_ref().and_then(|f| names.get(f).cloned())))
                .collect();
            let n = rows.len() as u64;
            Ok((json!({ "total": total, "offset": offset, "messages": rows }), n))
        })
        .await
    }

    #[tool(
        description = "Read one message (also an attached/embedded email, by the message_id that read_message or read_attachment gives for it): headers, recipients, body as plain text (paged by characters), links (visible text vs real target URL, mismatch flag, unwrapped SafeLinks/URL Defense), transport (return path, Received hops, first external server, SPF/DKIM/DMARC results, reply-to, spam verdict), attachments (index, name, type, size; embedded emails carry a message_id to open with read_message) and tags. Calendar entries, meeting messages, contacts and tasks add `item`: {type: appointment|contact|task, plus only the fields that are set: appointment start, end (ISO 8601 UTC, or YYYY-MM-DD for all-day; end exclusive), all_day, location, organizer, required, optional, resources ({name, email}), busy_status, recurring, recurrence, meeting (request|cancellation|accepted|tentative|declined|counter; counter = an attendee proposes a new time, start/end are the proposal), uid; contact display_name, given_name, surname, company, job_title, department, emails ({name, email}), phones and addresses ([{label, value}]), birthday, web_page; task status, percent_complete, start, due, completed, owner, uid}."
    )]
    async fn read_message(
        &self,
        Parameters(a): Parameters<ReadMessageArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolOut {
        let args = format!("id {}", a.id);
        self.run(&ctx, "read_message", args, move |me| {
            let (source, loc) = parse_message_id(&a.id)?;
            me.check(source)?;
            let msg = me.lib.message(source, &loc).map_err(|e| e.to_string())?;
            let h = &msg.header;
            let names = me.folder_names(source);
            let tags = me
                .lib
                .tags_of(&[stv_library::TagTarget { source, message: loc.clone(), attachment: None }])
                .pop()
                .unwrap_or_default();
            let attachments: Vec<Value> = msg
                .attachments
                .iter()
                .map(|t| {
                    let mut row = json!({
                        "index": t.index,
                        "name": t.filename,
                        "mime_type": t.mime_type,
                        "size": attachment_size(t),
                        "inline": t.inline,
                        "embedded_message": matches!(t.kind, AttachmentKind::EmbeddedMessage | AttachmentKind::Tnef),
                    });
                    if let Some(eloc) = embedded_locator(&loc, t) {
                        row["message_id"] = json!(message_id(source, &eloc));
                    }
                    row
                })
                .collect();
            let (links, links_truncated) =
                msg.body.html.as_deref().map_or((Vec::new(), false), |html| links::extract(html, MAX_LINKS));
            let transport = msg.raw_headers.as_deref().and_then(transport::summary);
            let body = stv_index::extract::body_text(msg.body, usize::MAX);
            let (text, more) = window(&body, a.offset.unwrap_or(0), chars_of(a.max_chars));
            let n = text.chars().count() as u64;
            let mut v = json!({
                "id": a.id,
                "mailbox": source.0,
                "date": iso(h.date),
                "from": address(h.from.as_ref()),
                "to": addresses(&h.to),
                "cc": addresses(&h.cc),
                "bcc": addresses(&h.bcc),
                "subject": h.subject,
                "folder": h.folder.as_ref().map(|f| names.get(f).cloned().unwrap_or_else(|| f.0.clone())),
                "message_id": h.message_id,
                "size": message_size(h),
                "embedded_in": embedded_in(source, &loc),
                "tags": tags,
                "attachments": attachments,
                "links": links,
                "transport": transport,
                "body": text,
                "body_continues": more,
            });
            if links_truncated {
                v["links_truncated"] = true.into();
            }
            if let Some(item) = &h.item {
                v["item"] = item_json(item);
            }
            if v["transport"].is_null() {
                v["transport_note"] = "no internet headers stored (typical for internal Exchange mail, drafts, or \
                                       attached items saved without headers)"
                    .into();
            }
            if a.raw_headers.unwrap_or(false) {
                v["raw_headers"] = json!(msg.raw_headers);
            }
            Ok((v, n))
        })
        .await
    }

    #[tool(
        description = "Text of one attachment (PDF, Word, Excel, PowerPoint, text, HTML), paged by characters. For an attached/embedded email it returns a summary plus a message_id: call read_message with that id for its full headers, links, transport/authentication details and its own attachments (nesting works to any depth). Images and other binaries return metadata only."
    )]
    async fn read_attachment(
        &self,
        Parameters(a): Parameters<ReadAttachmentArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolOut {
        let args = format!("id {} attachment {}", a.id, a.index);
        self.run(&ctx, "read_attachment", args, move |me| {
            let (source, loc) = parse_message_id(&a.id)?;
            me.check(source)?;
            let msg = me.lib.message(source, &loc).map_err(|e| e.to_string())?;
            let meta = msg
                .attachments
                .iter()
                .find(|t| t.index == a.index)
                .ok_or_else(|| format!("message has no attachment {}", a.index))?;
            let mut embedded = Value::Null;
            let text = match embedded_locator(&loc, meta) {
                Some(eloc) => {
                    let inner = me.lib.message(source, &eloc).map_err(|e| e.to_string())?;
                    let from = inner.header.from.as_ref().and_then(|f| f.email.as_ref().or(f.name.as_ref()));
                    let text = format!(
                        "Subject: {}\nFrom: {}\nDate: {}\n\n{}",
                        inner.header.subject,
                        from.map_or("", String::as_str),
                        iso(inner.header.date).as_str().unwrap_or(""),
                        stv_index::extract::body_text(inner.body, usize::MAX)
                    );
                    embedded = json!({
                        "message_id": message_id(source, &eloc),
                        "subject": inner.header.subject,
                        "from": address(inner.header.from.as_ref()),
                        "date": iso(inner.header.date),
                        "attachments": inner.attachments.len(),
                        "note": "call read_message with message_id for full headers, links, transport details and its attachments",
                    });
                    Some(text)
                }
                _ if stv_index::extract::attachment_kind(meta).is_some() => {
                    let bytes = me.lib.attachment(source, &loc, a.index).map_err(|e| e.to_string())?;
                    stv_index::extract::attachment_text(meta, &bytes)
                }
                _ => None,
            };
            let mut v = json!({ "name": meta.filename, "mime_type": meta.mime_type, "size": attachment_size(meta) });
            if !embedded.is_null() {
                v["embedded_message"] = embedded;
            }
            match text {
                Some(t) => {
                    let (part, more) = window(&t, a.offset.unwrap_or(0), chars_of(a.max_chars));
                    let n = part.chars().count() as u64;
                    v["text"] = Value::String(part);
                    v["text_continues"] = more;
                    Ok((v, n))
                }
                None => {
                    v["text"] = Value::Null;
                    v["note"] = "no text could be extracted from this attachment type".into();
                    Ok((v, 0))
                }
            }
        })
        .await
    }

    #[tool(
        description = "The conversation containing a message, oldest first: one entry per email (copies in other folders merged, listed in also_in) with a preview of that email's own text, plus the total entry count. Limited to shared mailboxes. Paged: offset/limit."
    )]
    async fn get_thread(&self, Parameters(a): Parameters<ThreadArgs>, ctx: RequestContext<RoleServer>) -> ToolOut {
        let args = format!("id {}", a.id);
        self.run(&ctx, "get_thread", args, move |me| {
            let (source, loc) = parse_message_id(&a.id)?;
            me.check(source)?;
            let offset = offset_of(a.offset)?;
            // Scoped in the library, so copies, folders and previews in unshared mailboxes
            // neither appear nor shape the merged entries.
            let items = me.lib.conversation(source, &loc, Some(&me.shared_ids())).map_err(|e| e.to_string())?;
            let rows: Vec<Value> = items
                .iter()
                .skip(offset)
                .take(limit_of(a.limit))
                .map(|c| {
                    let mut r = header_row(c.source, &c.header, None);
                    r["preview"] = json!(c.preview);
                    r["also_in"] = json!(c.also_in);
                    r["is_requested_message"] = json!(c.current);
                    r
                })
                .collect();
            let n = rows.len() as u64;
            Ok((json!({ "total": items.len(), "offset": offset, "conversation": rows }), n))
        })
        .await
    }

    #[tool(
        description = "List attachments in a mailbox (name, type, size, date, sender, carrying message id, index), with optional text/category filters and the total count. Inline signature images are hidden unless include_inline is true. Paged: offset/limit."
    )]
    async fn list_attachments(
        &self,
        Parameters(a): Parameters<ListAttachmentsArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> ToolOut {
        let args = format!("mailbox {} {:?}", a.mailbox, a.text.as_deref().unwrap_or(""));
        self.run(&ctx, "list_attachments", args, move |me| {
            let source = SourceId(a.mailbox);
            me.check(source)?;
            let categories = match a.category.as_deref() {
                None | Some("") => Vec::new(),
                Some(c) => vec![AttachmentCategory::ALL.into_iter().find(|x| x.as_str() == c).ok_or_else(|| {
                    let all: Vec<&str> = AttachmentCategory::ALL.iter().map(|x| x.as_str()).collect();
                    format!("unknown category {c:?}; use one of {}", all.join(", "))
                })?],
            };
            let q = AttachmentQuery {
                sources: vec![source],
                text: a.text.unwrap_or_default(),
                categories,
                include_inline: a.include_inline.unwrap_or(false),
                sort: a.sort.unwrap_or_default().into(),
                offset: offset_of(a.offset)?,
                limit: limit_of(a.limit),
                ..Default::default()
            };
            let page = me.lib.attachments(&q).map_err(|e| e.to_string())?;
            let rows: Vec<Value> = page
                .rows
                .iter()
                .map(|r| {
                    json!({
                        "message_id": message_id(r.source, &r.message),
                        "index": r.index,
                        "name": r.filename,
                        "category": r.category.as_str(),
                        "size": r.size,
                        "date": iso(r.date),
                        "from": address(r.from.as_ref()),
                        "subject": r.subject,
                    })
                })
                .collect();
            let n = rows.len() as u64;
            Ok((json!({ "total": page.total, "offset": q.offset, "attachments": rows }), n))
        })
        .await
    }

    #[tool(
        description = "Emails and attachments the user has tagged in the app (with their tags), newest-tagged first, with the total count. Paged: offset/limit."
    )]
    async fn list_tagged(&self, Parameters(a): Parameters<ListTaggedArgs>, ctx: RequestContext<RoleServer>) -> ToolOut {
        let args = format!("mailbox {:?} tag {:?}", a.mailbox, a.tag);
        self.run(&ctx, "list_tagged", args, move |me| {
            if let Some(m) = a.mailbox {
                me.check(SourceId(m))?;
            }
            let offset = offset_of(a.offset)?;
            let shared = me.shared_ids();
            let visible: Vec<_> = me
                .lib
                .tagged(a.mailbox.map(SourceId), a.tag.as_deref())
                .into_iter()
                .filter(|i| shared.contains(&i.target.source))
                .collect();
            let rows: Vec<Value> = visible
                .iter()
                .skip(offset)
                .take(limit_of(a.limit))
                .map(|i| {
                    let mut r = header_row(i.target.source, &i.header, None);
                    r["id"] = json!(message_id(i.target.source, &i.target.message));
                    r["tags"] = json!(i.tags);
                    if let Some(att) = &i.attachment {
                        r["attachment"] = json!({ "index": att.index, "name": att.filename, "size": att.size });
                    }
                    r
                })
                .collect();
            let n = rows.len() as u64;
            Ok((json!({ "total": visible.len(), "offset": offset, "tagged": rows }), n))
        })
        .await
    }
}

/// Snippets arrive as HTML with `<b>` highlights; agents want plain text.
fn strip_snippet(s: &str) -> String {
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
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#x27;", "'").replace("&amp;", "&")
}

#[tool_handler]
impl ServerHandler for StvMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_instructions(
                "Read-only access to email archives the user shared from STViewer. Start with list_mailboxes, \
                 then search (query syntax: from: to: subject: has:attachment after:YYYY-MM-DD ...) or \
                 list_messages, and read_message / read_attachment for full text. read_message also lists the \
                 body's links (with real targets and a mismatch flag) and a transport summary (Received hops, \
                 SPF/DKIM/DMARC); attached emails get their own message_id, readable with read_message. \
                 Message ids are opaque; pass them back unchanged. Listings are paged (offset/limit, max 100 rows) and long texts \
                 are paged by characters (offset/max_chars)."
                    .to_string(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_json_is_compact_with_iso_times_and_plain_days() {
        use mail_model::{Appointment, Contact, ItemDetails, Labeled, When};
        let v = item_json(&ItemDetails::Appointment(Appointment {
            start: Some(When::Time(983_196_000_000)),
            end: Some(When::Date("2001-02-27".into())),
            location: Some("Room 3321".into()),
            organizer: Some(Address { name: None, email: Some("org@example.org".into()) }),
            ..Appointment::default()
        }));
        assert_eq!(
            v,
            json!({
                "type": "appointment",
                "start": "2001-02-26T14:00:00Z",
                "end": "2001-02-27",
                "location": "Room 3321",
                "organizer": { "name": null, "email": "org@example.org" },
            })
        );
        let counter = ItemDetails::Appointment(Appointment {
            meeting: Some(mail_model::MeetingKind::Counter),
            ..Appointment::default()
        });
        assert_eq!(item_json(&counter)["meeting"], "counter");
        let v = item_json(&ItemDetails::Contact(Contact {
            birthday: Some(When::Date("1970-05-07".into())),
            phones: vec![Labeled { label: "mobile".into(), value: "+1 555 0100".into() }],
            ..Contact::default()
        }));
        assert_eq!(
            v,
            json!({
                "type": "contact",
                "birthday": "1970-05-07",
                "phones": [{ "label": "mobile", "value": "+1 555 0100" }],
            })
        );
    }

    #[test]
    fn window_pages_on_char_boundaries() {
        let (part, more) = window("héllo wörld", 3, 4);
        assert_eq!(part, "lo w");
        assert_eq!(more, json!({ "next_offset": 7, "total_chars": 11 }));
        let (part, more) = window("héllo", 3, 10);
        assert_eq!((part.as_str(), more), ("lo", Value::Null));
        // An offset past the end returns nothing and no continuation.
        let (part, more) = window("abc", usize::MAX, 10);
        assert_eq!((part.as_str(), more), ("", Value::Null));
    }

    #[test]
    fn message_ids_round_trip_and_reject_garbage() {
        let loc = Locator::Part { part: 1, inner: Box::new(Locator::Pst { nid: 0x200024 }) };
        let id = message_id(SourceId(7), &loc);
        assert_eq!(parse_message_id(&id).unwrap(), (SourceId(7), loc));
        for bad in ["", "7", "x:abc", "7:!!", "7:e30"] {
            assert!(parse_message_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn snippets_lose_markup_and_entities() {
        assert_eq!(strip_snippet("a <b>x&amp;y</b> &lt;z&gt;"), "a x&y <z>");
    }

    #[test]
    fn clip_cuts_on_char_boundary() {
        assert_eq!(clip("äöü".into(), 2), "äö…");
        assert_eq!(clip("abc".into(), 3), "abc");
    }

    #[test]
    fn access_log_rotates_without_losing_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("access.log");
        let log = AccessLog::new(path.clone(), |_| {});
        let entry = |i: u64| AccessLogEntry {
            at: 0,
            client: None,
            tool: "search".into(),
            args: "x".repeat(1000),
            ok: true,
            items: i,
            error: None,
        };
        // Enough entries to cross the rotation size once.
        let per_line = serde_json::to_string(&entry(0)).unwrap().len() as u64 + 1;
        let n = LOG_ROTATE_BYTES / per_line + 10;
        for i in 0..n {
            log.record(&entry(i));
        }
        assert!(std::fs::metadata(&path).unwrap().len() < LOG_ROTATE_BYTES);
        let items = |p: &Path| -> Vec<u64> {
            let text = std::fs::read_to_string(p).unwrap();
            text.lines().map(|l| serde_json::from_str::<AccessLogEntry>(l).unwrap().items).collect()
        };
        let all: Vec<u64> = [items(&rotated(&path)), items(&path)].concat();
        assert_eq!(all, (0..n).collect::<Vec<_>>());
    }
}
