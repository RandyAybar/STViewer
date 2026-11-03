//! STViewer desktop shell: Tauri commands over `stv_library::Library`, an `stv://`
//! protocol that serves inline (cid:) images, attachment bytes and thumbnails to the
//! reading pane, an `index-progress` event pump, and the optional agent-access (MCP)
//! server. One instance runs per data directory; logs go to `<data dir>/logs`.

mod agent;
mod commands;
mod files;
mod logging;
mod preview;
mod protocol;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mail_model::{AttachmentMeta, IndexProgress, Locator, SourceId};
use stv_library::Library;
use tauri::{Emitter, Manager};

/// The system allocator, with a memory budget when this process is a PDF worker.
#[global_allocator]
static ALLOCATOR: preview::Allocator = preview::Allocator;

/// Shared state managed by Tauri.
pub(crate) struct AppState {
    pub lib: Library,
    pub agent: Arc<agent::AgentAccess>,
    /// Per-user directory that attachments are written to before being opened with the
    /// default application; emptied at startup.
    pub open_root: PathBuf,
    /// Attachment lists of recently rendered messages, so each `cid:` image request does
    /// not re-parse the whole message.
    att_cache: Mutex<AttCache>,
}

/// Most-recently-used first: `((source, message), attachment list)`.
type AttCache = VecDeque<((SourceId, Locator), Arc<Vec<AttachmentMeta>>)>;

const ATT_CACHE_CAP: usize = 16;

impl AppState {
    fn new(lib: Library, agent: Arc<agent::AgentAccess>, open_root: PathBuf) -> Self {
        Self { lib, agent, open_root, att_cache: Mutex::new(VecDeque::with_capacity(ATT_CACHE_CAP)) }
    }

    /// Attachment metadata for a message, served from a small MRU cache.
    pub fn attachments(&self, id: SourceId, loc: &Locator) -> mail_model::Result<Arc<Vec<AttachmentMeta>>> {
        {
            let mut cache = self.att_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pos) = cache.iter().position(|((s, l), _)| *s == id && l == loc) {
                let entry = cache.remove(pos).expect("position is in bounds");
                let atts = entry.1.clone();
                cache.push_front(entry);
                return Ok(atts);
            }
        }
        let atts = Arc::new(self.lib.message(id, loc)?.attachments);
        self.remember(id, loc.clone(), atts.clone());
        Ok(atts)
    }

    /// Records the attachment list of a message the UI just loaded.
    pub fn remember(&self, id: SourceId, loc: Locator, atts: Arc<Vec<AttachmentMeta>>) {
        let mut cache = self.att_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = cache.iter().position(|((s, l), _)| *s == id && *l == loc) {
            cache.remove(pos);
        }
        if cache.len() == ATT_CACHE_CAP {
            cache.pop_back();
        }
        cache.push_front(((id, loc), atts));
    }
}

/// Data directory: `STVIEWER_DATA_DIR` if set (portable installs, tests; `true`), else the
/// library's per-user default, shared with the `stv` CLI (`false`).
fn data_dir() -> (PathBuf, bool) {
    match std::env::var_os("STVIEWER_DATA_DIR") {
        Some(d) if !d.is_empty() => (PathBuf::from(d), true),
        _ => (stv_library::default_data_dir(), false),
    }
}

/// App identifier for a custom data directory: the bundle identifier plus a hash of the
/// directory, so single-instance locking (and the WebView and cache directories) are per
/// library and a test instance is never redirected to the user's running app. The suffix
/// starts with a letter to stay a valid D-Bus name element on Linux.
fn instance_identifier(base: &str, dir: &Path) -> String {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    // FNV-1a: stable across builds and platforms, unlike `DefaultHasher`.
    let hash = dir
        .to_string_lossy()
        .to_lowercase()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{base}.d{hash:016x}")
}

/// Opens the library and wires up shared state; runs in Tauri's `setup` once the
/// single-instance check has passed.
fn start(app: &mut tauri::App, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let lib = Library::open(dir).map_err(|e| match e {
        mail_model::Error::InUse { .. } => {
            format!("{e}.\n\nClose it, or set STVIEWER_DATA_DIR to use a different library.")
        }
        e => format!("Could not open the library in {}: {e}", dir.display()),
    })?;
    spawn_progress_pump(app.handle().clone(), lib.clone());
    // Agent access stays off unless the user enabled it in Settings.
    let agent = agent::AgentAccess::load(lib.clone(), app.handle().clone(), dir);
    if agent.enabled() {
        let a = agent.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = a.apply().await {
                tracing::warn!("agent access: {e}");
            }
        });
    }
    // Per-user (unlike a shared /tmp), so other local accounts cannot plant or read files there.
    let cache = app.path().app_cache_dir()?;
    let open_root = cache.join("open");
    files::clear_open_dir(&open_root);
    // Copies left by a Quick Look render that was interrupted.
    files::clear_open_dir(&cache.join("preview"));
    let trim_root = cache.clone();
    std::thread::Builder::new().name("stv-preview-trim".into()).spawn(move || preview::trim_disk_cache(&trim_root))?;
    app.manage(AppState::new(lib, agent, open_root));
    Ok(())
}

/// Emits `index-progress` (payload: `IndexProgress[]`) whenever the library's progress changes.
fn spawn_progress_pump(app: tauri::AppHandle, lib: Library) {
    std::thread::Builder::new()
        .name("stv-progress-pump".into())
        .spawn(move || {
            let mut last: Vec<IndexProgress> = Vec::new();
            loop {
                let now = lib.index_progress();
                if now != last {
                    if app.emit("index-progress", &now).is_err() {
                        return;
                    }
                    last = now;
                }
                let busy = last.iter().any(|p| !p.done);
                std::thread::sleep(Duration::from_millis(if busy { 400 } else { 1500 }));
            }
        })
        .expect("spawn progress thread");
}

/// Runs the app. Startup failures are logged and shown in a native error box, then the
/// process exits with a failure code.
pub fn run() -> ExitCode {
    // PDF worker processes (see `preview`) start no app at all.
    if let Some(code) = preview::worker_main() {
        return code;
    }
    let (dir, custom_dir) = data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        logging::fatal(&format!("Could not create the data directory {}: {e}", dir.display()), None);
        return ExitCode::FAILURE;
    }
    let log_dir = dir.join("logs");
    let logs = match logging::init(&log_dir) {
        Ok(()) => Some(log_dir),
        Err(e) => {
            eprintln!("STViewer: logging disabled: {e}");
            None
        }
    };
    logging::install_panic_hook();
    tracing::info!("STViewer {} starting, data directory {}", env!("CARGO_PKG_VERSION"), dir.display());

    let mut context = tauri::generate_context!();
    if custom_dir {
        let id = instance_identifier(&context.config().identifier, &dir);
        context.config_mut().identifier = id;
    }
    let setup_logs = logs.clone();
    let result = tauri::Builder::default()
        // First, so a second launch exits before doing any work.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tracing::info!("second launch: focusing the running window");
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            // A failed setup would make Tauri panic; report it and exit instead.
            if let Err(e) = start(app, &dir) {
                logging::fatal(&e.to_string(), setup_logs.as_deref());
                tauri_plugin_single_instance::destroy(app);
                std::process::exit(1);
            }
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol("stv", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn_blocking(move || protocol::serve(&app, &request, responder));
        })
        .invoke_handler(tauri::generate_handler![
            commands::agent_access_get,
            commands::agent_access_set,
            commands::agent_access_regenerate_token,
            commands::add_paths,
            commands::remove_source,
            commands::sources,
            commands::folders,
            commands::count,
            commands::list,
            commands::message,
            commands::open_folder,
            commands::open_attachment,
            commands::save_attachment,
            commands::save_attachments,
            commands::save_attachment_list,
            commands::conversation,
            commands::search,
            commands::attachments,
            commands::attachment_text,
            commands::tags,
            commands::create_tag,
            commands::rename_tag,
            commands::delete_tag,
            commands::set_tag,
            commands::tags_of,
            commands::tagged,
            commands::tagged_count,
            commands::index_progress,
            commands::reindex,
            commands::export,
            commands::export_folder,
            commands::export_search,
        ])
        .run(context);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            logging::fatal(&format!("STViewer failed to start: {e}"), logs.as_deref());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_data_dirs_get_distinct_stable_identifiers() {
        let a = instance_identifier("org.stviewer.desktop", Path::new("does-not-exist/a"));
        let b = instance_identifier("org.stviewer.desktop", Path::new("does-not-exist/b"));
        assert_ne!(a, b);
        assert_eq!(a, instance_identifier("org.stviewer.desktop", Path::new("does-not-exist/a")));
        // A valid D-Bus element: starts with a letter, alphanumeric.
        let suffix = a.strip_prefix("org.stviewer.desktop.").unwrap();
        assert!(suffix.starts_with('d') && suffix.chars().all(|c| c.is_ascii_alphanumeric()), "{a}");
    }
}
