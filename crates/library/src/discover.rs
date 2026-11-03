//! Turning a user-supplied path into one or more mail sources.

use std::fs;
use std::path::{Path, PathBuf};

use mail_model::{Error, MailSource, Result, SourceKind, SourceOpener};

/// Openers in sniff order for single files: PST/OST magic is unambiguous, then the
/// OLE/TNEF formats, and MIME/mbox (which relies partly on extensions) last. `mbox` is
/// the mbox/eml opener to use (a library passes one whose header cache lives in its
/// data directory).
pub(crate) fn openers(mbox: &dyn SourceOpener) -> [&dyn SourceOpener; 3] {
    [stv_pst::opener(), stv_msg::opener(), mbox]
}

/// Detect the source kind of a single file or directory without parsing it.
pub fn sniff(path: &Path) -> Option<SourceKind> {
    openers(stv_mbox::opener()).into_iter().find_map(|o| o.sniff(path))
}

/// Open `path` as a single source with the first opener that recognises it (outside a
/// library: mbox header caches go to the per-user cache directory).
pub fn open_source(path: &Path) -> Result<Box<dyn MailSource>> {
    for o in openers(stv_mbox::opener()) {
        if o.sniff(path).is_some() {
            return o.open(path);
        }
    }
    if !path.exists() {
        return Err(Error::NotFound(path.display().to_string()));
    }
    Err(Error::Unsupported(format!("{} is not a recognised mail archive", path.display())))
}

/// Re-open a registered source, using the opener that claims `kind` for `path`.
pub(crate) fn open_kind(path: &Path, kind: SourceKind, mbox: &dyn SourceOpener) -> Result<Box<dyn MailSource>> {
    if !path.exists() {
        return Err(Error::NotFound(format!("source missing: {}", path.display())));
    }
    for o in openers(mbox) {
        if o.sniff(path) == Some(kind) {
            return o.open(path);
        }
    }
    Err(Error::Format(format!("{} is no longer recognised as {:?}", path.display(), kind)))
}

const MAX_DEPTH: usize = 24;

#[derive(Clone, Copy, Default)]
struct Claimed {
    /// An ancestor directory is already one mbox/eml/emlx tree source.
    mime_tree: bool,
    /// An ancestor directory is already one `.msg` directory source.
    msg_tree: bool,
}

/// All distinct sources under `path`. A file yields at most one source. A directory
/// may be claimed as a whole by the MIME-tree opener (Thunderbird profile, eml folder,
/// Apple Mail bundle) and/or the msg opener (folder of .msg files); PST/OST files are
/// always separate sources, wherever they live.
pub(crate) fn discover(path: &Path) -> Result<Vec<(PathBuf, SourceKind)>> {
    let meta = fs::metadata(path)?;
    if meta.is_file() {
        return Ok(sniff(path).map(|k| vec![(path.to_path_buf(), k)]).unwrap_or_default());
    }
    let mut out = Vec::new();
    walk(path, Claimed::default(), 0, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, mut claimed: Claimed, depth: usize, out: &mut Vec<(PathBuf, SourceKind)>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let [pst, msg, mime] = openers(stv_mbox::opener());
    if !claimed.mime_tree {
        if let Some(k) = mime.sniff(dir) {
            out.push((dir.to_path_buf(), k));
            claimed.mime_tree = true;
        }
    }
    if !claimed.msg_tree {
        if let Some(k) = msg.sniff(dir) {
            out.push((dir.to_path_buf(), k));
            claimed.msg_tree = true;
        }
    }

    let mut entries: Vec<(PathBuf, bool)> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let ft = e.file_type().ok()?;
                if ft.is_symlink() {
                    return None;
                }
                let name = e.file_name();
                if name.to_string_lossy().starts_with('.') {
                    return None;
                }
                Some((e.path(), ft.is_dir()))
            })
            .collect(),
        // An unreadable subdirectory must not abort the whole scan.
        Err(e) if depth > 0 => {
            tracing::warn!("skipping {}: {e}", dir.display());
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    entries.sort();

    for (p, is_dir) in entries {
        if is_dir {
            walk(&p, claimed, depth + 1, out)?;
            continue;
        }
        let kind = pst.sniff(&p).or_else(|| if claimed.msg_tree { None } else { msg.sniff(&p) }).or_else(|| {
            if claimed.mime_tree {
                None
            } else {
                mime.sniff(&p)
            }
        });
        if let Some(k) = kind {
            out.push((p, k));
        }
    }
    Ok(())
}
