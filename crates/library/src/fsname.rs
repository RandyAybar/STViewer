//! Windows-safe file names for exports.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// Longest file name (in chars) we produce; leaves room for the destination
/// directory under the classic 260-char `MAX_PATH`.
pub const MAX_NAME_CHARS: usize = 120;

const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn is_reserved(stem: &str) -> bool {
    let s = stem.trim_end_matches([' ', '.']).to_ascii_uppercase();
    if RESERVED.contains(&s.as_str()) {
        return true;
    }
    // COM0-9, LPT0-9 plus the superscript-digit variants Windows also reserves.
    let (Some(prefix), Some(tail)) = (s.get(..3), s.get(3..)) else {
        return false;
    };
    (prefix == "COM" || prefix == "LPT")
        && tail.chars().count() == 1
        && tail.chars().all(|c| c.is_ascii_digit() || matches!(c, '¹' | '²' | '³'))
}

/// Make `name` a valid, portable Windows file name: invalid characters
/// (`<>:"/\|?*`, control chars) and bidirectional-text controls (which can disguise an
/// extension, as in `invoice\u{202E}fdp.exe`) become `_`, trailing dots/spaces are
/// dropped, reserved device names (`CON`, `COM1.txt`, `CONIN$`, ...) get a `_` prefix,
/// and long names are truncated to [`MAX_NAME_CHARS`] keeping the extension. The result
/// never contains a path separator and is never empty, `.` or `..`.
pub fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = s.trim().trim_end_matches(['.', ' ']);
    if trimmed.len() != s.len() {
        s = trimmed.to_owned();
    }

    // Truncate, keeping a short extension.
    if s.chars().count() > MAX_NAME_CHARS {
        let (stem, ext) = split_ext(&s);
        let ext = if ext.chars().count() <= 16 { ext } else { "" };
        let keep = MAX_NAME_CHARS - ext.chars().count() - usize::from(!ext.is_empty());
        let mut out: String = stem.chars().take(keep).collect();
        out.truncate(out.trim_end_matches(['.', ' ']).len());
        if !ext.is_empty() {
            out.push('.');
            out.push_str(ext);
        }
        s = out;
    }

    if s.is_empty() || s.chars().all(|c| c == '.') {
        return "_".to_owned();
    }
    let stem = s.split('.').next().unwrap_or("");
    if is_reserved(stem) {
        s.insert(0, '_');
    }
    s
}

/// `("report.final", "pdf")` for `report.final.pdf`; `("name", "")` without extension.
fn split_ext(s: &str) -> (&str, &str) {
    match s.rfind('.') {
        Some(p) if p > 0 => (&s[..p], &s[p + 1..]),
        _ => (s, ""),
    }
}

/// Creates unique files inside one directory: `a.txt`, `a (2).txt`, ... Names taken by
/// this allocator are compared case-insensitively; files already on disk are never
/// overwritten (each file is created with `create_new`).
pub(crate) struct NameAllocator {
    dir: PathBuf,
    used: HashSet<String>,
}

impl NameAllocator {
    pub fn new(dir: &Path) -> Self {
        NameAllocator { dir: dir.to_path_buf(), used: HashSet::new() }
    }

    /// Creates a new file named after `wanted` (sanitised, collision-suffixed).
    pub fn create(&mut self, wanted: &str) -> io::Result<File> {
        let clean = sanitize_filename(wanted);
        let (stem, ext) = split_ext(&clean);
        for n in 1u32.. {
            let candidate = match n {
                1 => clean.clone(),
                _ if ext.is_empty() => format!("{stem} ({n})"),
                _ => format!("{stem} ({n}).{ext}"),
            };
            if !self.used.insert(candidate.to_lowercase()) {
                continue;
            }
            match OpenOptions::new().write(true).create_new(true).open(self.dir.join(&candidate)) {
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                r => return r,
            }
        }
        Err(io::Error::other(format!("no free file name for {clean:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn hostile_names_stay_inside_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("out");
        std::fs::create_dir(&dir).unwrap();
        let mut names = NameAllocator::new(&dir);
        for n in ["..", "../x.txt", "..\\y.txt", "/etc/passwd", "C:\\boot.ini", "CONIN$", "aux.log", "nul .txt"] {
            names.create(n).unwrap();
        }
        assert_eq!(
            names_in(&dir),
            [".._x.txt", ".._y.txt", "C__boot.ini", "_", "_CONIN$", "_aux.log", "_etc_passwd", "_nul .txt"]
        );
        assert_eq!(names_in(tmp.path()), ["out"]);
    }

    #[test]
    fn collisions_get_suffixes_and_existing_files_survive() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "keep").unwrap();
        let mut names = NameAllocator::new(tmp.path());
        for n in ["a.txt", "A.TXT", "b"] {
            names.create(n).unwrap();
        }
        names.create("b").unwrap();
        assert_eq!(names_in(tmp.path()), ["A (3).TXT", "a (2).txt", "a.txt", "b", "b (2)"]);
        assert_eq!(std::fs::read_to_string(tmp.path().join("a.txt")).unwrap(), "keep");
    }
}
