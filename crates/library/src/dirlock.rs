//! One process per library: an exclusive OS lock on `<data_dir>/library.lock`.
//!
//! The registry and tag stores are read once and rewritten from memory, and the index
//! allows one writer, so two processes sharing a data directory would overwrite each
//! other's changes. The lock is released by the OS when the holder exits, even on a crash.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::Path;

use mail_model::{Error, Result};

const LOCK_FILE: &str = "library.lock";
/// Holder's process id, for the "in use" message. A separate file, because Windows
/// does not let other processes read a locked file.
const PID_FILE: &str = "library.pid";

/// Holds the data directory lock until dropped (closing the file releases it).
pub(crate) struct DirLock {
    _file: File,
}

impl DirLock {
    /// Takes the lock without waiting; [`Error::InUse`] if another handle (in this or
    /// another process) holds it.
    pub(crate) fn acquire(data_dir: &Path) -> Result<DirLock> {
        let file = OpenOptions::new().write(true).create(true).truncate(false).open(data_dir.join(LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let pid = fs::read_to_string(data_dir.join(PID_FILE)).ok().and_then(|s| s.trim().parse().ok());
                return Err(Error::InUse { pid });
            }
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        // Informational only: a failure to record the pid must not fail the open.
        let _ = fs::write(data_dir.join(PID_FILE), std::process::id().to_string());
        Ok(DirLock { _file: file })
    }
}
