//! Outlook `.msg` ([MS-OXMSG]) and TNEF / `winmail.dat` ([MS-OXTNEF]) reader.
//!
//! Both formats decode into a [`mapi_props::MapiMessage`]; `mapi_props::convert` turns that
//! into the shared `mail_model` types so they render exactly like PST messages.
//!
//! Sources ([`opener`]): a single `.msg` file, a directory tree of `.msg` files (folders
//! mirror directories) — both [`mail_model::SourceKind::Msg`] — and a standalone TNEF file
//! ([`mail_model::SourceKind::Tnef`]). Messages are addressed by `Locator::File { path }`
//! (relative to the source root, empty for single-file sources) and `Locator::Embedded` for
//! messages attached to them. Attachment indices equal the position in
//! `MapiMessage::attachments`, which is what `AttachmentMeta::index` reports.

mod msg;
mod node;
mod source;
mod tnef;
mod value;

use mail_model::{Locator, Message, PropertyView, Result, SourceOpener};
use mapi_props::convert::property_views;

use crate::msg::Load;
use crate::node::{guard, Node};

pub use source::MsgOpener;

/// Entry point used by `stv-library` to sniff and open `.msg` / TNEF sources.
pub fn opener() -> &'static dyn SourceOpener {
    static OPENER: MsgOpener = MsgOpener;
    &OPENER
}

/// Decode a complete `.msg` file held in memory (e.g. an attachment of a MIME mail).
pub fn message_from_msg_bytes(bytes: &[u8], locator: Option<Locator>) -> Result<Message> {
    guard(|| Ok(Node::open_msg(bytes)?.message(Load::Full)?.to_message(locator, None)))
}

/// MAPI properties of an in-memory `.msg` file, for the property inspector.
pub fn properties_from_msg_bytes(bytes: &[u8]) -> Result<Vec<PropertyView>> {
    guard(|| Ok(property_views(&Node::open_msg(bytes)?.message(Load::Full)?.mapi.props)))
}

/// Bytes of attachment `index` of an in-memory `.msg` file. Embedded messages come back
/// as standalone `.msg` files.
pub fn attachment_from_msg_bytes(bytes: &[u8], index: u32) -> Result<Vec<u8>> {
    guard(|| Node::open_msg(bytes)?.attachment(index))
}

/// Decode a TNEF stream (`winmail.dat`, `application/ms-tnef` part).
pub fn message_from_tnef_bytes(bytes: &[u8], locator: Option<Locator>) -> Result<Message> {
    guard(|| Ok(Node::open_tnef(bytes)?.message(Load::Full)?.to_message(locator, None)))
}

/// MAPI properties of a TNEF stream, for the property inspector.
pub fn properties_from_tnef_bytes(bytes: &[u8]) -> Result<Vec<PropertyView>> {
    guard(|| Ok(property_views(&Node::open_tnef(bytes)?.message(Load::Full)?.mapi.props)))
}

/// Bytes of attachment `index` of a TNEF stream. Embedded messages come back as TNEF streams
/// (or `.msg` files when the writer encapsulated an OLE storage).
pub fn attachment_from_tnef_bytes(bytes: &[u8], index: u32) -> Result<Vec<u8>> {
    guard(|| Node::open_tnef(bytes)?.attachment(index))
}

/// True when `bytes` is a compound file containing an Outlook message.
pub(crate) fn msg_bytes_is_message(bytes: &[u8]) -> bool {
    bytes.starts_with(&value::CFB_MAGIC) && guard(|| Node::open_msg(bytes).map(|_| ())).is_ok()
}

/// Decode a `.msg`/TNEF container into raw MAPI form, following `chain` (attachment
/// indices) through embedded messages. Attachment payloads are omitted. For diagnostics
/// and tests that need property-level access (named properties, raw types).
pub fn decode_mapi(bytes: &[u8], chain: &[u32]) -> Result<mapi_props::MapiMessage> {
    guard(|| Ok(Node::open(bytes)?.walk(chain)?.message(Load::Full)?.mapi))
}
