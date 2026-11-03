//! Persistent source registry (`sources.json` in the data directory).

use std::path::{Path, PathBuf};

use mail_model::{Error, Result, SourceId, SourceKind};
use serde::{Deserialize, Serialize};

use crate::jsonfile;

pub(crate) const FILE_NAME: &str = "sources.json";
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub id: SourceId,
    pub kind: SourceKind,
    pub path: PathBuf,
    pub display_name: String,
    /// Files of a multi-part archive, in part order; empty for single-file sources.
    /// `path` is then the first part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<PathBuf>,
}

impl Entry {
    /// The archive files of this source: `parts`, or just `path`.
    pub fn files(&self) -> &[PathBuf] {
        if self.parts.is_empty() {
            std::slice::from_ref(&self.path)
        } else {
            &self.parts
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Registry {
    pub version: u32,
    /// Next id to hand out. Ids are never reused, so a stale index document can
    /// never be attributed to a different source.
    pub next_id: u32,
    pub sources: Vec<Entry>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry { version: VERSION, next_id: 1, sources: Vec::new() }
    }
}

impl Registry {
    /// The registry stored at `path`; empty if the file does not exist yet.
    pub fn load(path: &Path) -> Result<Registry> {
        let Some(mut reg) = jsonfile::load::<Registry>(path)? else { return Ok(Registry::default()) };
        if reg.version > VERSION {
            return Err(Error::Unsupported(format!(
                "{} was written by a newer STViewer (registry version {})",
                path.display(),
                reg.version
            )));
        }
        // Self-heal a hand-edited file: next_id must stay above every id in use.
        let max = reg.sources.iter().map(|e| e.id.0).max().unwrap_or(0);
        reg.next_id = reg.next_id.max(max + 1);
        reg.version = VERSION;
        Ok(reg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        jsonfile::save(path, self)
    }

    pub fn get(&self, id: SourceId) -> Option<&Entry> {
        self.sources.iter().find(|e| e.id == id)
    }

    pub fn find_path(&self, path: &Path) -> Option<&Entry> {
        self.sources.iter().find(|e| same_path(&e.path, path))
    }

    pub fn insert(&mut self, kind: SourceKind, path: PathBuf, parts: Vec<PathBuf>, display_name: String) -> Entry {
        let entry = Entry { id: SourceId(self.next_id), kind, path, display_name, parts };
        self.next_id += 1;
        self.sources.push(entry.clone());
        entry
    }

    pub fn remove(&mut self, id: SourceId) -> Option<Entry> {
        let pos = self.sources.iter().position(|e| e.id == id)?;
        Some(self.sources.remove(pos))
    }
}

/// Path equality as the OS sees it (case-insensitive on Windows).
pub(crate) fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}
