//! Crash-safe JSON files for the library's small stores (`sources.json`, `tags.json`).

use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::time::Duration;

use mail_model::{Error, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Parsed contents of `path`, or `None` when the file does not exist.
pub(crate) fn load<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| Error::Format(format!("{}: {e}", path.display())))
}

/// Replaces `path` atomically: writes and flushes a temp file next to it, then renames it
/// over the old one, so a crash leaves the old or the new contents, never a torn file.
/// The temp name carries the process id, so two processes sharing a data directory (the
/// app and `stv`) never write into the same temp file.
pub(crate) fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(|e| Error::Other(e.to_string()))?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = path.with_file_name(tmp_name);
    let written = fs::File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(&json)?;
            f.sync_all()
        })
        .and_then(|()| rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    sync_dir(path);
    Ok(())
}

/// `fs::rename`, retried briefly on Windows while another process (virus scanner, backup
/// or search indexer) holds the target open without delete sharing.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut delay = Duration::from_millis(10);
    for _ in 0..6 {
        match fs::rename(from, to) {
            Err(e) if cfg!(windows) && e.kind() == ErrorKind::PermissionDenied => {
                std::thread::sleep(delay);
                delay *= 2;
            }
            r => return r,
        }
    }
    fs::rename(from, to)
}

/// Makes the rename itself durable (POSIX needs an fsync of the directory; NTFS does not).
#[cfg(unix)]
fn sync_dir(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    }
}

#[cfg(not(unix))]
fn sync_dir(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_replaces_and_leaves_no_temp_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("store.json");
        assert_eq!(load::<Vec<u32>>(&path).unwrap(), None);
        save(&path, &vec![1u32, 2]).unwrap();
        save(&path, &vec![3u32]).unwrap();
        assert_eq!(load::<Vec<u32>>(&path).unwrap(), Some(vec![3]));
        let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["store.json"]);
    }

    #[test]
    fn unparsable_file_is_a_format_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("store.json");
        fs::write(&path, "{ truncated").unwrap();
        assert!(matches!(load::<Vec<u32>>(&path), Err(Error::Format(_))));
    }
}
