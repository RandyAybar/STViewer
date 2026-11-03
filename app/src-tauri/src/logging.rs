//! Diagnostics: a daily log file in `<data dir>/logs` (plus stderr in debug builds), a
//! panic hook that records panics there, and a native error box for fatal startup errors.

use std::path::Path;

use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{fmt, EnvFilter};

/// Daily files `stviewer.YYYY-MM-DD.log`; older ones beyond this count are deleted.
const KEEP_LOG_FILES: usize = 7;

/// Installs the global subscriber writing to `log_dir`. The level comes from `STVIEWER_LOG`,
/// else `RUST_LOG` (`EnvFilter` syntax, e.g. `debug` or `stv_mcp=trace`), default `info`.
pub fn init(log_dir: &Path) -> Result<(), String> {
    let filter = EnvFilter::try_from_env("STVIEWER_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let file = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("stviewer")
        .filename_suffix("log")
        .max_log_files(KEEP_LOG_FILES)
        .build(log_dir)
        .map_err(|e| format!("log directory {}: {e}", log_dir.display()))?;
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(file).with_ansi(false))
        .with(cfg!(debug_assertions).then(|| fmt::layer().with_writer(std::io::stderr).with_ansi(false)))
        .try_init()
        .map_err(|e| e.to_string())
}

/// Logs every panic (with a backtrace) before the default hook runs.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}\n{}", std::backtrace::Backtrace::force_capture());
        default(info);
    }));
}

/// Logs `message` and shows it in a blocking native error box, pointing at the logs.
/// Usable before Tauri starts and from the main thread inside `setup`.
pub fn fatal(message: &str, log_dir: Option<&Path>) {
    tracing::error!("{message}");
    let mut text = message.to_string();
    if let Some(dir) = log_dir {
        text.push_str(&format!("\n\nDetails are logged in {}", dir.display()));
    }
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("STViewer could not start")
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}
