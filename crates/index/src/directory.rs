//! `MmapDirectory` wrapper that retries transient Windows file-replacement failures.
//!
//! tantivy rewrites `.managed.json` / `meta.json` via write-temp-then-rename many times per
//! commit. On Windows the rename fails with `ERROR_ACCESS_DENIED` (or a sharing violation)
//! while another process — typically an antivirus / EDR scanner — briefly holds the
//! just-written target open. The failure is transient, so the atomic write is retried
//! with a short backoff instead of failing the whole commit.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    Directory, DirectoryLock, FileHandle, FileSlice, Lock, MmapDirectory, WatchCallback, WatchHandle, WritePtr,
};

const ATTEMPTS: u32 = 60;

#[derive(Clone, Debug)]
pub(crate) struct RetryingDirectory(pub MmapDirectory);

fn transient(e: &io::Error) -> bool {
    // 5 = ERROR_ACCESS_DENIED, 32 = ERROR_SHARING_VIOLATION, 33 = ERROR_LOCK_VIOLATION.
    e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(32 | 33))
}

/// Runs `op`, retrying transient Windows access/sharing failures with a short backoff
/// (up to ~2.5 s in total).
pub(crate) fn retry_transient<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(e) if transient(&e) && attempt + 1 < ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(u64::from(attempt.min(25)) * 2));
            }
            other => return other,
        }
    }
}

impl Directory for RetryingDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.0.get_file_handle(path)
    }
    fn open_read(&self, path: &Path) -> Result<FileSlice, OpenReadError> {
        self.0.open_read(path)
    }
    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.0.delete(path)
    }
    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.0.exists(path)
    }
    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        self.0.open_write(path)
    }
    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.0.atomic_read(path)
    }
    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        retry_transient(|| self.0.atomic_write(path, data))
    }
    fn sync_directory(&self) -> io::Result<()> {
        self.0.sync_directory()
    }
    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.0.acquire_lock(lock)
    }
    fn watch(&self, cb: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.0.watch(cb)
    }
}
