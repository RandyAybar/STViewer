//! Resolution of `Locator::Embedded` chains across MSG and TNEF containers.
//!
//! A [`Node`] is "one message somewhere": a storage inside an open `.msg` compound file,
//! or a fully decoded TNEF message. Walking an attachment index yields the embedded
//! message: a `.msg` sub-storage, a nested TNEF stream, or attachment bytes that are
//! themselves a `.msg` / TNEF file.

use std::io::{Cursor, Read, Seek};

use mail_model::{Error, Locator, Result};
use mapi_props::{MapiMessage, PropKey, PropValue};

use crate::msg::{AttachTarget, Load, MsgReader};
use crate::tnef;
use crate::value::{Decoded, CFB_MAGIC, PID_ATTACH_DATA};

pub(crate) trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

pub(crate) enum Node<'a> {
    Msg {
        reader: MsgReader<Box<dyn ReadSeek + 'a>>,
        path: String,
        embedded: bool,
        codepage: Option<u32>,
    },
    /// A decoded TNEF message and the byte length of its stream.
    Mapi {
        mapi: MapiMessage,
        size: u64,
    },
}

impl<'a> Node<'a> {
    /// Open a top-level `.msg` or TNEF container held in `bytes` (sniffed by magic).
    pub fn open(bytes: &'a [u8]) -> Result<Self> {
        if tnef::has_signature(bytes) {
            Self::tnef(bytes)
        } else if bytes.starts_with(&CFB_MAGIC) {
            Self::msg(Box::new(Cursor::new(bytes)))
        } else {
            Err(Error::Format("neither an Outlook .msg (CFB) nor a TNEF stream".into()))
        }
    }

    pub fn open_msg(bytes: &'a [u8]) -> Result<Self> {
        if !bytes.starts_with(&CFB_MAGIC) {
            return Err(Error::Format("not a compound file (missing D0CF11E0 signature)".into()));
        }
        Self::msg(Box::new(Cursor::new(bytes)))
    }

    pub fn open_tnef(bytes: &[u8]) -> Result<Self> {
        Self::tnef(bytes)
    }

    fn tnef(bytes: &[u8]) -> Result<Self> {
        Ok(Node::Mapi { mapi: tnef::parse(bytes)?, size: bytes.len() as u64 })
    }

    fn msg(inner: Box<dyn ReadSeek + 'a>) -> Result<Self> {
        Ok(Node::Msg { reader: MsgReader::open(inner)?, path: "/".into(), embedded: false, codepage: None })
    }

    /// Attachment bytes that hold a whole message (nested TNEF or `.msg`).
    fn from_owned(bytes: Vec<u8>, index: u32) -> Result<Self> {
        if tnef::has_signature(&bytes) {
            Self::tnef(&bytes)
        } else if bytes.starts_with(&CFB_MAGIC) {
            Self::msg(Box::new(Cursor::new(bytes)))
        } else {
            Err(Error::Unsupported(format!("attachment {index} is not an embedded message")))
        }
    }

    /// Descend into the message attached at `index`.
    pub fn child(self, index: u32) -> Result<Self> {
        match self {
            Node::Msg { mut reader, path, embedded, codepage } => match reader.attachment_target(&path, index)? {
                AttachTarget::Embedded(sub) => {
                    let codepage = reader.codepage_of(&path, embedded, codepage)?;
                    Ok(Node::Msg { reader, path: sub, embedded: true, codepage })
                }
                AttachTarget::Storage(_) => {
                    Err(Error::Unsupported(format!("attachment {index} is an OLE object, not a message")))
                }
                AttachTarget::Bytes(b) => Self::from_owned(b, index),
            },
            // The TNEF decoder leaves embedded messages as attachment bytes.
            Node::Mapi { mapi, .. } => Self::from_owned(take_payload(mapi, index)?, index),
        }
    }

    pub fn walk(mut self, chain: &[u32]) -> Result<Self> {
        for &i in chain {
            self = self.child(i)?;
        }
        Ok(self)
    }

    /// The decoded message; attachment payloads are omitted, their lengths kept in `Decoded`.
    pub fn message(self, load: Load) -> Result<Decoded> {
        match self {
            Node::Msg { mut reader, path, embedded, codepage } => reader.read_message(&path, embedded, codepage, load),
            Node::Mapi { mapi, size } => Ok(Decoded::strip(mapi, size)),
        }
    }

    /// Bytes of attachment `index`. Embedded `.msg` storages are re-serialised as
    /// standalone `.msg` files; nested TNEF messages are returned as TNEF streams.
    pub fn attachment(self, index: u32) -> Result<Vec<u8>> {
        match self {
            Node::Msg { mut reader, path, .. } => match reader.attachment_target(&path, index)? {
                AttachTarget::Bytes(b) => Ok(b),
                AttachTarget::Embedded(sub) => reader.extract_storage(&sub, true),
                AttachTarget::Storage(sub) => reader.extract_storage(&sub, false),
            },
            Node::Mapi { mapi, .. } => take_payload(mapi, index),
        }
    }
}

/// `PidTagAttachDataBinary` of attachment `index` of a decoded TNEF message.
fn take_payload(mapi: MapiMessage, index: u32) -> Result<Vec<u8>> {
    let count = mapi.attachments.len();
    let mut a = mapi
        .attachments
        .into_iter()
        .nth(index as usize)
        .ok_or_else(|| Error::NotFound(format!("attachment {index} (message has {count})")))?;
    match a.props.remove(&PropKey::Id(PID_ATTACH_DATA)).map(|p| p.value) {
        Some(PropValue::Binary(b)) => Ok(b),
        _ => Err(Error::NotFound(format!("attachment {index} has no data"))),
    }
}

/// Split a locator into the file path it starts from and the attachment chain.
pub(crate) fn split_locator(loc: &Locator) -> Result<(&str, Vec<u32>)> {
    let mut chain = Vec::new();
    let mut cur = loc;
    loop {
        match cur {
            Locator::Embedded { parent, attachment } => {
                chain.push(*attachment);
                cur = parent;
            }
            Locator::File { path } => {
                chain.reverse();
                return Ok((path, chain));
            }
            other => return Err(Error::NotFound(format!("locator {other:?} does not belong to a .msg/TNEF source"))),
        }
    }
}

/// Run a decoder, turning a panic inside a third-party parser into `Error::Format`.
pub(crate) fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(Error::Format("malformed container (decoder panicked)".into())))
}
