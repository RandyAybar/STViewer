//! Agent access (MCP): lets the user's coding agent read shared mailboxes through the
//! app. Off until the user turns it on in Settings; the choice, port, token and shared
//! mailboxes persist in `agent-access.json` in the data directory, and every agent tool
//! call is appended to `agent-access.log` and emitted as an `agent-access` event.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use mail_model::SourceId;
use serde::{Deserialize, Serialize};
use stv_library::Library;
use stv_mcp::{AccessLog, AccessLogEntry, ServerHandle, Shared};
use tauri::{AppHandle, Emitter};

const SETTINGS_FILE: &str = "agent-access.json";
const LOG_FILE: &str = "agent-access.log";
pub const DEFAULT_PORT: u16 = 47731;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Settings {
    enabled: bool,
    port: u16,
    token: String,
    allowed_sources: Vec<SourceId>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: false, port: DEFAULT_PORT, token: stv_mcp::new_token(), allowed_sources: Vec::new() }
    }
}

/// What the Settings panel shows.
#[derive(Clone, Debug, Serialize)]
pub struct AgentAccessState {
    pub enabled: bool,
    pub running: bool,
    pub port: u16,
    pub token: String,
    pub url: String,
    pub allowed_sources: Vec<SourceId>,
    pub error: Option<String>,
}

pub struct AgentAccess {
    lib: Library,
    app: AppHandle,
    dir: PathBuf,
    settings: Mutex<Settings>,
    shared: Shared,
    server: Mutex<Option<ServerHandle>>,
    error: Mutex<Option<String>>,
    /// Serialises [`AgentAccess::apply`]: two overlapping restarts would otherwise race
    /// for the port and leave a stale error or an orphaned server.
    applying: tauri::async_runtime::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AgentAccess {
    pub fn load(lib: Library, app: AppHandle, dir: &Path) -> Arc<AgentAccess> {
        let path = dir.join(SETTINGS_FILE);
        let mut settings = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice::<Settings>(&b).unwrap_or_else(|e| {
                tracing::warn!("{}: {e}; agent access reset to off", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        };
        // A hand-edited empty or short token would make the server refuse to start (or,
        // if accepted, be guessable): replace it.
        let weak_token = settings.token.len() < stv_mcp::MIN_TOKEN_LEN;
        if weak_token {
            settings.token = stv_mcp::new_token();
        }
        let shared: Shared = Arc::new(RwLock::new(settings.allowed_sources.iter().copied().collect()));
        let me = Arc::new(AgentAccess {
            lib,
            app,
            dir: dir.to_path_buf(),
            settings: Mutex::new(settings),
            shared,
            server: Mutex::new(None),
            error: Mutex::new(None),
            applying: tauri::async_runtime::Mutex::new(()),
        });
        if weak_token {
            let s = lock(&me.settings).clone();
            if let Err(e) = me.save(&s) {
                tracing::warn!("agent access: {e}");
            }
        }
        me
    }

    /// Atomically replaces the settings file. It holds the access token, so on Unix it is
    /// readable by the owner only.
    fn save(&self, s: &Settings) -> Result<(), String> {
        use std::io::Write;
        let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
        let path = self.dir.join(SETTINGS_FILE);
        let tmp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        opts.open(&tmp)
            .and_then(|mut f| f.write_all(&json))
            .and_then(|()| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("could not save {}: {e}", path.display()))
    }

    /// Current state; sources that were removed from the library are dropped from sharing.
    pub fn state(&self) -> AgentAccessState {
        let existing: HashSet<SourceId> = self.lib.sources().into_iter().map(|s| s.id).collect();
        let s = {
            let mut s = lock(&self.settings);
            let before = s.allowed_sources.len();
            s.allowed_sources.retain(|id| existing.contains(id));
            if s.allowed_sources.len() != before {
                *self.shared.write().unwrap_or_else(|p| p.into_inner()) = s.allowed_sources.iter().copied().collect();
                let _ = self.save(&s);
            }
            s.clone()
        };
        AgentAccessState {
            enabled: s.enabled,
            running: lock(&self.server).is_some(),
            port: s.port,
            url: format!("http://127.0.0.1:{}/mcp", s.port),
            token: s.token,
            allowed_sources: s.allowed_sources,
            error: lock(&self.error).clone(),
        }
    }

    /// Stop any running server, then start one if enabled. Errors (port in use) are kept
    /// in the state and returned.
    pub async fn apply(&self) -> Result<AgentAccessState, String> {
        let _applying = self.applying.lock().await;
        // Wait for the old server to release its port before binding again.
        let old = lock(&self.server).take();
        if let Some(old) = old {
            old.shutdown().await;
        }
        let s = lock(&self.settings).clone();
        *lock(&self.error) = None;
        if s.enabled {
            let app = self.app.clone();
            let log = AccessLog::new(self.dir.join(LOG_FILE), move |e: &AccessLogEntry| {
                let _ = app.emit("agent-access", e);
            });
            match stv_mcp::start(self.lib.clone(), self.shared.clone(), log, s.port, s.token.clone()).await {
                Ok(h) => *lock(&self.server) = Some(h),
                Err(e) => {
                    let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                        format!("port {} is already in use — choose another port", s.port)
                    } else {
                        format!("could not start agent access: {e}")
                    };
                    *lock(&self.error) = Some(msg);
                }
            }
        }
        let state = self.state();
        let _ = self.app.emit("agent-access-state", &state);
        match &state.error {
            Some(e) => Err(e.clone()),
            None => Ok(state),
        }
    }

    pub async fn set(&self, enabled: bool, port: u16, allowed: Vec<SourceId>) -> Result<AgentAccessState, String> {
        if port < 1024 {
            return Err("choose a port between 1024 and 65535".into());
        }
        {
            let mut s = lock(&self.settings);
            s.enabled = enabled;
            s.port = port;
            s.allowed_sources = allowed;
            *self.shared.write().unwrap_or_else(|p| p.into_inner()) = s.allowed_sources.iter().copied().collect();
            self.save(&s)?;
        }
        self.apply().await
    }

    pub async fn regenerate_token(&self) -> Result<AgentAccessState, String> {
        {
            let mut s = lock(&self.settings);
            s.token = stv_mcp::new_token();
            self.save(&s)?;
        }
        self.apply().await
    }

    pub fn enabled(&self) -> bool {
        lock(&self.settings).enabled
    }
}
