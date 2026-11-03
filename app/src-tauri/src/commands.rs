//! Tauri commands. Every library call runs on the blocking pool: synchronous Tauri
//! commands execute on the main (UI) thread, which must never wait on disk, the index or
//! a library lock held by a long operation.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mail_model::{
    AttachmentPage, AttachmentQuery, FolderId, Header, IndexProgress, Locator, Message, SearchOptions, SearchResults,
    Sort, SortKey, SourceId,
};
use serde::{Deserialize, Serialize};
use stv_library::{ExportFormat, ExportReport, Library, SourceInfo, Tag, TagTarget, TaggedItem};
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::files;
use crate::AppState;

/// Error shape seen by the frontend.
#[derive(Debug, Serialize)]
pub struct CmdError {
    kind: &'static str,
    message: String,
}

impl From<mail_model::Error> for CmdError {
    fn from(e: mail_model::Error) -> Self {
        use mail_model::Error as E;
        let kind = match &e {
            E::Io(_) => "io",
            E::Format(_) => "format",
            E::NotFound(_) => "not_found",
            E::Unsupported(_) => "unsupported",
            E::Encrypted(_) => "encrypted",
            E::Cancelled => "cancelled",
            E::InUse { .. } => "in_use",
            E::Other(_) => "other",
        };
        CmdError { kind, message: e.to_string() }
    }
}

impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> Self {
        CmdError { kind: "io", message: e.to_string() }
    }
}

impl From<String> for CmdError {
    fn from(message: String) -> Self {
        CmdError { kind: "other", message }
    }
}

type CmdResult<T> = Result<T, CmdError>;

async fn blocking<T, F>(f: F) -> CmdResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> CmdResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CmdError { kind: "other", message: format!("background task failed: {e}") })?
}

fn lib(state: &State<'_, AppState>) -> Library {
    state.lib.clone()
}

// ---- agent access (MCP) ----

#[tauri::command]
pub async fn agent_access_get(state: State<'_, AppState>) -> CmdResult<crate::agent::AgentAccessState> {
    let agent = state.agent.clone();
    blocking(move || Ok(agent.state())).await
}

/// Turns agent access on/off and sets port + shared mailboxes; (re)starts the server.
#[tauri::command]
pub async fn agent_access_set(
    state: State<'_, AppState>,
    enabled: bool,
    port: u16,
    allowed_sources: Vec<SourceId>,
) -> CmdResult<crate::agent::AgentAccessState> {
    let agent = state.agent.clone();
    Ok(agent.set(enabled, port, allowed_sources).await?)
}

#[tauri::command]
pub async fn agent_access_regenerate_token(state: State<'_, AppState>) -> CmdResult<crate::agent::AgentAccessState> {
    let agent = state.agent.clone();
    Ok(agent.regenerate_token().await?)
}

#[derive(Serialize)]
pub struct AddResult {
    added: Vec<SourceInfo>,
    errors: Vec<(PathBuf, String)>,
}

/// Adds files and/or directories in one library call, so the parts of a split archive
/// picked or dropped together become one source. Failures are reported per path so one
/// unreadable item does not discard the rest of a multi-file drop.
#[tauri::command]
pub async fn add_paths(state: State<'_, AppState>, paths: Vec<PathBuf>) -> CmdResult<AddResult> {
    let lib = lib(&state);
    blocking(move || {
        let outcome = lib.add_paths(&paths);
        Ok(AddResult {
            added: outcome.added,
            errors: outcome.errors.into_iter().map(|(p, e)| (p, e.to_string())).collect(),
        })
    })
    .await
}

#[tauri::command]
pub async fn remove_source(state: State<'_, AppState>, source: SourceId) -> CmdResult<()> {
    let lib = lib(&state);
    blocking(move || Ok(lib.remove_source(source)?)).await
}

#[tauri::command]
pub async fn sources(state: State<'_, AppState>) -> CmdResult<Vec<SourceInfo>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.sources())).await
}

#[tauri::command]
pub async fn folders(state: State<'_, AppState>, source: SourceId) -> CmdResult<Vec<mail_model::Folder>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.folders(source)?)).await
}

#[tauri::command]
pub async fn count(state: State<'_, AppState>, source: SourceId, folder: FolderId) -> CmdResult<u64> {
    let lib = lib(&state);
    blocking(move || Ok(lib.count(source, &folder)?)).await
}

#[tauri::command]
pub async fn list(
    state: State<'_, AppState>,
    source: SourceId,
    folder: FolderId,
    offset: u64,
    limit: u64,
    sort: Sort,
) -> CmdResult<Vec<Header>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.list(source, &folder, offset..offset.saturating_add(limit), sort)?)).await
}

/// Loads one message for the reading pane.
#[tauri::command]
pub async fn message(state: State<'_, AppState>, source: SourceId, locator: Locator) -> CmdResult<Message> {
    let st = state.inner();
    let lib = st.lib.clone();
    let loc = locator.clone();
    let msg = blocking(move || Ok(lib.message(source, &loc)?)).await?;
    st.remember(source, locator, Arc::new(msg.attachments.clone()));
    Ok(msg)
}

/// Writes the attachment to a fresh private directory, marked as downloaded from the
/// internet, and opens it with the OS default handler. Programs and scripts are opened
/// only after the user confirms a native warning; returns `None` when they decline.
#[tauri::command]
pub async fn open_attachment(
    app: tauri::AppHandle,
    source: SourceId,
    locator: Locator,
    index: u32,
) -> CmdResult<Option<PathBuf>> {
    blocking(move || {
        let st = app.state::<AppState>();
        // Cached when the message was displayed; otherwise this re-reads it once.
        let atts = st.attachments(source, &locator)?;
        let meta = atts
            .iter()
            .find(|a| a.index == index)
            .ok_or_else(|| mail_model::Error::NotFound(format!("attachment {index}")))?;
        let name = files::attachment_file_name(meta);
        if files::runs_code(&name) && !confirm_open_program(&app, &name) {
            return Ok(None);
        }
        let bytes = st.lib.attachment(source, &locator, index)?;
        let path = files::new_open_dir(&st.open_root)?.join(name);
        files::write_untrusted(&path, &bytes)?;
        // Starting the handler (a process spawn, or ShellExecute on Windows) blocks too.
        app.opener()
            .open_path(path.to_string_lossy(), None::<&str>)
            .map_err(|e| CmdError { kind: "other", message: e.to_string() })?;
        Ok(Some(path))
    })
    .await
}

/// Native, window-modal warning before launching an executable attachment. Blocks, so
/// it must run off the main thread.
fn confirm_open_program(app: &tauri::AppHandle, name: &str) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let mut dialog = app
        .dialog()
        .message(format!(
            "\"{name}\" is a program or script. Opening it runs code on this computer with your \
             permissions.\n\nOnly open it if you trust the sender and expected this file."
        ))
        .title("Open program attachment?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Open anyway".into(), "Cancel".into()));
    if let Some(w) = app.get_webview_window("main") {
        dialog = dialog.parent(&w);
    }
    let open = dialog.blocking_show();
    tracing::info!("program attachment {name:?}: user {}", if open { "chose to open it" } else { "cancelled" });
    open
}

/// Opens a directory (e.g. an export destination) in the OS file manager.
#[tauri::command]
pub async fn open_folder(app: tauri::AppHandle, path: PathBuf) -> CmdResult<()> {
    blocking(move || {
        if !path.is_dir() {
            return Err(CmdError { kind: "not_found", message: format!("{} is not a folder", path.display()) });
        }
        app.opener()
            .open_path(path.to_string_lossy(), None::<&str>)
            .map_err(|e| CmdError { kind: "other", message: e.to_string() })
    })
    .await
}

#[tauri::command]
pub async fn save_attachment(
    state: State<'_, AppState>,
    source: SourceId,
    locator: Locator,
    index: u32,
    dest: PathBuf,
) -> CmdResult<()> {
    let lib = lib(&state);
    blocking(move || {
        let bytes = lib.attachment(source, &locator, index)?;
        files::write_untrusted(&dest, &bytes)?;
        Ok(())
    })
    .await
}

/// Result of saving several attachments into one folder.
#[derive(Serialize, Default)]
pub struct SaveAllReport {
    written: Vec<PathBuf>,
    /// `(file name, error)` per attachment that could not be saved.
    failed: Vec<(String, String)>,
}

/// Attachments [`SaveAllReport::save_all`] reads at once.
const SAVE_READ_AHEAD: usize = 4;

impl SaveAllReport {
    /// Writes `bytes` as `name` in `dir`, renamed `name (n).ext` if that file already exists.
    fn save(&mut self, dir: &Path, name: String, bytes: mail_model::Result<Vec<u8>>) {
        let res = bytes.map_err(|e| e.to_string()).and_then(|bytes| {
            let path = files::unique_path(dir, &name);
            files::write_untrusted(&path, &bytes).map(|()| path).map_err(|e| e.to_string())
        });
        match res {
            Ok(p) => self.written.push(p),
            Err(e) => self.failed.push((name, e)),
        }
    }

    /// Saves attachments into `dir` in the order given: each item is its file name (or the
    /// `(label, error)` to report when it has none) and what `read` loads its bytes from.
    /// Up to [`SAVE_READ_AHEAD`] are read in parallel, as reading one can mean decoding or
    /// re-parsing its whole message; files are still written one at a time, in order, so
    /// the names chosen and the report are those of saving them one after another.
    fn save_all<T: Send>(
        &mut self,
        dir: &Path,
        mut items: impl Iterator<Item = (Result<String, (String, String)>, T)>,
        read: impl Fn(T) -> mail_model::Result<Vec<u8>> + Sync,
    ) {
        let read = &read;
        std::thread::scope(|s| {
            let mut pending = VecDeque::with_capacity(SAVE_READ_AHEAD);
            loop {
                while pending.len() < SAVE_READ_AHEAD {
                    let Some((name, item)) = items.next() else { break };
                    pending.push_back(name.map(|name| (name, s.spawn(move || read(item)))));
                }
                match pending.pop_front() {
                    Some(Ok((name, bytes))) => {
                        // A panicking read fails the command, as it did when reading inline.
                        let bytes = bytes.join().unwrap_or_else(|p| std::panic::resume_unwind(p));
                        self.save(dir, name, bytes);
                    }
                    Some(Err((label, e))) => self.failed.push((label, e)),
                    None => break,
                }
            }
        });
    }
}

/// Saves every attachment of a message into `dir`, de-duplicating file names.
#[tauri::command]
pub async fn save_attachments(
    state: State<'_, AppState>,
    source: SourceId,
    locator: Locator,
    dir: PathBuf,
) -> CmdResult<SaveAllReport> {
    let lib = lib(&state);
    blocking(move || {
        let msg = lib.message(source, &locator)?;
        std::fs::create_dir_all(&dir)?;
        let mut report = SaveAllReport::default();
        let names = msg.attachments.iter().map(|att| (Ok(files::attachment_file_name(att)), att.index));
        report.save_all(&dir, names, |index| lib.attachment(source, &locator, index));
        Ok(report)
    })
    .await
}

/// One attachment addressed by the UI.
#[derive(Deserialize)]
pub struct AttachmentRef {
    source: SourceId,
    message: Locator,
    index: u32,
}

/// Saves attachments, possibly of different messages, into `dir` under their sanitised
/// names, never overwriting existing files.
#[tauri::command]
pub async fn save_attachment_list(
    app: tauri::AppHandle,
    items: Vec<AttachmentRef>,
    dir: PathBuf,
) -> CmdResult<SaveAllReport> {
    blocking(move || {
        let st = app.state::<AppState>();
        std::fs::create_dir_all(&dir)?;
        let mut report = SaveAllReport::default();
        // Names are looked up here, in order, so attachments of one message share one
        // (cached) read of it; only the attachment bytes are read in parallel.
        let named = items.iter().map(|r| {
            let name = st.attachments(r.source, &r.message).map_err(|e| e.to_string()).and_then(|atts| {
                let meta = atts.iter().find(|a| a.index == r.index).ok_or("no such attachment")?;
                Ok(files::attachment_file_name(meta))
            });
            (name.map_err(|e| (format!("attachment {}", r.index), e)), r)
        });
        report.save_all(&dir, named, |r| st.lib.attachment(r.source, &r.message, r.index));
        Ok(report)
    })
    .await
}

/// Conversation of a message across all sources: one dated entry per email, with previews.
#[tauri::command]
pub async fn conversation(
    state: State<'_, AppState>,
    source: SourceId,
    locator: Locator,
) -> CmdResult<Vec<stv_library::ConversationItem>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.conversation(source, &locator, None)?)).await
}

#[tauri::command]
pub async fn search(state: State<'_, AppState>, query: String, opts: SearchOptions) -> CmdResult<SearchResults> {
    let lib = lib(&state);
    blocking(move || Ok(lib.search(&query, &opts)?)).await
}

#[tauri::command]
pub async fn attachments(state: State<'_, AppState>, query: AttachmentQuery) -> CmdResult<AttachmentPage> {
    let lib = lib(&state);
    blocking(move || Ok(lib.attachments(&query)?)).await
}

/// Characters of extracted text returned for a text-card tile.
const TEXT_CARD_CHARS: usize = 1500;

/// Beginning of the text extracted from a document attachment (for tiles that have no
/// picture), or `None` when the type has no extractable text.
#[tauri::command]
pub async fn attachment_text(
    app: tauri::AppHandle,
    source: SourceId,
    locator: Locator,
    index: u32,
) -> CmdResult<Option<String>> {
    blocking(move || {
        let st = app.state::<AppState>();
        let atts = st.attachments(source, &locator)?;
        let meta = atts
            .iter()
            .find(|a| a.index == index)
            .ok_or_else(|| mail_model::Error::NotFound(format!("attachment {index}")))?;
        // Extraction refuses larger files anyway: do not load them.
        if stv_index::extract::attachment_kind(meta).is_none() || meta.size > stv_index::extract::MAX_ATTACHMENT_BYTES {
            return Ok(None);
        }
        let bytes = st.lib.attachment(source, &locator, index)?;
        Ok(stv_index::extract::attachment_text(meta, &bytes).map(|t| t.chars().take(TEXT_CARD_CHARS).collect()))
    })
    .await
}

#[tauri::command]
pub async fn tags(state: State<'_, AppState>) -> CmdResult<Vec<Tag>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.tags())).await
}

#[tauri::command]
pub async fn create_tag(state: State<'_, AppState>, name: String, color: Option<String>) -> CmdResult<Tag> {
    let lib = lib(&state);
    blocking(move || Ok(lib.create_tag(&name, color.as_deref())?)).await
}

#[tauri::command]
pub async fn rename_tag(state: State<'_, AppState>, old: String, new: String) -> CmdResult<()> {
    let lib = lib(&state);
    blocking(move || Ok(lib.rename_tag(&old, &new)?)).await
}

#[tauri::command]
pub async fn delete_tag(state: State<'_, AppState>, name: String) -> CmdResult<()> {
    let lib = lib(&state);
    blocking(move || Ok(lib.delete_tag(&name)?)).await
}

/// Tagging reads each newly tagged item once (for its snapshot), so it runs off the UI thread.
#[tauri::command]
pub async fn set_tag(state: State<'_, AppState>, targets: Vec<TagTarget>, tag: String, on: bool) -> CmdResult<()> {
    let lib = lib(&state);
    blocking(move || Ok(lib.set_tag(&targets, &tag, on)?)).await
}

#[tauri::command]
pub async fn tags_of(state: State<'_, AppState>, targets: Vec<TagTarget>) -> CmdResult<Vec<Vec<String>>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.tags_of(&targets))).await
}

#[tauri::command]
pub async fn tagged(
    state: State<'_, AppState>,
    source: Option<SourceId>,
    tag: Option<String>,
) -> CmdResult<Vec<TaggedItem>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.tagged(source, tag.as_deref()))).await
}

#[tauri::command]
pub async fn tagged_count(state: State<'_, AppState>, source: SourceId) -> CmdResult<u64> {
    let lib = lib(&state);
    blocking(move || Ok(lib.tagged_count(source))).await
}

#[tauri::command]
pub async fn index_progress(state: State<'_, AppState>) -> CmdResult<Vec<IndexProgress>> {
    let lib = lib(&state);
    blocking(move || Ok(lib.index_progress())).await
}

#[tauri::command]
pub async fn reindex(state: State<'_, AppState>, source: SourceId) -> CmdResult<()> {
    let lib = lib(&state);
    blocking(move || Ok(lib.reindex(source)?)).await
}

#[tauri::command]
pub async fn export(
    state: State<'_, AppState>,
    items: Vec<(SourceId, Locator)>,
    format: ExportFormat,
    dest: PathBuf,
) -> CmdResult<ExportReport> {
    let lib = lib(&state);
    blocking(move || Ok(lib.export(&items, format, &dest)?)).await
}

/// Exports every message in a folder without shipping its locators through IPC.
#[tauri::command]
pub async fn export_folder(
    state: State<'_, AppState>,
    source: SourceId,
    folder: FolderId,
    format: ExportFormat,
    dest: PathBuf,
) -> CmdResult<ExportReport> {
    let lib = lib(&state);
    blocking(move || {
        const CHUNK: u64 = 5000;
        let total = lib.count(source, &folder)?;
        let mut items = Vec::with_capacity(total as usize);
        let sort = Sort { key: SortKey::Native, descending: false };
        let mut offset = 0;
        while offset < total {
            let page = lib.list(source, &folder, offset..(offset + CHUNK).min(total), sort)?;
            if page.is_empty() {
                break;
            }
            offset += page.len() as u64;
            items.extend(page.into_iter().filter_map(|h| h.locator).map(|l| (source, l)));
        }
        Ok(lib.export(&items, format, &dest)?)
    })
    .await
}

/// Exports every hit of a search (all pages).
#[tauri::command]
pub async fn export_search(
    state: State<'_, AppState>,
    query: String,
    opts: SearchOptions,
    format: ExportFormat,
    dest: PathBuf,
) -> CmdResult<ExportReport> {
    let lib = lib(&state);
    blocking(move || {
        const CHUNK: usize = 2000;
        let mut opts = SearchOptions { offset: 0, limit: CHUNK, facets: false, ..opts };
        let mut items = Vec::new();
        loop {
            let res = lib.search(&query, &opts)?;
            let n = res.hits.len();
            items.extend(res.hits.into_iter().filter_map(|h| h.header.locator.map(|l| (h.source, l))));
            opts.offset += n;
            if n == 0 || opts.offset as u64 >= res.total {
                break;
            }
        }
        Ok(lib.export(&items, format, &dest)?)
    })
    .await
}
