//! Read-only PST/OST reader ([MS-PST]): ANSI, Unicode and Unicode-4K (Outlook 2013+ OST,
//! zlib-compressed blocks) files, any NDB encoding. Memory-mapped and lazy: opening reads the
//! header, store properties and the named-property map only; folder contents tables are decoded
//! on first use and cached as compact row keys.
//!
//! Locators: [`Locator::Pst`] for messages, [`Locator::Embedded`] (recursive) for messages
//! embedded as attachments (attach method 5). Folder ids are `nid:<hex>`.

pub mod crypt;
pub mod ltp;
pub mod ndb;
pub mod store;

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use mail_model::{
    Error, Fingerprint, Folder, FolderId, Header, Locator, MailSource, Message, PropertyView, Result, Sort, SortKey,
    SourceKind, SourceOpener,
};
use mapi_props::items::ItemKind;
use mapi_props::{convert, tags, MapiMessage, PropMap};

use crate::ltp::{Node, Table};
pub use crate::ndb::Format;
use crate::store::{nid_type, FolderInfo, PstFile, NID_ROOT_FOLDER, NID_TYPE_NORMAL_FOLDER};

/// `nid:<hex>` folder id.
pub fn folder_id(nid: u32) -> FolderId {
    FolderId(format!("nid:{nid:x}"))
}

/// Inverse of [`folder_id`]; `NotFound` for ids not of the form `nid:<hex>`.
pub fn parse_folder_id(id: &FolderId) -> Result<u32> {
    id.0.strip_prefix("nid:")
        .and_then(|h| u32::from_str_radix(h, 16).ok())
        .ok_or_else(|| Error::NotFound(format!("folder {:?}", id.0)))
}

/// Per-row sort keys of a contents table, decoded once per folder.
struct RowKey {
    row: u32,
    nid: u32,
    date: i64,
    size: u32,
}

struct FolderIndex {
    table: Table,
    /// Contents-table columns needed to build a [`Header`].
    header_cols: Vec<ltp::Column>,
    /// Item kinds whose details the table lacks ([`ItemKind::key_column`]): rows of these
    /// kinds are completed from their message node.
    lacking: Vec<ItemKind>,
    rows: Vec<RowKey>,
    /// Ascending permutations (indices into `rows`) per sort key, built on first use.
    perms: Mutex<[Option<Arc<Vec<u32>>>; 6]>,
}

/// A PST/OST file as a [`MailSource`].
pub struct PstSource {
    path: PathBuf,
    file: PstFile,
    kind: SourceKind,
    folders: OnceLock<std::result::Result<Vec<Folder>, String>>,
    indexes: Mutex<HashMap<u32, Arc<FolderIndex>>>,
}

impl PstSource {
    /// Map the file and read its header, store properties and named-property map.
    pub fn open(path: &Path) -> Result<PstSource> {
        let file = PstFile::open(path)?;
        let ext_ost = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ost"));
        let kind = if file.ndb.header.is_ost() || ext_ost { SourceKind::Ost } else { SourceKind::Pst };
        Ok(PstSource {
            path: path.to_owned(),
            file,
            kind,
            folders: OnceLock::new(),
            indexes: Mutex::new(HashMap::new()),
        })
    }

    /// Underlying file (NDB/LTP access for diagnostics).
    pub fn file(&self) -> &PstFile {
        &self.file
    }

    pub fn format(&self) -> Format {
        self.file.ndb.format()
    }

    /// Folder tree. The IPM subtree ("Top of Personal Folders" / OST `IPM_SUBTREE`) becomes
    /// the first root, named after the store; other non-search folders under the NDB root
    /// are kept (with their ancestors) only when their subtree holds messages.
    fn build_folders(&self) -> Result<Vec<Folder>> {
        let f = &self.file;
        let mut all: Vec<FolderInfo> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        match f.child_folders(NID_ROOT_FOLDER) {
            Ok(children) => {
                for c in children.into_iter().filter(|&c| nid_type(c) == NID_TYPE_NORMAL_FOLDER) {
                    self.collect(c, None, 0, &mut all, &mut seen);
                }
            }
            Err(e) => tracing::warn!("root hierarchy table unreadable: {e}"),
        }
        if all.is_empty() {
            // Damaged root: fall back to the IPM subtree named by the store.
            if let Some(ipm) = f.ipm_subtree() {
                self.collect(ipm, None, 0, &mut all, &mut seen);
            }
        }
        if all.is_empty() {
            return Err(Error::Format("no folders found under the root folder".into()));
        }

        // Subtree message totals (pre-order list: children follow their parent).
        let pos: HashMap<u32, usize> = all.iter().enumerate().map(|(i, fi)| (fi.nid, i)).collect();
        let mut total: Vec<u64> = all.iter().map(|fi| fi.content_count.unwrap_or(0)).collect();
        for i in (0..all.len()).rev() {
            if let Some(&p) = all[i].parent.as_ref().and_then(|p| pos.get(p)) {
                total[p] += total[i];
            }
        }
        let is_ipm_name = |n: &str| {
            ["IPM_SUBTREE", "Top of Personal Folders", "Top of Information Store", "Top of Outlook data file"]
                .iter()
                .any(|x| n.eq_ignore_ascii_case(x))
        };
        let ipm = f.ipm_subtree().and_then(|n| pos.get(&n).copied()).or_else(|| {
            (0..all.len()).filter(|&i| is_ipm_name(&all[i].name)).max_by_key(|&i| (total[i], std::cmp::Reverse(i)))
        });

        let in_subtree = |i: usize, root: usize| {
            let mut cur = Some(i);
            while let Some(c) = cur {
                if c == root {
                    return true;
                }
                cur = all[c].parent.as_ref().and_then(|p| pos.get(p)).copied();
            }
            false
        };
        let mut order: Vec<usize> = Vec::with_capacity(all.len());
        if let Some(r) = ipm {
            order.extend((0..all.len()).filter(|&i| in_subtree(i, r)));
        }
        order.extend((0..all.len()).filter(|&i| total[i] > 0 && ipm.is_none_or(|r| !in_subtree(i, r))));

        let store_name =
            self.file.display_name().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| self.display_name());
        Ok(order
            .into_iter()
            .map(|i| {
                let fi = &all[i];
                let is_root = Some(i) == ipm;
                Folder {
                    id: folder_id(fi.nid),
                    parent: if is_root { None } else { fi.parent.map(folder_id) },
                    name: if is_root { store_name.clone() } else { fi.name.clone() },
                    kind: fi.kind,
                    message_count: fi.content_count,
                    unread_count: fi.unread_count,
                }
            })
            .collect())
    }

    fn collect(
        &self,
        nid: u32,
        parent: Option<u32>,
        depth: usize,
        out: &mut Vec<FolderInfo>,
        seen: &mut std::collections::HashSet<u32>,
    ) {
        if depth > 64 || !seen.insert(nid) {
            return; // cycle guard
        }
        match self.file.folder_info(nid, parent) {
            Ok(i) => out.push(i),
            Err(e) => {
                tracing::warn!("folder 0x{nid:X} skipped: {e}");
                return;
            }
        }
        let children = self.file.child_folders(nid).unwrap_or_else(|e| {
            tracing::warn!("folder 0x{nid:X}: hierarchy table unreadable: {e}");
            Vec::new()
        });
        for c in children.into_iter().filter(|&c| nid_type(c) == NID_TYPE_NORMAL_FOLDER) {
            self.collect(c, Some(nid), depth + 1, out, seen);
        }
    }

    fn folder_index(&self, nid: u32) -> Result<Arc<FolderIndex>> {
        if let Some(ix) = self.indexes.lock().unwrap_or_else(|e| e.into_inner()).get(&nid) {
            return Ok(ix.clone());
        }
        let Some(table) = self.file.contents_table(nid)? else {
            return Err(Error::NotFound(format!("contents table of folder 0x{nid:X}")));
        };
        let date_cols: Vec<_> = [tags::MESSAGE_DELIVERY_TIME, tags::CLIENT_SUBMIT_TIME]
            .iter()
            .filter_map(|&id| table.column(id).copied())
            .collect();
        let size_col = table.column(tags::MESSAGE_SIZE).copied();
        // `row_count` is an unverified upper bound; only decoded rows are stored.
        let mut rows = Vec::with_capacity(table.row_count.min(1 << 16));
        table.for_each_row(&mut |i, r| {
            let date =
                date_cols.iter().find_map(|c| table.value(r, c, None)).and_then(|v| v.as_time()).unwrap_or(i64::MIN);
            let size = size_col.and_then(|c| table.value(r, &c, None)).and_then(|v| v.as_i64()).unwrap_or(0);
            rows.push(RowKey {
                row: i as u32,
                nid: table.row_id(r),
                date,
                size: size.clamp(0, u32::MAX as i64) as u32,
            });
        });
        let header_cols = self.file.header_columns(&table);
        let lacking = [ItemKind::Appointment, ItemKind::Contact, ItemKind::Task]
            .into_iter()
            .filter(|k| {
                let (guid, lid) = k.key_column();
                let id = self.file.named_id(&guid, &mapi_props::NamedId::Lid(lid));
                id.is_none_or(|id| table.column(id).is_none())
            })
            .collect();
        let ix = Arc::new(FolderIndex { table, header_cols, lacking, rows, perms: Mutex::new(Default::default()) });
        self.indexes.lock().unwrap_or_else(|e| e.into_inner()).insert(nid, ix.clone());
        Ok(ix)
    }

    fn permutation(&self, ix: &FolderIndex, key: SortKey) -> Arc<Vec<u32>> {
        let slot = match key {
            SortKey::Date => 0,
            SortKey::From => 1,
            SortKey::Subject => 2,
            SortKey::Size => 3,
            SortKey::Native => 4,
            SortKey::Start => 5,
        };
        if let Some(p) = &ix.perms.lock().unwrap_or_else(|e| e.into_inner())[slot] {
            return p.clone();
        }
        let n = ix.rows.len() as u32;
        let mut perm: Vec<u32> = (0..n).collect();
        match key {
            SortKey::Native => {}
            SortKey::Date => perm.sort_by_key(|&i| (ix.rows[i as usize].date, i)),
            SortKey::Start => {
                // Built from the same cells as the listed rows, so the key is their
                // `Header::start_ms`. Named columns are the item ones (and categories).
                let fixed =
                    [tags::MESSAGE_CLASS, tags::MESSAGE_DELIVERY_TIME, tags::CLIENT_SUBMIT_TIME, tags::CREATION_TIME];
                let cols: Vec<_> =
                    ix.header_cols.iter().filter(|c| c.id >= 0x8000 || fixed.contains(&c.id)).copied().collect();
                let mut keys = vec![i64::MIN; ix.rows.len()];
                Self::for_each_indexed_row(ix, &mut |j, r| {
                    let props = self.row_props(ix, r, ix.rows[j].nid, &cols);
                    let h = convert::header_from_mapi(&MapiMessage { props, ..Default::default() }, None, None);
                    keys[j] = h.start_ms().unwrap_or(i64::MIN);
                });
                perm.sort_by_key(|&i| (keys[i as usize], i));
            }
            SortKey::Size => perm.sort_by_key(|&i| (ix.rows[i as usize].size, i)),
            SortKey::From | SortKey::Subject => {
                let t = &ix.table;
                let cols: Vec<_> = if key == SortKey::From {
                    [tags::SENT_REPRESENTING_NAME, tags::SENDER_NAME]
                } else {
                    [tags::NORMALIZED_SUBJECT, tags::SUBJECT]
                }
                .iter()
                .filter_map(|&id| t.column(id).copied())
                .collect();
                let mut keys = vec![String::new(); ix.rows.len()];
                Self::for_each_indexed_row(ix, &mut |j, r| {
                    if let Some(s) = cols.iter().find_map(|c| t.value(r, c, None)).as_ref().and_then(|v| v.as_str()) {
                        keys[j] = sort_text(s, key == SortKey::Subject);
                    }
                });
                perm.sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]).then(a.cmp(&b)));
            }
        }
        let perm = Arc::new(perm);
        ix.perms.lock().unwrap_or_else(|e| e.into_inner())[slot] = Some(perm.clone());
        perm
    }

    /// Visit the rows of `ix.rows` (by index) in one block-wise scan of the table. Both
    /// `for_each_row` and `ix.rows` (built by it) ascend by row index, so a cursor matches
    /// them up.
    fn for_each_indexed_row(ix: &FolderIndex, f: &mut dyn FnMut(usize, &ltp::Row)) {
        let mut j = 0;
        ix.table.for_each_row(&mut |i, r| {
            while ix.rows.get(j).is_some_and(|rk| (rk.row as usize) < i) {
                j += 1;
            }
            if ix.rows.get(j).is_some_and(|rk| rk.row as usize == i) {
                f(j, r);
                j += 1;
            }
        });
    }

    /// Cells `cols` of a row. An item whose details the table lacks ([`FolderIndex::lacking`])
    /// is completed from its message node, so list rows agree with [`MailSource::message`].
    fn row_props(&self, ix: &FolderIndex, r: &ltp::Row, nid: u32, cols: &[ltp::Column]) -> PropMap {
        let mut props = self.file.row_props_in(&ix.table, r, cols, None);
        if ix.lacking.is_empty() {
            return props;
        }
        let class = mapi_props::get(&props, tags::MESSAGE_CLASS).and_then(|v| v.as_str());
        if !class.and_then(ItemKind::of_class).is_some_and(|k| ix.lacking.contains(&k)) {
            return props;
        }
        match self.file.node(nid).and_then(|n| self.file.props_without_bodies(&n)) {
            Ok(own) => {
                for (k, v) in own {
                    props.entry(k).or_insert(v);
                }
            }
            Err(e) => tracing::warn!("message 0x{nid:X}: item properties unreadable: {e}"),
        }
        props
    }

    fn row_header(&self, ix: &FolderIndex, rk: &RowKey, folder: &FolderId) -> Result<Header> {
        let r = ix.table.row(rk.row as usize)?;
        let props = self.row_props(ix, &r, rk.nid, &ix.header_cols);
        let m = MapiMessage { props, ..Default::default() };
        Ok(convert::header_from_mapi(&m, Some(Locator::Pst { nid: rk.nid }), Some(folder.clone())))
    }

    /// Resolve a locator to its message node and the folder holding the top-level message.
    fn resolve(&self, loc: &Locator) -> Result<(Node, Option<u32>)> {
        match loc {
            Locator::Pst { nid } => {
                let e = self.file.node_entry(*nid)?;
                let node = Node::load(&self.file.ndb, *nid, e.bid_data, e.bid_sub)?;
                Ok((node, (nid_type(e.parent) == NID_TYPE_NORMAL_FOLDER).then_some(e.parent)))
            }
            Locator::Embedded { parent, attachment } => {
                let (p, folder) = self.resolve(parent)?;
                Ok((self.file.embedded_node(&p, *attachment)?, folder))
            }
            other => Err(Error::Unsupported(format!("locator {other:?} does not belong to a PST/OST"))),
        }
    }
}

/// Case-insensitive sort text; for subjects, reply/forward prefixes are ignored.
fn sort_text(s: &str, subject: bool) -> String {
    let mut s = convert::strip_subject_marker(s).trim();
    if subject {
        loop {
            let starts = |p: &str| s.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
            let cut = ["re: ", "fw: ", "fwd: "].into_iter().find(|p| starts(p)).map(str::len);
            match cut {
                Some(n) => s = s[n..].trim_start(),
                None => break,
            }
        }
    }
    s.to_lowercase()
}

/// 64-bit FNV-1a, used for [`Fingerprint::head_hash`].
fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF2_9CE4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3))
}

impl MailSource for PstSource {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    /// The file name identifies an archive; the store's own display name is almost
    /// always the generic "Personal Folders" / "Outlook Data File".
    fn display_name(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|n| !n.trim().is_empty())
            .or_else(|| self.file.display_name())
            .unwrap_or_default()
    }

    fn fingerprint(&self) -> Result<Fingerprint> {
        let md = std::fs::metadata(&self.path)?;
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as i64);
        let bytes = self.file.ndb.bytes();
        Ok(Fingerprint {
            path: self.path.clone(),
            size: md.len(),
            mtime,
            head_hash: fnv1a(&bytes[..bytes.len().min(64 * 1024)]),
        })
    }

    fn folders(&self) -> Result<Vec<Folder>> {
        self.folders.get_or_init(|| self.build_folders().map_err(|e| e.to_string())).clone().map_err(Error::Format)
    }

    fn count(&self, folder: &FolderId) -> Result<u64> {
        let nid = parse_folder_id(folder)?;
        match self.folder_index(nid) {
            Ok(ix) => Ok(ix.rows.len() as u64),
            Err(Error::NotFound(_)) => Ok(0),
            Err(e) => Err(e),
        }
    }

    fn list(&self, folder: &FolderId, range: Range<u64>, sort: Sort) -> Result<Vec<Header>> {
        let nid = parse_folder_id(folder)?;
        let ix = match self.folder_index(nid) {
            Ok(ix) => ix,
            Err(Error::NotFound(_)) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let n = ix.rows.len();
        let start = (range.start as usize).min(n);
        let end = (range.end as usize).min(n);
        let perm = self.permutation(&ix, sort.key);
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        for i in start..end {
            let p = if sort.descending { perm[n - 1 - i] } else { perm[i] } as usize;
            match self.row_header(&ix, &ix.rows[p], folder) {
                Ok(h) => out.push(h),
                Err(e) => tracing::warn!("folder 0x{nid:X}: row {} skipped: {e}", ix.rows[p].row),
            }
        }
        Ok(out)
    }

    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_> {
        let folders = match self.folders() {
            Ok(f) => f,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(folders.into_iter().flat_map(move |f| {
            let ix = match parse_folder_id(&f.id).and_then(|nid| self.folder_index(nid)) {
                Ok(ix) => Some(ix),
                Err(Error::NotFound(_)) => None,
                Err(e) => {
                    tracing::warn!("folder {} skipped: {e}", f.id.0);
                    None
                }
            };
            let n = ix.as_ref().map_or(0, |ix| ix.rows.len());
            (0..n).filter_map(move |i| {
                let ix = ix.as_ref()?;
                match self.row_header(ix, &ix.rows[i], &f.id) {
                    Ok(h) => Some(Ok(h)),
                    Err(e) => {
                        tracing::warn!("folder {}: row {} skipped: {e}", f.id.0, ix.rows[i].row);
                        None
                    }
                }
            })
        }))
    }

    fn message(&self, loc: &Locator) -> Result<Message> {
        let (node, folder) = self.resolve(loc)?;
        let (m, sizes) = self.file.message(&node)?;
        let mut msg = convert::message_from_mapi(&m, Some(loc.clone()), folder.map(folder_id));
        for (meta, size) in msg.attachments.iter_mut().zip(sizes) {
            if let Some(s) = size {
                meta.size = s;
            }
        }
        Ok(msg)
    }

    fn attachment(&self, loc: &Locator, index: u32) -> Result<Vec<u8>> {
        let (node, _) = self.resolve(loc)?;
        self.file.attachment_data(&node, index)
    }

    fn raw_mime(&self, _loc: &Locator) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn properties(&self, loc: &Locator) -> Result<Vec<PropertyView>> {
        let (node, _) = self.resolve(loc)?;
        Ok(convert::property_views(&self.file.props(&node, true)?))
    }
}

struct Opener;

impl SourceOpener for Opener {
    fn sniff(&self, path: &Path) -> Option<SourceKind> {
        use std::io::Read;
        if !path.is_file() {
            return None;
        }
        let mut head = [0u8; 12];
        std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
        if &head[..4] != ndb::MAGIC {
            return None;
        }
        let ost = &head[8..10] == b"SO" || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ost"));
        Some(if ost { SourceKind::Ost } else { SourceKind::Pst })
    }

    fn open(&self, path: &Path) -> Result<Box<dyn MailSource>> {
        Ok(Box::new(PstSource::open(path)?))
    }
}

/// Entry point used by `stv-library`.
pub fn opener() -> &'static dyn SourceOpener {
    static OPENER: Opener = Opener;
    &OPENER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_id_roundtrip() {
        assert_eq!(folder_id(0x8022).0, "nid:8022");
        assert_eq!(parse_folder_id(&folder_id(0x8022)).unwrap(), 0x8022);
        assert!(parse_folder_id(&FolderId("Inbox".into())).is_err());
    }

    #[test]
    fn subject_sort_text() {
        assert_eq!(sort_text("RE: FW: Hello", true), "hello");
        assert_eq!(sort_text("\u{1}\u{5}Fwd: X", true), "x");
        assert_eq!(sort_text("Re: x", false), "re: x");
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a(b""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xAF63_DC4C_8601_EC8C);
    }
}
