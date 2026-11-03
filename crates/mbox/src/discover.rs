//! Source layout discovery: which files/directories hold messages and how they map to folders.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use mail_model::{Error, Result, SourceKind};

use crate::scan;
use crate::store::{FileEntry, UnitKind};
use crate::util::{ext_lower, mtime_ms, read_at, rel_join};

const MAX_DEPTH: u32 = 40;
const SNIFF_BYTES: usize = 4096;

/// Extensions that never hold an mbox (skips reading them while walking).
const NOT_MBOX_EXT: &[&str] = &[
    "msf",
    "dat",
    "json",
    "html",
    "htm",
    "js",
    "sqlite",
    "sqlite-wal",
    "sqlite-shm",
    "txt",
    "log",
    "ini",
    "lz4",
    "mozlz4",
    "jsonlz4",
    "baklz4",
    "xml",
    "plist",
    "db",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "ico",
    "css",
    "pst",
    "ost",
    "msg",
    "tnef",
    "lock",
    "bak",
    "rdf",
    "mab",
    "toc",
    "emlxpart",
    "emlx",
    "eml",
    "pdf",
    "zip",
    "exe",
    "dll",
    "sig",
    "list",
    "rss",
    "opml",
    "xpi",
    "cer",
    "der",
];

/// Directories inside mail profiles that never contain mailboxes.
const SKIP_DIRS: &[&str] = &[
    "cache2",
    "startupcache",
    "storage",
    "crashes",
    "minidumps",
    "datareporting",
    "extensions",
    "saved-telemetry-pings",
    "security_state",
    "settings",
    "maildata",
    "attachments",
    ".git",
    "thumbnails",
    "shader-cache",
    "jumplistcache",
];

#[derive(Clone, Debug)]
pub(crate) struct UnitSpec {
    pub kind: UnitKind,
    /// mbox: file path relative to the source root ("" for a single-file source).
    /// Files: unit directory relative to the root.
    pub rel: String,
    pub abs: PathBuf,
    /// Folder showing this unit.
    pub folder: String,
    /// eml/emlx files relative to `abs`, sorted by `rel`.
    pub files: Vec<FileEntry>,
    /// mbox (size, mtime) at discovery.
    pub stamp: (u64, i64),
}

#[derive(Clone, Debug)]
pub(crate) struct FolderSpec {
    pub id: String,
    pub parent: Option<String>,
    pub name: String,
    pub unit: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct Layout {
    pub kind: SourceKind,
    pub folders: Vec<FolderSpec>,
    pub units: Vec<UnitSpec>,
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

pub(crate) fn sniff_file(path: &Path) -> Option<SourceKind> {
    match ext_lower(path).as_str() {
        "eml" => return Some(SourceKind::Eml),
        "emlx" => return Some(SourceKind::AppleMail),
        e if NOT_MBOX_EXT.contains(&e) => return None,
        _ => {}
    }
    let f = File::open(path).ok()?;
    let head = read_at(&f, 0, SNIFF_BYTES).ok()?;
    scan::looks_like_mbox(&head).then_some(SourceKind::Mbox)
}

fn is_mbox_file(path: &Path, size: u64, siblings: &HashSet<String>, name: &str) -> bool {
    let ext = ext_lower(path);
    if NOT_MBOX_EXT.contains(&ext.as_str()) {
        return false;
    }
    if size == 0 {
        return siblings.contains(&format!("{name}.msf").to_lowercase())
            || siblings.contains(&format!("{name}.sbd").to_lowercase());
    }
    File::open(path).and_then(|f| read_at(&f, 0, SNIFF_BYTES)).is_ok_and(|h| scan::looks_like_mbox(&h))
}

/// Cheap bounded check of a directory: Apple Mail > Thunderbird/mbox > eml.
pub(crate) fn sniff_dir(path: &Path) -> Option<SourceKind> {
    let mut budget = 5000usize;
    let mut reads = 64usize;
    let mut best: Option<SourceKind> = None;
    let rank = |k: SourceKind| match k {
        SourceKind::AppleMail => 3,
        SourceKind::Thunderbird => 2,
        _ => 1,
    };
    let mut queue = vec![(path.to_path_buf(), 0u32)];
    while let Some((dir, depth)) = queue.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            if budget == 0 {
                return best;
            }
            budget -= 1;
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            let Ok(ft) = e.file_type() else { continue };
            let found = if ft.is_dir() {
                if is_bundle_dir(&name) {
                    Some(SourceKind::AppleMail)
                } else if name.ends_with(".sbd") {
                    Some(SourceKind::Thunderbird)
                } else {
                    if depth < 6 && !SKIP_DIRS.contains(&name.as_str()) {
                        queue.push((p, depth + 1));
                    }
                    None
                }
            } else {
                match ext_lower(&p).as_str() {
                    "emlx" => Some(SourceKind::AppleMail),
                    "msf" => Some(SourceKind::Thunderbird),
                    "eml" => Some(SourceKind::Eml),
                    e if NOT_MBOX_EXT.contains(&e) => None,
                    _ if reads > 0 => {
                        reads -= 1;
                        sniff_file(&p).map(|_| SourceKind::Thunderbird)
                    }
                    _ => None,
                }
            };
            if let Some(k) = found {
                if best.is_none_or(|b| rank(k) > rank(b)) {
                    best = Some(k);
                }
                if k == SourceKind::AppleMail {
                    return best;
                }
            }
        }
    }
    best
}

pub(crate) fn single_file_layout(path: &Path, kind: SourceKind) -> Result<Layout> {
    let meta = std::fs::metadata(path)?;
    let name = file_name(path);
    let (unit_kind, files, abs) = match kind {
        SourceKind::Mbox => (UnitKind::Mbox, Vec::new(), path.to_path_buf()),
        SourceKind::Eml | SourceKind::AppleMail => (
            if kind == SourceKind::Eml { UnitKind::Eml } else { UnitKind::Emlx },
            vec![FileEntry { rel: name.clone(), size: meta.len(), mtime: mtime_ms(&meta) }],
            path.parent().map(Path::to_path_buf).unwrap_or_default(),
        ),
        other => return Err(Error::Unsupported(format!("{other:?} is not handled by stv-mbox"))),
    };
    Ok(Layout {
        kind,
        folders: vec![FolderSpec { id: String::new(), parent: None, name, unit: Some(0) }],
        units: vec![UnitSpec {
            kind: unit_kind,
            rel: String::new(),
            abs,
            folder: String::new(),
            files,
            stamp: (meta.len(), mtime_ms(&meta)),
        }],
    })
}

struct Walker {
    folders: Vec<FolderSpec>,
    units: Vec<UnitSpec>,
    apple: bool,
    /// Canonical paths of listed directories: symlink/junction cycles and aliases are walked once.
    visited: HashSet<PathBuf>,
}

/// Apple Mail mailbox bundle directory (`Name.mbox` / `Name.imapmbox`), given its lowercase name.
fn is_bundle_dir(lower: &str) -> bool {
    lower.ends_with(".mbox") || lower.ends_with(".imapmbox")
}

/// Folder name of a bundle directory (extension dropped).
fn bundle_name(name: &str) -> String {
    name.rsplit_once('.').map_or(name, |(n, _)| n).to_string()
}

/// `name` without an ASCII `suffix`, compared case-insensitively.
fn strip_suffix_ci<'a>(name: &'a str, suffix: &str) -> Option<&'a str> {
    let cut = name.len().checked_sub(suffix.len())?;
    name.get(cut..).is_some_and(|s| s.eq_ignore_ascii_case(suffix)).then(|| &name[..cut])
}

struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    mtime: i64,
}

fn list_dir(dir: &Path) -> Vec<Entry> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<Entry> = rd
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let meta = std::fs::metadata(&path).ok()?;
            Some(Entry {
                name: e.file_name().to_string_lossy().into_owned(),
                path,
                is_dir: meta.is_dir(),
                size: meta.len(),
                mtime: mtime_ms(&meta),
            })
        })
        .collect();
    fn folded(s: &str) -> impl Iterator<Item = char> + '_ {
        s.chars().flat_map(char::to_lowercase)
    }
    v.sort_by(|a, b| folded(&a.name).cmp(folded(&b.name)).then_with(|| a.name.cmp(&b.name)));
    v
}

impl Walker {
    fn add_folder(&mut self, id: String, parent: &str, name: String) {
        self.folders.push(FolderSpec { id, parent: Some(parent.to_string()), name, unit: None });
    }

    fn attach_unit(&mut self, folder: &str, spec: UnitSpec) {
        let idx = self.units.len();
        self.units.push(spec);
        if let Some(f) = self.folders.iter_mut().find(|f| f.id == folder) {
            f.unit = Some(idx);
        }
    }

    /// Lists `dir` unless it is too deep or was already walked (via another path or link).
    fn enter(&mut self, dir: &Path, depth: u32) -> Option<Vec<Entry>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.visited.insert(key).then(|| list_dir(dir))
    }

    fn walk(&mut self, abs: &Path, rel: &str, folder: &str, depth: u32) {
        let Some(entries) = self.enter(abs, depth) else { return };
        let names: HashSet<String> = entries.iter().map(|e| e.name.to_lowercase()).collect();
        let mut emls = Vec::new();
        for e in entries.iter().filter(|e| !e.is_dir) {
            if ext_lower(&e.path) == "eml" {
                emls.push(FileEntry { rel: e.name.clone(), size: e.size, mtime: e.mtime });
            } else if is_mbox_file(&e.path, e.size, &names, &e.name) {
                let id = rel_join(rel, &e.name);
                self.add_folder(id.clone(), folder, e.name.clone());
                self.attach_unit(
                    &id,
                    UnitSpec {
                        kind: UnitKind::Mbox,
                        rel: id.clone(),
                        abs: e.path.clone(),
                        folder: id.clone(),
                        files: Vec::new(),
                        stamp: (e.size, e.mtime),
                    },
                );
            }
        }
        if !emls.is_empty() {
            emls.sort_by(|a, b| a.rel.cmp(&b.rel));
            self.attach_unit(
                folder,
                UnitSpec {
                    kind: UnitKind::Eml,
                    rel: rel.to_string(),
                    abs: abs.to_path_buf(),
                    folder: folder.to_string(),
                    files: emls,
                    stamp: (0, 0),
                },
            );
        }
        for e in entries.iter().filter(|e| e.is_dir) {
            let lower = e.name.to_lowercase();
            let child_rel = rel_join(rel, &e.name);
            if let Some(base) = strip_suffix_ci(&e.name, ".sbd") {
                let fid = rel_join(rel, base);
                if !self.folders.iter().any(|f| f.id == fid) {
                    self.add_folder(fid.clone(), folder, base.to_string());
                }
                self.walk(&e.path, &child_rel, &fid, depth + 1);
            } else if is_bundle_dir(&lower) {
                self.apple = true;
                self.add_folder(child_rel.clone(), folder, bundle_name(&e.name));
                self.walk_bundle(&e.path, &child_rel, depth + 1);
            } else if !SKIP_DIRS.contains(&lower.as_str()) {
                self.add_folder(child_rel.clone(), folder, e.name.clone());
                self.walk(&e.path, &child_rel, &child_rel, depth + 1);
            }
        }
    }

    /// Apple Mail bundle: an exported `mbox` file, or `.emlx` files anywhere below
    /// (except inside nested `.mbox` bundles, which become child folders).
    fn walk_bundle(&mut self, abs: &Path, fid: &str, depth: u32) {
        let Some(entries) = self.enter(abs, depth) else { return };
        let Some(m) = entries.iter().find(|e| !e.is_dir && e.name == "mbox") else {
            let mut files = Vec::new();
            let mut nested = Vec::new();
            self.collect_emlx(entries, "", &mut files, &mut nested, depth);
            if !files.is_empty() {
                files.sort_by(|a, b| a.rel.cmp(&b.rel));
                self.attach_unit(
                    fid,
                    UnitSpec {
                        kind: UnitKind::Emlx,
                        rel: fid.to_string(),
                        abs: abs.to_path_buf(),
                        folder: fid.to_string(),
                        files,
                        stamp: (0, 0),
                    },
                );
            }
            for (sub_rel, sub_abs, name) in nested {
                let child = rel_join(fid, &sub_rel);
                self.add_folder(child.clone(), fid, name);
                self.walk_bundle(&sub_abs, &child, depth + 1);
            }
            return;
        };
        self.attach_unit(
            fid,
            UnitSpec {
                kind: UnitKind::Mbox,
                rel: rel_join(fid, &m.name),
                abs: m.path.clone(),
                folder: fid.to_string(),
                files: Vec::new(),
                stamp: (m.size, m.mtime),
            },
        );
        for e in entries.iter().filter(|e| e.is_dir && is_bundle_dir(&e.name.to_lowercase())) {
            let child = rel_join(fid, &e.name);
            self.add_folder(child.clone(), fid, bundle_name(&e.name));
            self.walk_bundle(&e.path, &child, depth + 1);
        }
    }

    /// `.emlx` files below a bundle directory (already listed as `entries`), skipping
    /// `Attachments`; nested bundles are returned in `nested` as `(rel, abs, name)`.
    fn collect_emlx(
        &mut self,
        entries: Vec<Entry>,
        rel: &str,
        files: &mut Vec<FileEntry>,
        nested: &mut Vec<(String, PathBuf, String)>,
        depth: u32,
    ) {
        for e in entries {
            let r = rel_join(rel, &e.name);
            let lower = e.name.to_lowercase();
            if e.is_dir {
                if is_bundle_dir(&lower) {
                    nested.push((r, e.path, bundle_name(&e.name)));
                } else if lower != "attachments" {
                    if let Some(sub) = self.enter(&e.path, depth + 1) {
                        self.collect_emlx(sub, &r, files, nested, depth + 1);
                    }
                }
            } else if lower.ends_with(".emlx") {
                files.push(FileEntry { rel: r, size: e.size, mtime: e.mtime });
            }
        }
    }

    /// Drops container folders with no message-bearing descendants and orders the
    /// rest depth-first (each folder directly followed by its subtree).
    fn prune_and_order(&mut self) {
        let parent_of: HashMap<&str, Option<&str>> =
            self.folders.iter().map(|f| (f.id.as_str(), f.parent.as_deref())).collect();
        let mut keep: HashSet<String> = HashSet::new();
        keep.insert(String::new());
        for f in self.folders.iter().filter(|f| f.unit.is_some()) {
            let mut cur = Some(f.id.as_str());
            while let Some(id) = cur {
                if !keep.insert(id.to_string()) {
                    break;
                }
                cur = parent_of.get(id).copied().flatten();
            }
        }
        let folders = std::mem::take(&mut self.folders);
        let mut children: HashMap<Option<String>, Vec<FolderSpec>> = HashMap::new();
        for f in folders.into_iter().filter(|f| keep.contains(&f.id)) {
            children.entry(f.parent.clone()).or_default().push(f);
        }
        let mut stack: Vec<FolderSpec> = children.remove(&None).unwrap_or_default();
        stack.reverse();
        while let Some(f) = stack.pop() {
            if let Some(mut kids) = children.remove(&Some(f.id.clone())) {
                kids.reverse();
                stack.extend(kids);
            }
            self.folders.push(f);
        }
    }
}

pub(crate) fn dir_layout(root: &Path) -> Result<Layout> {
    let mut w = Walker {
        folders: vec![FolderSpec { id: String::new(), parent: None, name: file_name(root), unit: None }],
        units: Vec::new(),
        apple: false,
        visited: HashSet::new(),
    };
    w.walk(root, "", "", 0);
    if w.units.is_empty() {
        return Err(Error::Format(format!("no mailboxes found in {}", root.display())));
    }
    w.prune_and_order();
    // Units are never removed and folder ids are stable, so unit ↔ folder links survive.
    let kind = if w.apple || w.units.iter().any(|u| u.kind == UnitKind::Emlx) {
        SourceKind::AppleMail
    } else if w.units.iter().any(|u| u.kind == UnitKind::Mbox) {
        SourceKind::Thunderbird
    } else {
        SourceKind::Eml
    };
    Ok(Layout { kind, folders: w.folders, units: w.units })
}
