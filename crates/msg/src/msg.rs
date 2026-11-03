//! Outlook `.msg` reader ([MS-OXMSG]) on top of the `cfb` compound-file crate.
//!
//! Layout recap:
//! - each message / recipient / attachment storage has a `__properties_version1.0`
//!   stream: a header (32 bytes top-level, 24 embedded, 8 recipient/attachment) followed
//!   by 16-byte entries `{tag, flags, value[8]}`; fixed-size values are inline;
//! - variable-length values live in `__substg1.0_TTTTTTTT` streams; multi-valued
//!   variable-length values have one `__substg1.0_TTTTTTTT-NNNNNNNN` stream per value;
//! - `__nameid_version1.0` (root only) maps ids ≥ 0x8000 to `(GUID, LID | name)`;
//! - recipients: `__recip_version1.0_#XXXXXXXX`, attachments: `__attach_version1.0_#XXXXXXXX`;
//! - an embedded message is the `__substg1.0_3701000D` storage of an attachment and
//!   shares the root's named-property map.

use std::collections::HashMap;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use cfb::CompoundFile;
use mail_model::{Error, Result};
use mapi_props::{MapiAttachment, MapiMessage, NamedId, Prop, PropKey, PropMap, PropType, PropValue};

use crate::value::*;

const PROPS_STREAM: &str = "__properties_version1.0";
const NAMEID_STORAGE: &str = "__nameid_version1.0";
const RECIP_PREFIX: &str = "__recip_version1.0_#";
const ATTACH_PREFIX: &str = "__attach_version1.0_#";
const SUBSTG_PREFIX: &str = "__substg1.0_";
const EMBEDDED_STORAGE: &str = "__substg1.0_3701000D";
/// Storage nesting accepted when copying a sub-tree (see `copy_tree`).
const MAX_STORAGE_DEPTH: u32 = 64;

/// Which properties to materialise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Load {
    /// Everything except attachment payloads (loaded on demand).
    Full,
    /// Skip body streams (plain/RTF/HTML) and attachment renderings: enough for a `Header`.
    Header,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StorageKind {
    TopLevel,
    Embedded,
    Child,
}

impl StorageKind {
    fn header_len(self) -> usize {
        match self {
            StorageKind::TopLevel => 32,
            StorageKind::Embedded => 24,
            StorageKind::Child => 8,
        }
    }
}

struct Child {
    name: String,
    len: u64,
    is_stream: bool,
}

fn fmt_err(e: std::io::Error) -> Error {
    Error::Format(format!("msg: {e}"))
}

fn join(parent: &str, name: &str) -> String {
    let p = parent.trim_end_matches('/');
    format!("{p}/{name}")
}

/// Hex suffix of `__recip_version1.0_#0000000A`-style names.
fn hex_suffix(name: &str, prefix: &str) -> Option<u32> {
    let rest = name.get(..prefix.len()).filter(|p| p.eq_ignore_ascii_case(prefix)).map(|_| &name[prefix.len()..])?;
    u32::from_str_radix(rest, 16).ok()
}

/// Parse `__substg1.0_TTTTTTTT[-NNNNNNNN]` → (tag, index).
fn parse_substg(name: &str) -> Option<(u32, Option<u32>)> {
    let rest = name
        .get(..SUBSTG_PREFIX.len())
        .filter(|p| p.eq_ignore_ascii_case(SUBSTG_PREFIX))
        .map(|_| &name[SUBSTG_PREFIX.len()..])?;
    let tag = u32::from_str_radix(rest.get(..8)?, 16).ok()?;
    match rest.len() {
        8 => Some((tag, None)),
        17 if rest.as_bytes()[8] == b'-' => Some((tag, Some(u32::from_str_radix(rest.get(9..)?, 16).ok()?))),
        _ => None,
    }
}

/// An open `.msg` compound file plus its named-property map.
pub(crate) struct MsgReader<F> {
    cfb: CompoundFile<F>,
    names: HashMap<u16, PropKey>,
    /// Byte length of the compound file.
    len: u64,
}

impl<F: Read + Seek> MsgReader<F> {
    pub fn open(mut inner: F) -> Result<Self> {
        let len = inner.seek(SeekFrom::End(0)).map_err(fmt_err)?;
        inner.rewind().map_err(fmt_err)?;
        let cfb = CompoundFile::open(inner).map_err(fmt_err)?;
        let mut r = MsgReader { cfb, names: HashMap::new(), len };
        if !r.has_stream("/", PROPS_STREAM) {
            return Err(Error::Format("compound file is not an Outlook message (no __properties_version1.0)".into()));
        }
        r.names = r.read_named_map()?;
        Ok(r)
    }

    fn children(&self, path: &str) -> Result<Vec<Child>> {
        let entries = self.cfb.read_storage(path).map_err(fmt_err)?;
        Ok(entries.map(|e| Child { name: e.name().to_owned(), len: e.len(), is_stream: e.is_stream() }).collect())
    }

    fn find(&self, path: &str, name: &str) -> Option<Child> {
        self.children(path).ok()?.into_iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn has_stream(&self, path: &str, name: &str) -> bool {
        self.find(path, name).is_some_and(|c| c.is_stream)
    }

    fn has_storage(&self, path: &str, name: &str) -> bool {
        self.find(path, name).is_some_and(|c| !c.is_stream)
    }

    fn read_stream(&mut self, path: &str) -> Result<Vec<u8>> {
        let mut s = self.cfb.open_stream(path).map_err(fmt_err)?;
        let mut buf = Vec::with_capacity(s.len().min(64 << 20) as usize);
        s.read_to_end(&mut buf).map_err(fmt_err)?;
        Ok(buf)
    }

    fn read_named_map(&mut self) -> Result<HashMap<u16, PropKey>> {
        let mut map = HashMap::new();
        let base = join("/", NAMEID_STORAGE);
        if !self.has_storage("/", NAMEID_STORAGE) {
            return Ok(map);
        }
        let read = |r: &mut Self, name: &str| -> Result<Vec<u8>> {
            if r.has_stream(&base, name) {
                r.read_stream(&join(&base, name))
            } else {
                Ok(Vec::new())
            }
        };
        let guids = read(self, "__substg1.0_00020102")?;
        let entries = read(self, "__substg1.0_00030102")?;
        let strings = read(self, "__substg1.0_00040102")?;
        for e in entries.as_chunks::<8>().0 {
            let id_or_off = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
            let info = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
            let is_string = info & 1 == 1;
            let guid_index = ((info >> 1) & 0x7FFF) as usize;
            let prop_index = (info >> 16) as u16;
            let Some(prop_id) = 0x8000u16.checked_add(prop_index) else { continue };
            let guid = match guid_index {
                1 => PS_MAPI,
                2 => PS_PUBLIC_STRINGS,
                n if n >= 3 => match guids.get((n - 3) * 16..(n - 2) * 16) {
                    Some(g) => g.try_into().expect("16-byte slice"),
                    None => continue,
                },
                _ => continue,
            };
            let id = if is_string {
                let off = id_or_off as usize;
                let Some(len) = u32_at(&strings, off) else { continue };
                let Some(s) = off.checked_add(4).and_then(|a| strings.get(a..a.checked_add(len as usize)?)) else {
                    continue;
                };
                NamedId::Name(utf16_to_string(s))
            } else {
                NamedId::Lid(id_or_off)
            };
            map.insert(prop_id, PropKey::Named { guid, id });
        }
        Ok(map)
    }

    fn key(&self, id: u16) -> PropKey {
        if id >= 0x8000 {
            if let Some(k) = self.names.get(&id) {
                return k.clone();
            }
        }
        PropKey::Id(id)
    }

    /// Read one property storage. String8 values are left as raw bytes for
    /// [`decode_string8_props`]. `skip(id)` omits a variable-length property.
    fn read_props(&mut self, path: &str, kind: StorageKind, skip: &dyn Fn(u16) -> bool) -> Result<PropMap> {
        let children = self.children(path)?;
        let mut map = PropMap::new();

        if children.iter().any(|c| c.is_stream && c.name.eq_ignore_ascii_case(PROPS_STREAM)) {
            let bytes = self.read_stream(&join(path, PROPS_STREAM))?;
            let mut hdr = kind.header_len();
            // Some writers use the top-level header size for embedded messages and vice versa.
            let fits = |h: usize| bytes.len().checked_sub(h).is_some_and(|n| n.is_multiple_of(16));
            if bytes.len() >= hdr && !fits(hdr) {
                for alt in [32usize, 24, 8] {
                    if fits(alt) {
                        hdr = alt;
                        break;
                    }
                }
            }
            for e in bytes.get(hdr..).unwrap_or_default().as_chunks::<16>().0 {
                let tag = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                let Some(ty) = PropType::from_u16(tag as u16) else { continue };
                let id = (tag >> 16) as u16;
                let value = &e[8..16];
                if ty == PropType::Object {
                    // Embedded message / OLE storage; resolved lazily via `child()`.
                    let name = format!("{SUBSTG_PREFIX}{tag:08X}");
                    if children.iter().any(|c| !c.is_stream && c.name.eq_ignore_ascii_case(&name)) {
                        map.insert(self.key(id), Prop { ty, value: PropValue::Object(0) });
                    }
                    continue;
                }
                if let Some(v) = decode_scalar(ty, value) {
                    if ty != PropType::Guid {
                        map.insert(self.key(id), Prop { ty, value: v });
                    }
                }
            }
        }

        // Variable-length and multi-valued properties.
        let mut mv_parts: HashMap<u32, Vec<(u32, String)>> = HashMap::new();
        for c in children.iter().filter(|c| c.is_stream) {
            let Some((tag, index)) = parse_substg(&c.name) else { continue };
            if let Some(i) = index {
                mv_parts.entry(tag).or_default().push((i, c.name.clone()));
            }
        }
        for c in children.iter().filter(|c| c.is_stream) {
            let Some((tag, None)) = parse_substg(&c.name) else { continue };
            let Some(ty) = PropType::from_u16(tag as u16) else { continue };
            let id = (tag >> 16) as u16;
            if skip(id) {
                continue;
            }
            let value = match ty {
                PropType::Unicode => PropValue::String(utf16_to_string(&self.read_stream(&join(path, &c.name))?)),
                PropType::String8 | PropType::Binary => PropValue::Binary(self.read_stream(&join(path, &c.name))?),
                PropType::Guid => {
                    let b = self.read_stream(&join(path, &c.name))?;
                    match decode_scalar(PropType::Guid, &b) {
                        Some(v) => v,
                        None => continue,
                    }
                }
                PropType::MultiString8 | PropType::MultiUnicode | PropType::MultiBinary => {
                    let mut parts = mv_parts.remove(&tag).unwrap_or_default();
                    parts.sort_unstable_by_key(|p| p.0);
                    let mut vals = Vec::with_capacity(parts.len());
                    for (_, name) in &parts {
                        vals.push(self.read_stream(&join(path, name))?);
                    }
                    match ty {
                        PropType::MultiUnicode => {
                            PropValue::MultiString(vals.iter().map(|b| utf16_to_string(b)).collect())
                        }
                        _ => PropValue::MultiBinary(vals),
                    }
                }
                _ if mv_elem(ty).is_some() => {
                    let b = self.read_stream(&join(path, &c.name))?;
                    match decode_mv_fixed(ty, &b) {
                        Some(v) => v,
                        None => continue,
                    }
                }
                _ => continue,
            };
            map.insert(self.key(id), Prop { ty, value });
        }
        Ok(map)
    }

    fn numbered_storages(&self, path: &str, prefix: &str) -> Result<Vec<String>> {
        let mut v: Vec<(u32, String)> = self
            .children(path)?
            .into_iter()
            .filter(|c| !c.is_stream)
            .filter_map(|c| hex_suffix(&c.name, prefix).map(|n| (n, c.name)))
            .collect();
        v.sort_unstable_by_key(|x| x.0);
        Ok(v.into_iter().map(|x| join(path, &x.1)).collect())
    }

    /// Storage paths of the attachments of the message at `path`, in index order.
    pub fn attachment_paths(&self, path: &str) -> Result<Vec<String>> {
        self.numbered_storages(path, ATTACH_PREFIX)
    }

    /// Decode the message stored at `path` (`"/"` for the file itself).
    pub fn read_message(
        &mut self,
        path: &str,
        embedded: bool,
        inherited_cp: Option<u32>,
        load: Load,
    ) -> Result<Decoded> {
        let header_skip = |id: u16| matches!(id, PID_BODY | PID_RTF_COMPRESSED | PID_BODY_HTML);
        let no_skip = |_: u16| false;
        let skip: &dyn Fn(u16) -> bool = if load == Load::Header { &header_skip } else { &no_skip };
        let kind = if embedded { StorageKind::Embedded } else { StorageKind::TopLevel };
        let mut props = self.read_props(path, kind, skip)?;
        let cp = message_codepage(&props, inherited_cp);
        decode_string8_props(&mut props, cp);

        let mut recipients = Vec::new();
        for rp in self.numbered_storages(path, RECIP_PREFIX)? {
            let mut r = self.read_props(&rp, StorageKind::Child, &no_skip)?;
            decode_string8_props(&mut r, cp);
            recipients.push(r);
        }

        // Payloads are loaded on demand by `attachment_target`; only their sizes are kept.
        let att_skip = |id: u16| id == PID_ATTACH_RENDERING || id == PID_ATTACH_DATA;
        let data_stream = format!("{SUBSTG_PREFIX}37010102");
        let mut attachments = Vec::new();
        let mut payload_sizes = Vec::new();
        for ap in self.attachment_paths(path)? {
            let mut a = self.read_props(&ap, StorageKind::Child, &att_skip)?;
            decode_string8_props(&mut a, cp);
            let mut payload = self.find(&ap, &data_stream).filter(|c| c.is_stream).map(|c| c.len);
            if self.is_embedded_message(&ap) {
                a.insert(
                    PropKey::Id(PID_ATTACH_METHOD),
                    Prop { ty: PropType::Int32, value: PropValue::Int32(ATTACH_EMBEDDED_MSG) },
                );
                // Writers usually omit PR_ATTACH_SIZE for embedded messages.
                if int_prop(&a, PID_ATTACH_SIZE).is_none_or(|s| s <= 0) {
                    payload = Some(self.storage_size(&join(&ap, EMBEDDED_STORAGE))?);
                }
            }
            payload_sizes.push(payload);
            attachments.push(MapiAttachment { props: a, embedded: None });
        }
        // `.msg` files don't store PR_MESSAGE_SIZE: fall back to the container / storage size.
        let size = if path == "/" { self.len } else { self.storage_size(path)? };
        Ok(Decoded { mapi: MapiMessage { props, recipients, attachments }, payload_sizes, size })
    }

    /// Total length of the streams below storage `path`: the size of the message it holds.
    fn storage_size(&self, path: &str) -> Result<u64> {
        Ok(self.cfb.walk_storage(path).map_err(fmt_err)?.filter(|e| e.is_stream()).map(|e| e.len()).sum())
    }

    fn is_embedded_message(&self, att_path: &str) -> bool {
        self.has_storage(att_path, EMBEDDED_STORAGE) && self.has_stream(&join(att_path, EMBEDDED_STORAGE), PROPS_STREAM)
    }

    /// What attachment `index` of the message at `path` holds.
    pub fn attachment_target(&mut self, path: &str, index: u32) -> Result<AttachTarget> {
        let paths = self.attachment_paths(path)?;
        let ap = paths
            .get(index as usize)
            .ok_or_else(|| Error::NotFound(format!("attachment {index} (message has {})", paths.len())))?
            .clone();
        if self.has_storage(&ap, EMBEDDED_STORAGE) {
            let sub = join(&ap, EMBEDDED_STORAGE);
            return Ok(if self.has_stream(&sub, PROPS_STREAM) {
                AttachTarget::Embedded(sub)
            } else {
                AttachTarget::Storage(sub)
            });
        }
        let data = format!("{SUBSTG_PREFIX}37010102");
        if self.has_stream(&ap, &data) {
            return Ok(AttachTarget::Bytes(self.read_stream(&join(&ap, &data))?));
        }
        Err(Error::NotFound(format!("attachment {index} has no data (by reference)")))
    }

    /// Codepage the message at `path` uses for String8 values (for nested resolution).
    pub fn codepage_of(&mut self, path: &str, embedded: bool, inherited: Option<u32>) -> Result<Option<u32>> {
        let kind = if embedded { StorageKind::Embedded } else { StorageKind::TopLevel };
        let props = self.read_props(path, kind, &|id| !(0x3FD0..0x4000).contains(&id))?;
        Ok(message_codepage(&props, inherited))
    }

    /// Serialise the storage at `path` as a standalone compound file. For an embedded
    /// message (`as_message`), the properties header grows to the top-level 32 bytes and
    /// the root's named-property map is copied so the result is a valid `.msg`.
    pub fn extract_storage(&mut self, path: &str, as_message: bool) -> Result<Vec<u8>> {
        let mut out = CompoundFile::create(Cursor::new(Vec::new())).map_err(fmt_err)?;
        self.copy_tree(path, &mut out, "/", as_message, 0)?;
        if as_message && self.has_storage("/", NAMEID_STORAGE) && !self.has_storage(path, NAMEID_STORAGE) {
            self.copy_tree(&join("/", NAMEID_STORAGE), &mut out, &join("/", NAMEID_STORAGE), false, 0)?;
        }
        out.flush().map_err(fmt_err)?;
        Ok(out.into_inner().into_inner())
    }

    fn copy_tree(
        &mut self,
        src: &str,
        out: &mut CompoundFile<Cursor<Vec<u8>>>,
        dst: &str,
        fix_header: bool,
        depth: u32,
    ) -> Result<()> {
        // Real messages nest two storages per embedded level; a deeper chain is a crafted
        // file that would otherwise overflow the stack (uncatchable by the panic guard).
        if depth > MAX_STORAGE_DEPTH {
            return Err(Error::Format(format!("msg: storages nested deeper than {MAX_STORAGE_DEPTH}")));
        }
        if dst != "/" {
            out.create_storage(dst).map_err(fmt_err)?;
        }
        for c in self.children(src)? {
            let s = join(src, &c.name);
            let d = join(dst, &c.name);
            if !c.is_stream {
                // Nested embedded messages keep their 24-byte header.
                self.copy_tree(&s, out, &d, false, depth + 1)?;
            } else if fix_header && c.name.eq_ignore_ascii_case(PROPS_STREAM) {
                let mut bytes = self.read_stream(&s)?;
                if bytes.len().checked_sub(24).is_some_and(|n| n.is_multiple_of(16)) {
                    bytes.splice(24..24, [0u8; 8]);
                }
                out.create_stream(&d).and_then(|mut w| w.write_all(&bytes)).map_err(fmt_err)?;
            } else {
                let mut r = self.cfb.open_stream(&s).map_err(fmt_err)?;
                let mut w = out.create_stream(&d).map_err(fmt_err)?;
                std::io::copy(&mut r, &mut w).map_err(fmt_err)?;
            }
        }
        Ok(())
    }
}

/// Content of one attachment storage.
pub(crate) enum AttachTarget {
    /// `PidTagAttachDataBinary` bytes.
    Bytes(Vec<u8>),
    /// Storage path of an embedded message.
    Embedded(String),
    /// Storage path of an OLE object that is not a message.
    Storage(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substg_names() {
        assert_eq!(parse_substg("__substg1.0_0037001F"), Some((0x0037_001F, None)));
        assert_eq!(parse_substg("__substg1.0_8003101f-00000002"), Some((0x8003_101F, Some(2))));
        assert_eq!(parse_substg("__substg1.0_0037001"), None);
        assert_eq!(parse_substg("__properties_version1.0"), None);
        assert_eq!(hex_suffix("__attach_version1.0_#0000000A", ATTACH_PREFIX), Some(10));
        assert_eq!(hex_suffix("__attach_version1.0_#zz", ATTACH_PREFIX), None);
    }
}
