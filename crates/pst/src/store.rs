//! Messaging layer ([MS-PST] 2.4): message store, named-property map, folders, messages,
//! recipients, attachments and embedded messages, producing [`MapiMessage`]s.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use mail_model::{Error, FolderKind, Result};
use mapi_props::{names, tags, MapiAttachment, MapiMessage, NamedId, Prop, PropKey, PropMap, PropType, PropValue};

use crate::ltp::{decode_value, read_pc, Column, Node, Row, Table};
use crate::ndb::{Backing, Ndb, NodeEntry};

pub const NID_MESSAGE_STORE: u32 = 0x21;
pub const NID_NAME_TO_ID_MAP: u32 = 0x61;
pub const NID_ROOT_FOLDER: u32 = 0x122;
pub const NID_ATTACHMENT_TABLE: u32 = 0x671;
pub const NID_RECIPIENT_TABLE: u32 = 0x692;

pub const NID_TYPE_NORMAL_FOLDER: u32 = 0x02;
pub const NID_TYPE_SEARCH_FOLDER: u32 = 0x03;
pub const NID_TYPE_NORMAL_MESSAGE: u32 = 0x04;
pub const NID_TYPE_ASSOC_MESSAGE: u32 = 0x08;
pub const NID_TYPE_HIERARCHY_TABLE: u32 = 0x0D;
pub const NID_TYPE_CONTENTS_TABLE: u32 = 0x0E;

pub fn nid_type(nid: u32) -> u32 {
    nid & 0x1F
}

/// Sibling node of a folder (`hierarchy`/`contents` table) sharing its index.
pub fn folder_table_nid(folder: u32, ty: u32) -> u32 {
    (folder & !0x1F) | ty
}

/// Folder metadata read from the folder's property context.
#[derive(Clone, Debug)]
pub struct FolderInfo {
    pub nid: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub container_class: Option<String>,
    pub kind: FolderKind,
    pub content_count: Option<u64>,
    pub unread_count: Option<u64>,
}

/// Folder kind from `PidTagContainerClass` (`IPF.Note`, `IPF.Appointment`, …).
pub fn folder_kind(class: Option<&str>) -> FolderKind {
    let Some(c) = class else {
        return FolderKind::Mail;
    };
    let c = c.to_ascii_lowercase();
    let is = |p: &str| c.strip_prefix(p).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'));
    if c.is_empty() || is("ipf.note") || is("ipf.imap") || c == "ipf" {
        FolderKind::Mail
    } else if is("ipf.appointment") {
        FolderKind::Calendar
    } else if is("ipf.contact") {
        FolderKind::Contacts
    } else if is("ipf.task") {
        FolderKind::Tasks
    } else if is("ipf.stickynote") {
        FolderKind::Notes
    } else if is("ipf.journal") {
        FolderKind::Journal
    } else {
        FolderKind::Other
    }
}

/// An opened PST/OST: NDB + named property map + store properties.
pub struct PstFile {
    pub ndb: Arc<Ndb>,
    named: HashMap<u16, ([u8; 16], NamedId)>,
    pub store_props: PropMap,
}

impl PstFile {
    /// Memory-map `path` and open it.
    pub fn open(path: &Path) -> Result<PstFile> {
        let f = std::fs::File::open(path)?;
        // SAFETY: the map is read-only and every read is bounds-checked against its length, but
        // the bytes are only stable while no other process writes or truncates the file. That is
        // the accepted mmap caveat for a viewer: Outlook locks the stores it has open, and
        // archives being viewed are not expected to change underneath.
        let map = unsafe { memmap2::Mmap::map(&f)? };
        PstFile::from_backing(Backing::Map(map))
    }

    /// Open an in-memory PST/OST image.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<PstFile> {
        PstFile::from_backing(Backing::Vec(bytes))
    }

    fn from_backing(b: Backing) -> Result<PstFile> {
        let ndb = Arc::new(Ndb::new(b)?);
        let mut f = PstFile { ndb, named: HashMap::new(), store_props: PropMap::new() };
        match f.load_name_map() {
            Ok(m) => f.named = m,
            Err(e) => tracing::warn!("named property map unreadable: {e}"),
        }
        f.store_props = f.props(&f.node(NID_MESSAGE_STORE)?, false)?;
        Ok(f)
    }

    /// Top-level node from the NBT with its subnodes loaded.
    pub fn node(&self, nid: u32) -> Result<Node> {
        let e = self.ndb.node(nid)?;
        Node::load(&self.ndb, nid, e.bid_data, e.bid_sub)
    }

    pub fn node_entry(&self, nid: u32) -> Result<NodeEntry> {
        self.ndb.node(nid)
    }

    /// Named property map ([MS-PST] 2.4.7): property id (≥ 0x8000) → (GUID, name/LID).
    fn load_name_map(&self) -> Result<HashMap<u16, ([u8; 16], NamedId)>> {
        let node = self.node(NID_NAME_TO_ID_MAP)?;
        let raw = read_pc(&self.ndb, &node, &|_, _| false)?;
        let bytes = |id: u16| raw.iter().find(|p| p.id == id).and_then(|p| p.bytes.as_deref().map(<[u8]>::to_vec));
        let guids = bytes(0x0002).unwrap_or_default();
        let entries = bytes(0x0003).unwrap_or_default();
        let strings = bytes(0x0004).unwrap_or_default();
        let mut map = HashMap::with_capacity(entries.len() / 8);
        for e in entries.as_chunks::<8>().0 {
            let prop_id = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
            let guid_n = u16::from_le_bytes([e[4], e[5]]);
            let idx = u16::from_le_bytes([e[6], e[7]]);
            let guid = match guid_n >> 1 {
                0 => continue,
                1 => names::PS_MAPI,
                2 => names::PS_PUBLIC_STRINGS,
                g => {
                    let o = (g as usize - 3) * 16;
                    match guids.get(o..o + 16) {
                        Some(s) => s.try_into().unwrap(),
                        None => continue,
                    }
                }
            };
            let id = if guid_n & 1 == 1 {
                let o = prop_id as usize;
                let Some(len) = strings.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()) as usize)
                else {
                    continue;
                };
                let Some(name) = strings.get(o + 4..o + 4 + len) else {
                    continue;
                };
                NamedId::Name(mapi_props::codepage::decode_utf16le(name))
            } else {
                NamedId::Lid(prop_id)
            };
            map.insert(0x8000u16.wrapping_add(idx), (guid, id));
        }
        Ok(map)
    }

    /// Map a raw property id to a [`PropKey`], resolving named properties.
    pub fn key(&self, id: u16) -> PropKey {
        if id >= 0x8000 {
            if let Some((guid, nid)) = self.named.get(&id) {
                return PropKey::Named { guid: *guid, id: nid.clone() };
            }
        }
        PropKey::Id(id)
    }

    /// Property id used in this file for a named property, if mapped.
    pub fn named_id(&self, guid: &[u8; 16], id: &NamedId) -> Option<u16> {
        self.named.iter().find(|(_, (g, n))| g == guid && n == id).map(|(k, _)| *k)
    }

    /// Decode a node's property context into a [`PropMap`]. With `lazy_data`, the attachment
    /// payload (`PidTagAttachDataBinary`) is not loaded.
    pub fn props(&self, node: &Node, lazy_data: bool) -> Result<PropMap> {
        self.props_sized(node, lazy_data).map(|(m, _)| m)
    }

    /// Like [`PstFile::props`], also returning the size of the payload left unread by `lazy_data`.
    pub fn props_sized(&self, node: &Node, lazy_data: bool) -> Result<(PropMap, Option<u64>)> {
        let skip = |id: u16, ty: u16| lazy_data && id == tags::ATTACH_DATA_BINARY && ty == PropType::Binary as u16;
        self.props_skipping(node, &skip)
    }

    /// A message's properties without its bodies (plain text, RTF, HTML): what a list row
    /// needs when the contents table lacks it.
    pub fn props_without_bodies(&self, node: &Node) -> Result<PropMap> {
        let skip = |id: u16, _| matches!(id, tags::BODY | tags::RTF_COMPRESSED | tags::HTML);
        self.props_skipping(node, &skip).map(|(m, _)| m)
    }

    /// Decode a property context; properties `skip` selects are not loaded (their size is
    /// returned when one was).
    fn props_skipping(&self, node: &Node, skip: &dyn Fn(u16, u16) -> bool) -> Result<(PropMap, Option<u64>)> {
        let raw = read_pc(&self.ndb, node, skip)?;
        let cp = raw
            .iter()
            .find(|p| p.id == tags::MESSAGE_CODEPAGE && p.ty == PropType::Int32 as u16)
            .map(|p| u32::from_le_bytes(p.inline))
            .or_else(|| {
                raw.iter()
                    .find(|p| p.id == tags::INTERNET_CODEPAGE && p.ty == PropType::Int32 as u16)
                    .map(|p| u32::from_le_bytes(p.inline))
            });
        let mut map = PropMap::new();
        let mut skipped = None;
        for p in raw {
            let Some(ty) = PropType::from_u16(p.ty) else {
                continue;
            };
            let value = if ty == PropType::Object {
                // PtypObject: heap item holds { nid: u32, size: u32 } of a subnode.
                match p.bytes.as_deref() {
                    Some(b) if b.len() >= 4 => PropValue::Object(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64),
                    _ => PropValue::Object(u32::from_le_bytes(p.inline) as u64),
                }
            } else if p.bytes.is_none() && ty.fixed_size().is_none_or(|n| n > 4) {
                // Deliberately not loaded (lazy attachment data).
                skipped = p.skipped_size;
                continue;
            } else {
                let bytes: &[u8] = match &p.bytes {
                    Some(b) => b,
                    None => &p.inline,
                };
                match decode_value(ty, bytes, cp) {
                    Some(v) => v,
                    None => {
                        tracing::warn!("node 0x{:X}: malformed property 0x{:04X}{:04X}", node.nid, p.id, p.ty);
                        continue;
                    }
                }
            };
            map.insert(self.key(p.id), Prop { ty, value });
        }
        Ok((map, skipped))
    }

    /// All present cells of a table row.
    pub fn row_props(&self, t: &Table, row: &Row, cp: Option<u32>) -> PropMap {
        self.row_props_in(t, row, &t.columns, cp)
    }

    /// Present cells of `cols` (a subset of `t.columns`) in a table row.
    pub fn row_props_in(&self, t: &Table, row: &Row, cols: &[Column], cp: Option<u32>) -> PropMap {
        let mut map = PropMap::new();
        for c in cols {
            let Some(ty) = PropType::from_u16(c.ty) else {
                continue;
            };
            if let Some(v) = t.value(row, c, cp) {
                map.insert(self.key(c.id), Prop { ty, value: v });
            }
        }
        map
    }

    /// Columns of a contents table that [`mapi_props::convert::header_from_mapi`] reads.
    pub fn header_columns(&self, t: &Table) -> Vec<Column> {
        const IDS: &[u16] = &[
            tags::SUBJECT,
            tags::SUBJECT_PREFIX,
            tags::NORMALIZED_SUBJECT,
            tags::CONVERSATION_TOPIC,
            tags::CONVERSATION_INDEX,
            tags::SENDER_NAME,
            tags::SENDER_EMAIL_ADDRESS,
            tags::SENDER_ADDRESS_TYPE,
            tags::SENDER_SMTP_ADDRESS,
            tags::SENT_REPRESENTING_NAME,
            tags::SENT_REPRESENTING_EMAIL_ADDRESS,
            tags::SENT_REPRESENTING_ADDRESS_TYPE,
            tags::SENT_REPRESENTING_SMTP_ADDRESS,
            tags::DISPLAY_TO,
            tags::DISPLAY_CC,
            tags::DISPLAY_BCC,
            tags::MESSAGE_DELIVERY_TIME,
            tags::CLIENT_SUBMIT_TIME,
            tags::CREATION_TIME,
            tags::MESSAGE_SIZE,
            tags::MESSAGE_FLAGS,
            tags::HAS_ATTACHMENTS,
            tags::FLAG_STATUS,
            tags::MESSAGE_CLASS,
            tags::INTERNET_MESSAGE_ID,
            tags::IN_REPLY_TO_ID,
            tags::INTERNET_REFERENCES,
        ];
        let keywords = self.named_id(&names::PS_PUBLIC_STRINGS, &NamedId::Name(names::NAME_KEYWORDS.into()));
        // Calendar, contact and task fields, so list rows carry their item details.
        let item_named: Vec<u16> =
            mapi_props::items::ROW_NAMED.iter().filter_map(|(g, lid)| self.named_id(g, &NamedId::Lid(*lid))).collect();
        t.columns
            .iter()
            .filter(|c| {
                IDS.contains(&c.id)
                    || Some(c.id) == keywords
                    || mapi_props::items::ROW_FIXED_IDS.contains(&c.id)
                    || item_named.contains(&c.id)
            })
            .copied()
            .collect()
    }

    pub fn table(&self, node: Node) -> Result<Table> {
        Table::open(&self.ndb, node)
    }

    pub fn display_name(&self) -> Option<String> {
        mapi_props::get(&self.store_props, tags::DISPLAY_NAME).and_then(PropValue::as_str).map(str::to_owned)
    }

    /// NID of the IPM subtree ("Top of Personal Folders") from `PidTagIpmSubtreeEntryId`.
    pub fn ipm_subtree(&self) -> Option<u32> {
        let b = mapi_props::get(&self.store_props, tags::IPM_SUBTREE_ENTRY_ID).and_then(PropValue::as_bytes)?;
        b.get(20..24).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }

    pub fn folder_info(&self, nid: u32, parent: Option<u32>) -> Result<FolderInfo> {
        let p = self.props(&self.node(nid)?, false)?;
        let s = |id| mapi_props::get(&p, id).and_then(PropValue::as_str).map(str::to_owned);
        let n = |id| mapi_props::get(&p, id).and_then(PropValue::as_i64).map(|v| v.max(0) as u64);
        let class = s(tags::CONTAINER_CLASS);
        Ok(FolderInfo {
            nid,
            parent,
            name: s(tags::DISPLAY_NAME).unwrap_or_default(),
            kind: folder_kind(class.as_deref()),
            container_class: class,
            content_count: n(tags::CONTENT_COUNT),
            unread_count: n(tags::CONTENT_UNREAD_COUNT),
        })
    }

    /// Child folder NIDs from a folder's hierarchy table (row ids), in table order.
    pub fn child_folders(&self, folder: u32) -> Result<Vec<u32>> {
        let nid = folder_table_nid(folder, NID_TYPE_HIERARCHY_TABLE);
        let node = match self.node(nid) {
            Ok(n) => n,
            Err(Error::NotFound(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let t = self.table(node)?;
        let mut out = Vec::new();
        t.for_each_row(&mut |_, r| out.push(t.row_id(r)));
        Ok(out)
    }

    /// Contents table of a folder (`None` if the folder has none).
    pub fn contents_table(&self, folder: u32) -> Result<Option<Table>> {
        match self.node(folder_table_nid(folder, NID_TYPE_CONTENTS_TABLE)) {
            Ok(n) => self.table(n).map(Some),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Rows of a sub-table (recipients / attachments) of a message node.
    fn sub_table(&self, msg: &Node, nid: u32) -> Result<Option<Table>> {
        match msg.subnode(nid) {
            Some(_) => self.table(msg.child(&self.ndb, nid)?).map(Some),
            None => Ok(None),
        }
    }

    /// Full MAPI message: properties, recipients, attachment properties (payload not loaded,
    /// embedded messages not decoded). Also returns each attachment's payload size, when it
    /// is stored by value (exact, unlike `PidTagAttachSize` which includes overhead).
    pub fn message(&self, node: &Node) -> Result<(MapiMessage, Vec<Option<u64>>)> {
        let props = self.props(node, true)?;
        let cp = mapi_props::get(&props, tags::MESSAGE_CODEPAGE).and_then(PropValue::as_i64).map(|v| v as u32);
        let mut recipients = Vec::new();
        match self.sub_table(node, NID_RECIPIENT_TABLE) {
            Ok(Some(t)) => t.for_each_row(&mut |_, r| recipients.push(self.row_props(&t, r, cp))),
            Ok(None) => {}
            Err(e) => tracing::warn!("message 0x{:X}: recipient table: {e}", node.nid),
        }
        let mut sizes = Vec::new();
        let mut attachments = Vec::new();
        match self.sub_table(node, NID_ATTACHMENT_TABLE) {
            Ok(Some(t)) => t.for_each_row(&mut |_, r| {
                let (props, size) = self.attachment_props(node, &t, r, cp);
                sizes.push(size);
                attachments.push(MapiAttachment { props, embedded: None });
            }),
            Ok(None) => {}
            Err(e) => tracing::warn!("message 0x{:X}: attachment table: {e}", node.nid),
        }
        Ok((MapiMessage { props, recipients, attachments }, sizes))
    }

    /// Properties of an attachment (payload not loaded) and its payload size: from the
    /// attachment object when present, else the attachment table row.
    fn attachment_props(&self, msg: &Node, t: &Table, row: &Row, cp: Option<u32>) -> (PropMap, Option<u64>) {
        let id = t.row_id(row);
        if msg.subnode(id).is_some() {
            match msg.child(&self.ndb, id).and_then(|n| self.props_sized(&n, true)) {
                Ok(p) => return p,
                Err(e) => tracing::warn!("message 0x{:X}: attachment object: {e}", msg.nid),
            }
        }
        (self.row_props(t, row, cp), None)
    }

    /// Node of attachment `index`: the `index`-th readable row of the attachment table, the
    /// same numbering as [`PstFile::message`] uses.
    pub fn attachment_node(&self, msg: &Node, index: u32) -> Result<Node> {
        let not_found = || Error::NotFound(format!("attachment {index} of message 0x{:X}", msg.nid));
        let t = self.sub_table(msg, NID_ATTACHMENT_TABLE)?.ok_or_else(not_found)?;
        let (mut pos, mut id) = (0u32, None);
        t.for_each_row(&mut |_, r| {
            if pos == index {
                id = Some(t.row_id(r));
            }
            pos += 1;
        });
        let id = id.ok_or_else(not_found)?;
        if msg.subnode(id).is_none() {
            return Err(Error::Format(format!("attachment {index} has no attachment object")));
        }
        msg.child(&self.ndb, id)
    }

    /// Payload bytes of an attach-by-value attachment.
    pub fn attachment_data(&self, msg: &Node, index: u32) -> Result<Vec<u8>> {
        let a = self.attachment_node(msg, index)?;
        let mut p = self.props(&a, false)?;
        let method = mapi_props::get(&p, tags::ATTACH_METHOD).and_then(PropValue::as_i64).unwrap_or(1);
        match p.remove(&PropKey::Id(tags::ATTACH_DATA_BINARY)).map(|p| p.value) {
            Some(PropValue::Binary(b)) => Ok(b),
            Some(PropValue::Object(_)) | None if method == tags::ATTACH_EMBEDDED_MSG as i64 => {
                Err(Error::Unsupported("embedded message attachments open via Locator::Embedded".into()))
            }
            Some(PropValue::Object(_)) => Err(Error::Unsupported("OLE object attachment payload".into())),
            _ if method != tags::ATTACH_BY_VALUE as i64 => {
                Err(Error::Unsupported(format!("attachment method {method} has no stored payload")))
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Node of the message embedded in attachment `index` (attach method 5).
    pub fn embedded_node(&self, msg: &Node, index: u32) -> Result<Node> {
        let a = self.attachment_node(msg, index)?;
        let p = self.props(&a, true)?;
        let method = mapi_props::get(&p, tags::ATTACH_METHOD).and_then(PropValue::as_i64).unwrap_or(1);
        if method != tags::ATTACH_EMBEDDED_MSG as i64 {
            return Err(Error::Unsupported(format!(
                "attachment {index} is not a natively embedded message (method {method})"
            )));
        }
        match p.get(&PropKey::Id(tags::ATTACH_DATA_OBJECT)).map(|p| &p.value) {
            Some(PropValue::Object(nid)) => a.child(&self.ndb, *nid as u32),
            _ => Err(Error::Format(format!("embedded message attachment {index} has no object subnode"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_kinds_from_container_class() {
        assert_eq!(folder_kind(None), FolderKind::Mail);
        assert_eq!(folder_kind(Some("IPF.Note")), FolderKind::Mail);
        assert_eq!(folder_kind(Some("IPF.Note.OutlookHomepage")), FolderKind::Mail);
        assert_eq!(folder_kind(Some("IPF.Appointment")), FolderKind::Calendar);
        assert_eq!(folder_kind(Some("IPF.Contact.MOC.QuickContacts")), FolderKind::Contacts);
        assert_eq!(folder_kind(Some("IPF.Task")), FolderKind::Tasks);
        assert_eq!(folder_kind(Some("IPF.StickyNote")), FolderKind::Notes);
        assert_eq!(folder_kind(Some("IPF.Journal")), FolderKind::Journal);
        assert_eq!(folder_kind(Some("IPF.Configuration")), FolderKind::Other);
        assert_eq!(folder_kind(Some("IPF.Notes")), FolderKind::Other);
    }

    #[test]
    fn nid_helpers() {
        assert_eq!(folder_table_nid(0x8022, NID_TYPE_CONTENTS_TABLE), 0x802E);
        assert_eq!(nid_type(0x200024), NID_TYPE_NORMAL_MESSAGE);
    }
}
