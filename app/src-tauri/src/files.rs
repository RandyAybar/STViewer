//! File names and locations for attachments written to disk.

use std::path::{Path, PathBuf};

use mail_model::AttachmentMeta;

/// Best-effort removal of attachments opened in a previous session.
pub fn clear_open_dir(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

/// Fresh directory under `root` (the per-user open-attachments dir) for one opened
/// attachment, so same-named attachments never overwrite a file still open elsewhere.
pub fn new_open_dir(root: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    for n in 0..u32::MAX {
        let dir = root.join(n.to_string());
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free directory name"))
}

/// Writes attachment bytes to `path`. On Windows the file also gets the Mark of the Web
/// (`Zone.Identifier`, internet zone) like a browser download, so SmartScreen and Office
/// Protected View treat it as untrusted.
pub fn write_untrusted(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        // Best effort: FAT volumes and some network shares have no alternate data streams.
        let _ = std::fs::write(PathBuf::from(stream), "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    Ok(())
}

/// Extensions the OS runs as code when "opened" (programs, scripts, installers, shortcuts,
/// mountable disk images and their Linux/macOS counterparts), whitespace-separated.
/// Compared case-insensitively.
const RUNS_CODE: &str = "accde action ade adp app appimage applescript application appref-ms appx appxbundle bat chm \
    cmd com command cpl desktop diagcab dll dmg exe fileloc gadget hta img inetloc inf ins iso isp jar jnlp js jse \
    library-ms lnk mde mpkg msc msh msh1 msh2 mshxml msi msix msixbundle msp mst pif pkg ps1 ps1xml ps2 psc1 psd1 \
    psm1 rdp reg run scf scpt scr sct searchconnector-ms settingcontent-ms sh shb sys terminal theme themepack url vb \
    vbe vbs vhd vhdx website workflow ws wsb wsc wsf wsh xbap xll";

/// Whether opening a file with this name would run code rather than display a document.
/// Trailing dots and spaces are ignored, as Windows strips them when creating the file.
pub fn runs_code(name: &str) -> bool {
    let name = name.trim_end_matches(['.', ' ']);
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| RUNS_CODE.split_ascii_whitespace().any(|x| x.eq_ignore_ascii_case(ext)))
}

/// Windows-safe file name for an attachment. Mailers sometimes send full client paths,
/// so only the last path component is kept.
pub fn attachment_file_name(att: &AttachmentMeta) -> String {
    match att.filename.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => stv_library::sanitize_filename(n.rsplit(['/', '\\']).next().unwrap_or(n)),
        None => format!("attachment-{}{}", att.index, extension_for(att.mime_type.as_deref())),
    }
}

fn extension_for(mime: Option<&str>) -> &'static str {
    match mime.map(|m| m.trim().to_ascii_lowercase()).as_deref() {
        Some("message/rfc822") => ".eml",
        Some("application/vnd.ms-outlook") => ".msg",
        Some("application/ms-tnef") => ".dat",
        Some("application/pdf") => ".pdf",
        Some("image/png") => ".png",
        Some("image/jpeg") => ".jpg",
        Some("image/gif") => ".gif",
        Some("text/plain") => ".txt",
        Some("text/html") => ".html",
        _ => ".bin",
    }
}

/// `dir/name`, or `dir/name (n).ext` if that already exists.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("unbounded search finds a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_path_component_and_sanitizes() {
        let att = AttachmentMeta { filename: Some("C:\\Users\\x\\re:port?.pdf".into()), ..Default::default() };
        assert_eq!(attachment_file_name(&att), "re_port_.pdf");
        let att = AttachmentMeta { filename: Some("con.txt".into()), ..Default::default() };
        assert_eq!(attachment_file_name(&att), "_con.txt");
    }

    #[test]
    fn unnamed_attachment_gets_extension_from_mime() {
        let att = AttachmentMeta { index: 3, mime_type: Some("image/png".into()), ..Default::default() };
        assert_eq!(attachment_file_name(&att), "attachment-3.png");
    }

    #[test]
    fn unique_path_appends_counter() {
        let dir = std::env::temp_dir().join(format!("stv-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"1").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        assert_eq!(unique_path(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn programs_and_scripts_are_flagged() {
        for name in [
            "setup.exe",
            "Invoice.pdf.EXE",
            "run.ps1",
            "x.lnk",
            "trailing.exe. .",
            "a.AppImage",
            "s.sh",
            "disk.ISO",
            "x.rdp",
            "y.library-ms",
            "Installer.DMG",
            "x.terminal",
            "y.fileloc",
        ] {
            assert!(runs_code(name), "{name}");
        }
        for name in ["report.pdf", "exe", "photo.jpg", "notes.txt", "data.xlsx", "exe.zip", ""] {
            assert!(!runs_code(name), "{name}");
        }
    }

    #[test]
    fn each_open_gets_its_own_directory() {
        let root = std::env::temp_dir().join(format!("stv-open-{}", std::process::id()));
        let a = new_open_dir(&root).unwrap();
        let b = new_open_dir(&root).unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        clear_open_dir(&root);
        assert!(!root.exists());
    }
}
