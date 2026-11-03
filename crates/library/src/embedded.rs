//! Cross-format embedded messages: a TNEF blob or `.msg` file attached to a MIME mail,
//! an `.eml` attached to a PST message, and any nesting of those. The source is asked
//! first; when it answers `Error::Unsupported` for a `Locator::Embedded`, the parent
//! attachment's bytes are fetched (recursively, through this same module) and decoded
//! with the helper of the format the bytes actually are.

use std::ops::Range;

use mail_model::{
    Error, Fingerprint, Folder, FolderId, Header, Locator, MailSource, Message, PropertyView, Result, Sort, SourceKind,
};

/// A source view whose `message`/`attachment`/`raw_mime` also resolve cross-format
/// embedded messages; handed to the indexer so TNEF/.msg/.eml attachments get their
/// bodies indexed whatever container they sit in.
pub(crate) struct Resolving<'a>(pub &'a dyn MailSource);

impl MailSource for Resolving<'_> {
    fn kind(&self) -> SourceKind {
        self.0.kind()
    }
    fn display_name(&self) -> String {
        self.0.display_name()
    }
    fn fingerprint(&self) -> Result<Fingerprint> {
        self.0.fingerprint()
    }
    fn folders(&self) -> Result<Vec<Folder>> {
        self.0.folders()
    }
    fn count(&self, folder: &FolderId) -> Result<u64> {
        self.0.count(folder)
    }
    fn list(&self, folder: &FolderId, range: Range<u64>, sort: Sort) -> Result<Vec<Header>> {
        self.0.list(folder, range, sort)
    }
    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_> {
        self.0.iter_headers()
    }
    fn message(&self, loc: &Locator) -> Result<Message> {
        message(self.0, loc)
    }
    fn attachment(&self, loc: &Locator, index: u32) -> Result<Vec<u8>> {
        attachment(self.0, loc, index)
    }
    fn raw_mime(&self, loc: &Locator) -> Result<Option<Vec<u8>>> {
        raw_mime(self.0, loc)
    }
    fn properties(&self, loc: &Locator) -> Result<Vec<PropertyView>> {
        properties(self.0, loc)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Blob {
    Tnef,
    /// OLE compound file (Outlook `.msg`).
    Msg,
    Mime,
}

const OLE_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
/// TNEF signature 0x223E9F78, little-endian. [MS-OXTNEF] 2.1.3.1
const TNEF_MAGIC: [u8; 4] = [0x78, 0x9F, 0x3E, 0x22];

/// Classify attachment bytes as a message container, or `None` if they are not a message.
pub(crate) fn classify(bytes: &[u8]) -> Option<Blob> {
    if bytes.starts_with(&TNEF_MAGIC) {
        return Some(Blob::Tnef);
    }
    if bytes.starts_with(&OLE_MAGIC) {
        return Some(Blob::Msg);
    }
    looks_like_mime(bytes).then_some(Blob::Mime)
}

/// First non-blank line is an RFC 5322 header field (`Name: value`) or an mbox `From ` line.
fn looks_like_mime(bytes: &[u8]) -> bool {
    let start = bytes.iter().position(|b| !b" \t\r\n".contains(b)).unwrap_or(bytes.len());
    let rest = &bytes[start..];
    let rest = rest.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(rest);
    if rest.starts_with(b"From ") {
        return true;
    }
    let name_len = rest.iter().take_while(|&&b| (b'!'..=b'~').contains(&b) && b != b':').count();
    name_len > 0 && rest.get(name_len) == Some(&b':')
}

pub(crate) fn message(src: &dyn MailSource, loc: &Locator) -> Result<Message> {
    match src.message(loc) {
        Err(Error::Unsupported(why)) => match loc {
            Locator::Embedded { parent, attachment: idx } => {
                let bytes = attachment(src, parent, *idx)?;
                decode_message(&bytes, loc, *idx, &why)
            }
            _ => Err(Error::Unsupported(why)),
        },
        r => r,
    }
}

pub(crate) fn attachment(src: &dyn MailSource, loc: &Locator, index: u32) -> Result<Vec<u8>> {
    match src.attachment(loc, index) {
        Err(Error::Unsupported(why)) => match loc {
            Locator::Embedded { parent, attachment: idx } => {
                let container = attachment(src, parent, *idx)?;
                match classify(&container) {
                    Some(Blob::Tnef) => stv_msg::attachment_from_tnef_bytes(&container, index),
                    Some(Blob::Msg) => stv_msg::attachment_from_msg_bytes(&container, index),
                    Some(Blob::Mime) => stv_mbox::attachment_from_mime(&container, index),
                    None => Err(Error::Unsupported(why)),
                }
            }
            _ => Err(Error::Unsupported(why)),
        },
        r => r,
    }
}

/// Native RFC 5322 bytes for `loc`, if any exist. Foreign MIME attachments (an `.eml`
/// inside a PST message) count as native: their bytes are the message.
pub(crate) fn raw_mime(src: &dyn MailSource, loc: &Locator) -> Result<Option<Vec<u8>>> {
    match src.raw_mime(loc) {
        Err(Error::Unsupported(why)) => match loc {
            Locator::Embedded { parent, attachment: idx } => {
                let bytes = attachment(src, parent, *idx)?;
                match classify(&bytes) {
                    Some(Blob::Mime) => Ok(Some(bytes)),
                    Some(_) => Ok(None),
                    None => Err(Error::Unsupported(why)),
                }
            }
            _ => Ok(None),
        },
        r => r,
    }
}

/// Inspector properties for `loc`, decoding cross-format embedded messages like [`message`].
pub(crate) fn properties(src: &dyn MailSource, loc: &Locator) -> Result<Vec<PropertyView>> {
    match src.properties(loc) {
        Err(Error::Unsupported(why)) => match loc {
            Locator::Embedded { parent, attachment: idx } => {
                let bytes = attachment(src, parent, *idx)?;
                match classify(&bytes) {
                    Some(Blob::Tnef) => stv_msg::properties_from_tnef_bytes(&bytes),
                    Some(Blob::Msg) => stv_msg::properties_from_msg_bytes(&bytes),
                    Some(Blob::Mime) => stv_mbox::properties_from_mime(&bytes),
                    None => Err(Error::Unsupported(format!("attachment {idx} is not a message ({why})"))),
                }
            }
            _ => Err(Error::Unsupported(why)),
        },
        r => r,
    }
}

fn decode_message(bytes: &[u8], loc: &Locator, index: u32, why: &str) -> Result<Message> {
    let mut m = match classify(bytes) {
        Some(Blob::Tnef) => stv_msg::message_from_tnef_bytes(bytes, Some(loc.clone()))?,
        Some(Blob::Msg) => stv_msg::message_from_msg_bytes(bytes, Some(loc.clone()))?,
        Some(Blob::Mime) => stv_mbox::message_from_mime(bytes, Some(loc.clone()), None)?,
        None => {
            return Err(Error::Unsupported(format!("attachment {index} is not a message ({why})")));
        }
    };
    m.header.locator = Some(loc.clone());
    // .msg and TNEF carry no message size; the container's byte count is the honest size.
    if m.header.size == 0 {
        m.header.size = bytes.len() as u64;
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_containers() {
        assert_eq!(classify(&[0x78, 0x9F, 0x3E, 0x22, 1, 0]), Some(Blob::Tnef));
        assert_eq!(classify(&OLE_MAGIC), Some(Blob::Msg));
        assert_eq!(classify(b"Received: from x\r\nSubject: y\r\n\r\nbody"), Some(Blob::Mime));
        assert_eq!(classify(b"\r\n\xEF\xBB\xBFFrom: a@b\r\n"), Some(Blob::Mime));
        assert_eq!(classify(b"From a@b Thu Jan  1 00:00:00 1970\nSubject: x\n\n"), Some(Blob::Mime));
        assert_eq!(classify(b"%PDF-1.7\n"), None);
        assert_eq!(classify(b"Hello world: not a header"), None);
        assert_eq!(classify(b""), None);
    }
}
