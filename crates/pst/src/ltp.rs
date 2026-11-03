//! Lists, Tables and Properties layer ([MS-PST] 2.3): heap-on-node (HN), BTree-on-heap
//! (BTH), property context (PC) and table context (TC, plus the OST "AC" variant).

use std::collections::HashSet;
use std::sync::Arc;

use mail_model::{Error, Result};
use mapi_props::{codepage, filetime_to_unix_ms, PropType, PropValue};

use crate::ndb::{Block, Ndb, Slice, SubnodeEntry};

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

pub const HN_SIG: u8 = 0xEC;
pub const CLIENT_SIG_PC: u8 = 0xBC;
pub const CLIENT_SIG_TC: u8 = 0x7C;
pub const CLIENT_SIG_AC: u8 = 0xAC;
pub const CLIENT_SIG_BTH: u8 = 0xB5;

/// A node (top-level NBT node or subnode): its data tree plus resolved subnodes.
#[derive(Clone)]
pub struct Node {
    pub nid: u32,
    pub bid_data: u64,
    pub subnodes: Arc<Vec<SubnodeEntry>>,
}

impl Node {
    pub fn load(ndb: &Ndb, nid: u32, bid_data: u64, bid_sub: u64) -> Result<Node> {
        Ok(Node { nid, bid_data, subnodes: Arc::new(ndb.subnodes(bid_sub)?) })
    }

    pub fn subnode(&self, nid: u32) -> Option<&SubnodeEntry> {
        self.subnodes.binary_search_by_key(&nid, |e| e.nid).ok().map(|i| &self.subnodes[i])
    }

    /// Open a subnode as a node of its own (its nested subnodes resolved).
    pub fn child(&self, ndb: &Ndb, nid: u32) -> Result<Node> {
        let e =
            self.subnode(nid).ok_or_else(|| Error::NotFound(format!("subnode 0x{nid:X} of node 0x{:X}", self.nid)))?;
        Node::load(ndb, e.nid, e.bid_data, e.bid_sub)
    }
}

/// Heap-on-node. Blocks are fetched lazily through the NDB block cache.
#[derive(Clone)]
pub struct Heap {
    ndb: Arc<Ndb>,
    bids: Arc<Vec<u64>>,
    first: Block,
    pub client_sig: u8,
    pub user_root: u32,
}

impl Heap {
    pub fn open(ndb: &Arc<Ndb>, node: &Node) -> Result<Heap> {
        if node.bid_data == 0 {
            return Err(Error::Format(format!("node 0x{:X} has no data", node.nid)));
        }
        let bids = ndb.data_tree(node.bid_data)?;
        let first_bid = *bids.first().ok_or_else(|| Error::Format(format!("node 0x{:X} has no data", node.nid)))?;
        let first = ndb.block(first_bid)?;
        if first.len() < 12 || first[2] != HN_SIG {
            return Err(Error::Format(format!("node 0x{:X}: not a heap (bad HNHDR signature)", node.nid)));
        }
        Ok(Heap { ndb: ndb.clone(), client_sig: first[3], user_root: u32_at(&first, 4), bids: Arc::new(bids), first })
    }

    fn block(&self, i: usize) -> Result<Block> {
        if i == 0 {
            return Ok(self.first.clone());
        }
        let bid = *self.bids.get(i).ok_or_else(|| Error::Format(format!("HID block index {i} out of range")))?;
        self.ndb.block(bid)
    }

    /// Bytes of heap allocation `hid`.
    pub fn get(&self, hid: u32) -> Result<Slice> {
        if hid & 0x1F != 0 {
            return Err(Error::Format(format!("0x{hid:X} is not a HID")));
        }
        let (bi, idx) = self.ndb.format().hid_parts(hid);
        if idx == 0 {
            return Err(Error::Format(format!("HID 0x{hid:X} has index 0")));
        }
        let b = self.block(bi)?;
        let bad = || Error::Format(format!("HID 0x{hid:X}: corrupt heap page map"));
        if b.len() < 2 {
            return Err(bad());
        }
        let pm = u16_at(&b, 0) as usize;
        if pm + 4 > b.len() {
            return Err(bad());
        }
        let c_alloc = u16_at(&b, pm) as usize;
        if idx > c_alloc || pm + 4 + (c_alloc + 1) * 2 > b.len() {
            return Err(bad());
        }
        let start = u16_at(&b, pm + 4 + (idx - 1) * 2) as usize;
        let end = u16_at(&b, pm + 4 + idx * 2) as usize;
        if start > end || end > pm {
            return Err(bad());
        }
        Ok(Slice::new(b, start..end))
    }
}

/// Resolve an HNID (heap allocation or subnode) to its bytes. 0 → empty.
pub fn read_hnid(heap: &Heap, node: &Node, hnid: u32) -> Result<Slice> {
    if hnid == 0 {
        Ok(Slice::empty())
    } else if hnid & 0x1F == 0 {
        heap.get(hnid)
    } else {
        let e = node.subnode(hnid).ok_or_else(|| Error::Format(format!("missing subnode 0x{hnid:X}")))?;
        Ok(Slice::whole(heap.ndb.read_data(e.bid_data)?))
    }
}

/// BTree-on-heap header. [MS-PST] 2.3.2.1
#[derive(Clone, Copy, Debug)]
pub struct Bth {
    pub key_size: usize,
    pub data_size: usize,
    levels: u8,
    root: u32,
}

impl Bth {
    pub fn open(heap: &Heap, hid: u32) -> Result<Bth> {
        let h = heap.get(hid)?;
        if h.len() < 8 || h[0] != CLIENT_SIG_BTH {
            return Err(Error::Format("bad BTH header".into()));
        }
        let key_size = h[1] as usize;
        if !matches!(key_size, 2 | 4 | 8 | 16) || h[2] as usize > 32 {
            return Err(Error::Format(format!("bad BTH sizes key={key_size} data={}", h[2])));
        }
        Ok(Bth { key_size, data_size: h[2] as usize, levels: h[3], root: u32_at(&h, 4) })
    }

    /// Visit every leaf record in key order. Each BTH page is visited at most once, so a
    /// corrupt index cannot loop or multiply its records.
    pub fn walk(&self, heap: &Heap, f: &mut dyn FnMut(&[u8], &[u8])) -> Result<()> {
        if self.root == 0 {
            return Ok(());
        }
        if self.levels > 8 {
            return Err(Error::Format("BTH deeper than 8 levels".into()));
        }
        self.walk_level(heap, self.root, self.levels, &mut HashSet::new(), f)
    }

    fn walk_level(
        &self,
        heap: &Heap,
        hid: u32,
        level: u8,
        seen: &mut HashSet<u32>,
        f: &mut dyn FnMut(&[u8], &[u8]),
    ) -> Result<()> {
        // Single-page BTHs (the common case) cannot repeat a page; skip the bookkeeping.
        if self.levels > 0 && !seen.insert(hid) {
            return Err(Error::Format(format!("BTH page 0x{hid:X} referenced more than once")));
        }
        let b = heap.get(hid)?;
        let k = self.key_size;
        if level == 0 {
            for r in b.chunks_exact(k + self.data_size) {
                f(&r[..k], &r[k..]);
            }
        } else {
            for r in b.chunks_exact(k + 4) {
                self.walk_level(heap, u32_at(r, k), level - 1, seen, f)?;
            }
        }
        Ok(())
    }
}

/// One raw property from a PC: fixed values ≤ 4 bytes inline, everything else resolved bytes.
pub struct RawProp {
    pub id: u16,
    pub ty: u16,
    pub inline: [u8; 4],
    pub bytes: Option<Slice>,
    /// Size of a value deliberately left unread by `skip`, when it could be determined.
    pub skipped_size: Option<u64>,
}

/// Size of an HNID's value without reading it (heap allocation length or data tree size).
pub fn hnid_size(heap: &Heap, node: &Node, hnid: u32) -> Result<u64> {
    if hnid == 0 {
        Ok(0)
    } else if hnid & 0x1F == 0 {
        Ok(heap.get(hnid)?.len() as u64)
    } else {
        let e = node.subnode(hnid).ok_or_else(|| Error::Format(format!("missing subnode 0x{hnid:X}")))?;
        heap.ndb.data_size(e.bid_data)
    }
}

/// Read a property context. `skip(id, ty)` returning true leaves `bytes` unresolved (lazy
/// large values such as attachment data); only their size is recorded.
pub fn read_pc(ndb: &Arc<Ndb>, node: &Node, skip: &dyn Fn(u16, u16) -> bool) -> Result<Vec<RawProp>> {
    let heap = Heap::open(ndb, node)?;
    if heap.client_sig != CLIENT_SIG_PC {
        return Err(Error::Format(format!(
            "node 0x{:X}: not a property context (sig 0x{:02X})",
            node.nid, heap.client_sig
        )));
    }
    let bth = Bth::open(&heap, heap.user_root)?;
    if bth.key_size != 2 || bth.data_size != 6 {
        return Err(Error::Format("property context BTH has wrong record size".into()));
    }
    let mut recs = Vec::new();
    bth.walk(&heap, &mut |k, d| recs.push((u16_at(k, 0), u16_at(d, 0), [d[2], d[3], d[4], d[5]])))?;
    let mut out = Vec::with_capacity(recs.len());
    for (id, ty, inline) in recs {
        let small = PropType::from_u16(ty).and_then(PropType::fixed_size).is_some_and(|n| n <= 4);
        let hnid = u32::from_le_bytes(inline);
        let (bytes, skipped_size) = if small {
            (None, None)
        } else if skip(id, ty) {
            (None, hnid_size(&heap, node, hnid).ok())
        } else {
            match read_hnid(&heap, node, hnid) {
                Ok(b) => (Some(b), None),
                Err(e) => {
                    tracing::warn!("node 0x{:X}: property 0x{id:04X}{ty:04X} unreadable: {e}", node.nid);
                    continue;
                }
            }
        };
        out.push(RawProp { id, ty, inline, bytes, skipped_size });
    }
    Ok(out)
}

/// Decode one value. `b` holds the value bytes (inline bytes for small fixed types).
/// Returns `None` for malformed values and for `PtypObject` (container-specific).
pub fn decode_value(ty: PropType, b: &[u8], cp: Option<u32>) -> Option<PropValue> {
    fn arr<const N: usize>(b: &[u8]) -> Option<[u8; N]> {
        b.get(..N)?.try_into().ok()
    }
    fn multi_fixed<T, const N: usize>(b: &[u8], f: impl Fn([u8; N]) -> T) -> Vec<T> {
        b.as_chunks::<N>().0.iter().map(|&c| f(c)).collect()
    }
    fn multi_var(b: &[u8]) -> Option<Vec<&[u8]>> {
        if b.is_empty() {
            return Some(Vec::new());
        }
        let n = u32_at(b.get(..4)?, 0) as usize;
        if n > b.len() / 4 {
            return None;
        }
        let offs: Vec<usize> =
            (0..n).map(|i| b.get(4 + i * 4..8 + i * 4).map(|s| u32_at(s, 0) as usize)).collect::<Option<_>>()?;
        (0..n)
            .map(|i| {
                let end = if i + 1 < n { offs[i + 1] } else { b.len() };
                b.get(offs[i]..end)
            })
            .collect()
    }
    use PropType as T;
    Some(match ty {
        T::Null => PropValue::Null,
        T::Int16 => PropValue::Int16(i16::from_le_bytes(arr(b)?)),
        T::Int32 => PropValue::Int32(i32::from_le_bytes(arr(b)?)),
        T::Float32 => PropValue::Float32(f32::from_le_bytes(arr(b)?)),
        T::ErrorCode => PropValue::ErrorCode(u32::from_le_bytes(arr(b)?)),
        T::Boolean => PropValue::Boolean(*b.first()? != 0),
        T::Float64 => PropValue::Float64(f64::from_le_bytes(arr(b)?)),
        T::AppTime => PropValue::AppTime(f64::from_le_bytes(arr(b)?)),
        T::Currency => PropValue::Currency(i64::from_le_bytes(arr(b)?)),
        T::Int64 => PropValue::Int64(i64::from_le_bytes(arr(b)?)),
        T::SysTime => PropValue::SysTime(filetime_to_unix_ms(u64::from_le_bytes(arr(b)?))),
        T::Guid => PropValue::Guid(arr(b)?),
        T::Unicode => PropValue::String(codepage::decode_utf16le(b)),
        T::String8 => PropValue::String(codepage::decode_string8(b, cp)),
        T::Binary | T::ServerId | T::Restriction | T::RuleAction => PropValue::Binary(b.to_vec()),
        T::Object => return None,
        T::MultiInt16 => PropValue::MultiInt16(multi_fixed(b, i16::from_le_bytes)),
        T::MultiInt32 => PropValue::MultiInt32(multi_fixed(b, i32::from_le_bytes)),
        T::MultiFloat32 => PropValue::MultiFloat32(multi_fixed(b, f32::from_le_bytes)),
        T::MultiFloat64 => PropValue::MultiFloat64(multi_fixed(b, f64::from_le_bytes)),
        T::MultiAppTime => PropValue::MultiAppTime(multi_fixed(b, f64::from_le_bytes)),
        T::MultiCurrency => PropValue::MultiCurrency(multi_fixed(b, i64::from_le_bytes)),
        T::MultiInt64 => PropValue::MultiInt64(multi_fixed(b, i64::from_le_bytes)),
        T::MultiSysTime => {
            PropValue::MultiSysTime(multi_fixed(b, |a: [u8; 8]| filetime_to_unix_ms(u64::from_le_bytes(a))))
        }
        T::MultiGuid => PropValue::MultiGuid(multi_fixed(b, |a: [u8; 16]| a)),
        T::MultiUnicode => PropValue::MultiString(multi_var(b)?.into_iter().map(codepage::decode_utf16le).collect()),
        T::MultiString8 => {
            PropValue::MultiString(multi_var(b)?.into_iter().map(|s| codepage::decode_string8(s, cp)).collect())
        }
        T::MultiBinary => PropValue::MultiBinary(multi_var(b)?.into_iter().map(<[u8]>::to_vec).collect()),
    })
}

/// Column descriptor of a TC/AC table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    pub id: u16,
    pub ty: u16,
    pub ib: usize,
    pub cb: usize,
    pub bit: usize,
}

enum Rows {
    None,
    Heap(Slice),
    Blocks { bids: Vec<u64>, per_block: usize },
}

/// Table context: column descriptors + row matrix. Rows are read on demand.
pub struct Table {
    ndb: Arc<Ndb>,
    heap: Heap,
    node: Node,
    pub columns: Vec<Column>,
    row_size: usize,
    ceb_offset: usize,
    /// HIDs of the table's own structures (TCINFO, row index, row matrix). Some writers leave
    /// such stale HIDs in string cells whose CEB bit is set; they are treated as absent.
    structural: [u32; 3],
    rows: Rows,
    /// Rows addressable through [`Table::row`]. For row matrices spanning several blocks this
    /// is derived from block counts without reading them, so a damaged file can overstate it:
    /// iterate with [`Table::for_each_row`] and never allocate per claimed row.
    pub row_count: usize,
}

/// One row of a [`Table`].
pub struct Row(Slice);

impl Row {
    /// Raw row bytes (fixed columns, HNIDs and the cell-existence bitmap).
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Table {
    pub fn open(ndb: &Arc<Ndb>, node: Node) -> Result<Table> {
        let heap = Heap::open(ndb, &node)?;
        let info = heap.get(heap.user_root)?;
        let (columns, rgib, hnid_rows) = match heap.client_sig {
            CLIENT_SIG_TC => {
                if info.len() < 22 || info[0] != CLIENT_SIG_TC {
                    return Err(Error::Format("bad TCINFO".into()));
                }
                let n = info[1] as usize;
                if info.len() < 22 + n * 8 {
                    return Err(Error::Format("TCINFO column array truncated".into()));
                }
                let cols = (0..n)
                    .map(|i| {
                        let o = 22 + i * 8;
                        let tag = u32_at(&info, o);
                        Column {
                            id: (tag >> 16) as u16,
                            ty: tag as u16,
                            ib: u16_at(&info, o + 4) as usize,
                            cb: info[o + 6] as usize,
                            bit: info[o + 7] as usize,
                        }
                    })
                    .collect::<Vec<_>>();
                let rgib = [u16_at(&info, 2), u16_at(&info, 4), u16_at(&info, 6), u16_at(&info, 8)];
                (cols, rgib, u32_at(&info, 14))
            }
            CLIENT_SIG_AC => {
                // libpff "ac table": 40-byte header, 16-byte column definitions in a separate allocation.
                if info.len() < 28 || info[0] != CLIENT_SIG_AC {
                    return Err(Error::Format("bad AC table header".into()));
                }
                let n = u16_at(&info, 22) as usize;
                let defs = heap.get(u32_at(&info, 24))?;
                if defs.len() < n * 16 {
                    return Err(Error::Format("AC column definitions truncated".into()));
                }
                let cols = (0..n)
                    .filter_map(|i| {
                        let o = i * 16;
                        // Columns stored through a secondary "a5" table are not supported; skip them.
                        (u32_at(&defs, o + 12) == 0).then(|| Column {
                            ty: u16_at(&defs, o),
                            id: u16_at(&defs, o + 2),
                            ib: u16_at(&defs, o + 4) as usize,
                            cb: u16_at(&defs, o + 6) as usize,
                            bit: u16_at(&defs, o + 8) as usize,
                        })
                    })
                    .collect::<Vec<_>>();
                let rgib = [u16_at(&info, 2), u16_at(&info, 4), u16_at(&info, 6), u16_at(&info, 8)];
                (cols, rgib, u32_at(&info, 14))
            }
            s => return Err(Error::Format(format!("node 0x{:X}: not a table (sig 0x{s:02X})", node.nid))),
        };
        let row_size = rgib[3] as usize;
        let ceb_offset = rgib[2] as usize;
        let ceb_len = columns.iter().map(|c| c.bit / 8 + 1).max().unwrap_or(0);
        // Every row starts with the 4-byte dwRowID ([`Table::row_id`]).
        let too_small = row_size != 0 && row_size < 4;
        if too_small || columns.iter().any(|c| c.ib + c.cb > row_size) || ceb_offset + ceb_len > row_size {
            return Err(Error::Format(format!("node 0x{:X}: column layout exceeds row size {row_size}", node.nid)));
        }
        let (rows, row_count) = if hnid_rows == 0 || row_size == 0 {
            (Rows::None, 0)
        } else if hnid_rows & 0x1F == 0 {
            let s = heap.get(hnid_rows)?;
            let n = s.len() / row_size;
            (Rows::Heap(s), n)
        } else {
            let e = node
                .subnode(hnid_rows)
                .ok_or_else(|| Error::Format(format!("row matrix subnode 0x{hnid_rows:X} missing")))?;
            let bids = ndb.data_tree(e.bid_data)?;
            let (Some(&first_bid), Some(&last_bid)) = (bids.first(), bids.last()) else {
                return Err(Error::Format(format!("row matrix subnode 0x{hnid_rows:X} has no data")));
            };
            // Rows never straddle blocks; every block but the last is full. [MS-PST] 2.3.4.4
            let first = ndb.block(first_bid)?.len();
            let per_block = (first / row_size).max(1);
            let last = if bids.len() > 1 { ndb.block(last_bid)?.len() } else { first };
            let n = (bids.len() - 1) * per_block + (last / row_size).min(per_block);
            (Rows::Blocks { bids, per_block }, n)
        };
        let structural = [heap.user_root, u32_at(&info, 10), hnid_rows];
        Ok(Table { ndb: ndb.clone(), heap, node, columns, row_size, ceb_offset, structural, rows, row_count })
    }

    pub fn column(&self, id: u16) -> Option<&Column> {
        self.columns.iter().find(|c| c.id == id)
    }

    /// Visit every row in order, fetching each row-matrix block once. Rows of an unreadable
    /// block are skipped (logged); the callback receives the row index.
    pub fn for_each_row(&self, f: &mut dyn FnMut(usize, &Row)) {
        let rs = self.row_size;
        match &self.rows {
            Rows::None => {}
            Rows::Heap(s) => {
                for i in 0..self.row_count {
                    f(i, &Row(s.sub(i * rs..(i + 1) * rs)));
                }
            }
            Rows::Blocks { bids, per_block } => {
                for (bi, &bid) in bids.iter().enumerate() {
                    let first = bi * per_block;
                    if first >= self.row_count {
                        break;
                    }
                    let b = match self.ndb.block(bid) {
                        Ok(b) => Slice::whole(b),
                        Err(e) => {
                            tracing::warn!("table 0x{:X}: row block {bi} unreadable: {e}", self.node.nid);
                            continue;
                        }
                    };
                    let n = (*per_block).min(self.row_count - first).min(b.len() / rs);
                    for j in 0..n {
                        f(first + j, &Row(b.sub(j * rs..(j + 1) * rs)));
                    }
                }
            }
        }
    }

    pub fn row(&self, i: usize) -> Result<Row> {
        if i >= self.row_count {
            return Err(Error::NotFound(format!("row {i} of {}", self.row_count)));
        }
        let rs = self.row_size;
        match &self.rows {
            Rows::None => Err(Error::NotFound("empty table".into())),
            Rows::Heap(s) => Ok(Row(s.sub(i * rs..(i + 1) * rs))),
            Rows::Blocks { bids, per_block } => {
                let b = self.ndb.block(bids[i / per_block])?;
                let o = (i % per_block) * rs;
                if o + rs > b.len() {
                    return Err(Error::Format(format!("row {i} beyond its row-matrix block")));
                }
                Ok(Row(Slice::new(b, o..o + rs)))
            }
        }
    }

    fn exists(&self, row: &Row, c: &Column) -> bool {
        let byte = self.ceb_offset + c.bit / 8;
        row.0.get(byte).is_some_and(|b| b & (0x80 >> (c.bit % 8)) != 0)
    }

    /// Raw cell: inline bytes for fixed types ≤ 8 bytes, otherwise the HNID-resolved bytes.
    /// `None` when the cell is absent (CEB bit clear).
    pub fn cell_bytes(&self, row: &Row, c: &Column) -> Result<Option<Slice>> {
        if !self.exists(row, c) {
            return Ok(None);
        }
        let raw = &row.0[c.ib..c.ib + c.cb];
        let inline = PropType::from_u16(c.ty).and_then(PropType::fixed_size).is_some_and(|n| n <= 8);
        if inline {
            return Ok(Some(row.0.sub(c.ib..c.ib + c.cb)));
        }
        if raw.len() < 4 {
            return Err(Error::Format("variable-size table cell narrower than an HNID".into()));
        }
        let hnid = u32_at(raw, 0);
        if hnid & 0x1F == 0 && self.structural.contains(&hnid) {
            return Ok(None);
        }
        read_hnid(&self.heap, &self.node, hnid).map(Some)
    }

    /// Decoded cell value (`None` if absent, unknown type or malformed).
    pub fn value(&self, row: &Row, c: &Column, cp: Option<u32>) -> Option<PropValue> {
        let ty = PropType::from_u16(c.ty)?;
        match self.cell_bytes(row, c) {
            Ok(Some(b)) => decode_value(ty, &b, cp),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!("table 0x{:X}: column 0x{:04X}: {e}", self.node.nid, c.id);
                None
            }
        }
    }

    /// `dwRowID` (column `PidTagLtpRowId`, always at offset 0).
    pub fn row_id(&self, row: &Row) -> u32 {
        u32_at(&row.0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_scalar_and_multi_values() {
        assert_eq!(decode_value(PropType::Int32, &[1, 0, 0, 0], None), Some(PropValue::Int32(1)));
        assert_eq!(decode_value(PropType::Boolean, &[1, 0, 0, 0], None), Some(PropValue::Boolean(true)));
        assert_eq!(decode_value(PropType::Int32, &[1, 0], None), None, "truncated");
        let ft = mapi_props::unix_ms_to_filetime(86_400_000).to_le_bytes();
        assert_eq!(decode_value(PropType::SysTime, &ft, None), Some(PropValue::SysTime(86_400_000)));
        assert_eq!(decode_value(PropType::Unicode, b"h\0i\0", None), Some(PropValue::String("hi".into())));
        assert_eq!(decode_value(PropType::String8, b"\xe9", Some(1252)), Some(PropValue::String("é".into())));
        // MultiUnicode: count, offsets, data.
        let mut mv = Vec::new();
        mv.extend_from_slice(&2u32.to_le_bytes());
        mv.extend_from_slice(&12u32.to_le_bytes());
        mv.extend_from_slice(&16u32.to_le_bytes());
        mv.extend_from_slice(b"a\0b\0c\0");
        assert_eq!(
            decode_value(PropType::MultiUnicode, &mv, None),
            Some(PropValue::MultiString(vec!["ab".into(), "c".into()]))
        );
        // Offsets past the end → malformed, not a panic.
        mv[4] = 200;
        assert_eq!(decode_value(PropType::MultiUnicode, &mv, None), None);
        assert_eq!(
            decode_value(PropType::MultiInt32, &[1, 0, 0, 0, 2, 0, 0, 0], None),
            Some(PropValue::MultiInt32(vec![1, 2]))
        );
        let mut huge = vec![0xFF; 8];
        huge[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode_value(PropType::MultiBinary, &huge, None), None);
    }
}
