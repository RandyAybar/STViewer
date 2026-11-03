//! Node Database layer ([MS-PST] 2.2): header, NBT/BBT B-trees, blocks (decoding,
//! 4K-format zlib inflation), data trees (XBLOCK/XXBLOCK) and subnode trees (SLBLOCK/SIBLOCK).
//!
//! Everything reads straight from an mmap; only blocks that need decoding (encrypted or
//! compressed) or concatenation are copied, and those copies live in a small LRU.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::ops::{Deref, Range};
use std::sync::{Arc, Mutex, OnceLock};

use mail_model::{Error, Result};

use crate::crypt;

/// Bytes backing a PST: an mmap of the file, or an owned buffer (tests, in-memory sources).
pub enum Backing {
    Map(memmap2::Mmap),
    Vec(Vec<u8>),
}

impl Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Backing::Map(m) => m,
            Backing::Vec(v) => v,
        }
    }
}

/// On-disk layout generation, from the header's `wVer`. [MS-PST] 2.2.2.6
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// wVer 14/15: 32-bit ids, 512-byte pages.
    Ansi,
    /// wVer 21/23: 64-bit ids, 512-byte pages.
    Unicode,
    /// wVer 36: 64-bit ids, 4096-byte pages, blocks up to 64 KiB, optionally zlib-compressed.
    Unicode4k,
}

impl Format {
    /// Size of a B-tree page (NBT/BBT/AMap).
    pub fn page_size(self) -> usize {
        match self {
            Format::Unicode4k => 4096,
            _ => 512,
        }
    }
    /// Size of BIDs / NIDs inside B-tree pages, subnode and XBLOCK entries.
    pub fn id_size(self) -> usize {
        match self {
            Format::Ansi => 4,
            _ => 8,
        }
    }
    /// Split a HID into (block index, allocation index). [MS-PST] 2.3.1.1; 4K: libpff
    pub fn hid_parts(self, hid: u32) -> (usize, usize) {
        match self {
            Format::Unicode4k => ((hid >> 19) as usize, ((hid >> 5) & 0x3FFF) as usize),
            _ => ((hid >> 16) as usize, ((hid >> 5) & 0x7FF) as usize),
        }
    }
    /// Build a HID (inverse of [`Format::hid_parts`]).
    pub fn make_hid(self, block: usize, index: usize) -> u32 {
        match self {
            Format::Unicode4k => ((block as u32) << 19) | ((index as u32) << 5),
            _ => ((block as u32) << 16) | ((index as u32) << 5),
        }
    }
    /// (entries area size, page-trailer offset) of a B-tree page.
    fn btpage_layout(self) -> (usize, usize) {
        match self {
            Format::Ansi => (496, 500),
            Format::Unicode => (488, 496),
            Format::Unicode4k => (4056, 4072),
        }
    }
    /// Size of the trailer after each data block.
    pub fn block_trailer_size(self) -> usize {
        match self {
            Format::Ansi => 12,
            Format::Unicode => 16,
            Format::Unicode4k => 24,
        }
    }
    /// Allocation granularity of data blocks.
    pub fn block_align(self) -> usize {
        match self {
            Format::Unicode4k => 512,
            _ => 64,
        }
    }
    /// Bytes on disk taken by a block whose data is `cb` bytes (data + padding + trailer).
    pub fn block_alloc_size(self, cb: usize) -> usize {
        let a = self.block_align();
        let mut size = cb.div_ceil(a) * a;
        if size - cb < self.block_trailer_size() {
            size += a;
        }
        size
    }
}

/// `ptype` of a Block B-tree page.
pub const PTYPE_BBT: u8 = 0x80;
/// `ptype` of a Node B-tree page.
pub const PTYPE_NBT: u8 = 0x81;

/// File header fields the reader uses or reports. [MS-PST] 2.2.2.6
#[derive(Clone, Debug)]
pub struct Header {
    pub format: Format,
    /// `wVer` (14/15 ANSI, 21/23 Unicode, 36 Unicode-4K).
    pub version: u16,
    /// `wVerClient`.
    pub client_version: u16,
    /// `wMagicClient`: `SM` (PST), `SO` (OST), `AB` (PAB).
    pub client: [u8; 2],
    /// `bCryptMethod` (see [`crypt`]).
    pub crypt: u8,
    /// `BREFNBT`: block id and file offset of the Node B-tree root page.
    pub nbt_bid: u64,
    pub nbt_ib: u64,
    /// `BREFBBT`: block id and file offset of the Block B-tree root page.
    pub bbt_bid: u64,
    pub bbt_ib: u64,
    /// `ibFileEof` as declared (not validated against the real size).
    pub file_eof: u64,
    /// `dwCRCPartial` as stored / as computed over bytes 8..479.
    pub crc_partial: (u32, u32),
}

impl Header {
    /// `wMagicClient` is `SO` (offline store).
    pub fn is_ost(&self) -> bool {
        &self.client == b"SO"
    }
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
fn u64_at(d: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
}
/// Little-endian unsigned of `size` (4 or 8) bytes.
pub(crate) fn uint_at(d: &[u8], o: usize, size: usize) -> u64 {
    if size == 4 {
        u32_at(d, o) as u64
    } else {
        u64_at(d, o)
    }
}

pub const MAGIC: &[u8; 4] = b"!BDN";

/// Parse the file header. [MS-PST] 2.2.2.6
pub fn parse_header(d: &[u8]) -> Result<Header> {
    if d.len() < 512 || &d[..4] != MAGIC {
        return Err(Error::Format("not a PST/OST file (missing !BDN magic)".into()));
    }
    let version = u16_at(d, 10);
    let format = match version {
        14 | 15 => Format::Ansi,
        21 | 23 => Format::Unicode,
        36 => Format::Unicode4k,
        v => return Err(Error::Unsupported(format!("PST/OST format version {v}"))),
    };
    let (crypt, nbt_bid, nbt_ib, bbt_bid, bbt_ib, eof) = match format {
        Format::Ansi => (
            d[461],
            u32_at(d, 184) as u64,
            u32_at(d, 188) as u64,
            u32_at(d, 192) as u64,
            u32_at(d, 196) as u64,
            u32_at(d, 168) as u64,
        ),
        _ => {
            if d.len() < 564 {
                return Err(Error::Format("Unicode PST header truncated".into()));
            }
            (d[513], u64_at(d, 216), u64_at(d, 224), u64_at(d, 232), u64_at(d, 240), u64_at(d, 184))
        }
    };
    Ok(Header {
        format,
        version,
        client_version: u16_at(d, 12),
        client: [d[8], d[9]],
        crypt,
        nbt_bid,
        nbt_ib,
        bbt_bid,
        bbt_ib,
        file_eof: eof,
        crc_partial: (u32_at(d, 4), crypt::crc(&d[8..8 + 471])),
    })
}

/// A decoded block: either a window of the mmap or an owned (decoded/inflated/concatenated) buffer.
#[derive(Clone)]
pub struct Block(Repr);

#[derive(Clone)]
enum Repr {
    Mapped(Arc<Backing>, Range<usize>),
    /// `Arc<Vec>` rather than `Arc<[u8]>`: wrapping a freshly decoded buffer must not copy it.
    Owned(Arc<Vec<u8>>),
}

impl Block {
    pub fn from_vec(v: Vec<u8>) -> Block {
        Block(Repr::Owned(Arc::new(v)))
    }
    pub fn empty() -> Block {
        Block::from_vec(Vec::new())
    }
    /// Approximate memory held, for the block cache budget.
    fn cost(&self) -> usize {
        match &self.0 {
            Repr::Mapped(..) => 64,
            Repr::Owned(b) => b.capacity() + 64,
        }
    }
}

impl Deref for Block {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.0 {
            Repr::Mapped(b, r) => &b[r.clone()],
            Repr::Owned(v) => v,
        }
    }
}

/// A sub-range of a [`Block`] (heap allocation, property value, table row).
#[derive(Clone)]
pub struct Slice {
    block: Block,
    range: Range<usize>,
}

impl Slice {
    pub fn new(block: Block, range: Range<usize>) -> Slice {
        debug_assert!(range.end <= block.len());
        Slice { block, range }
    }
    pub fn whole(block: Block) -> Slice {
        let n = block.len();
        Slice { block, range: 0..n }
    }
    pub fn empty() -> Slice {
        Slice::whole(Block::empty())
    }
    /// Sub-range relative to this slice (caller guarantees `r.end <= self.len()`).
    pub fn sub(&self, r: Range<usize>) -> Slice {
        debug_assert!(r.end <= self.range.len());
        Slice { block: self.block.clone(), range: self.range.start + r.start..self.range.start + r.end }
    }
}

impl Deref for Slice {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.block[self.range.clone()]
    }
}

/// Leaf entry of the Node B-tree. [MS-PST] 2.2.2.7.7.4
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeEntry {
    pub nid: u32,
    pub bid_data: u64,
    pub bid_sub: u64,
    pub parent: u32,
}

/// Leaf entry of the Block B-tree. [MS-PST] 2.2.2.7.7.3
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEntry {
    pub bid: u64,
    pub ib: u64,
    pub cb: u16,
    pub refs: u16,
}

/// Entry of a subnode tree (SLENTRY). [MS-PST] 2.2.2.8.3.3.1.1
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubnodeEntry {
    pub nid: u32,
    pub bid_data: u64,
    pub bid_sub: u64,
}

struct PageView<'a> {
    entries: &'a [u8],
    count: usize,
    size: usize,
    level: u8,
}

impl<'a> PageView<'a> {
    fn entry(&self, i: usize) -> &'a [u8] {
        &self.entries[i * self.size..(i + 1) * self.size]
    }
}

/// LRU of decoded blocks, bounded by approximate byte cost.
struct BlockCache {
    map: HashMap<u64, (Block, u64)>,
    tick: u64,
    bytes: usize,
    budget: usize,
}

impl BlockCache {
    fn get(&mut self, bid: u64) -> Option<Block> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(&bid).map(|e| {
            e.1 = tick;
            e.0.clone()
        })
    }

    fn insert(&mut self, bid: u64, b: Block) {
        self.tick += 1;
        self.bytes += b.cost();
        if let Some((old, _)) = self.map.insert(bid, (b, self.tick)) {
            self.bytes -= old.cost();
        }
        if self.bytes > self.budget {
            // Evict the oldest quarter in one pass; amortised O(1) per insert.
            let mut ticks: Vec<u64> = self.map.values().map(|e| e.1).collect();
            let cut = ticks.len() / 4;
            let (_, &mut threshold, _) = ticks.select_nth_unstable(cut);
            let mut freed = 0;
            self.map.retain(|_, e| {
                let keep = e.1 > threshold;
                if !keep {
                    freed += e.0.cost();
                }
                keep
            });
            self.bytes -= freed;
        }
    }
}

const CACHE_BUDGET: usize = 24 << 20;
const MAX_TREE_DEPTH: usize = 16;

/// State of a [`Ndb::walk_btree`] traversal.
struct Walk<'a> {
    seen: HashSet<u64>,
    err: Option<Error>,
    f: &'a mut dyn FnMut(&[u8]),
}

pub struct Ndb {
    data: Arc<Backing>,
    pub header: Header,
    cache: Mutex<BlockCache>,
    /// Complete NBT / BBT maps from a full walk, built on the first lookup miss. Some
    /// writers (seen in eDiscovery exports) produce B-trees whose pages are not in key
    /// order; binary search then misses entries that a walk still finds.
    nbt_all: OnceLock<HashMap<u32, NodeEntry>>,
    bbt_all: OnceLock<HashMap<u64, BlockEntry>>,
}

impl Ndb {
    pub fn new(data: Backing) -> Result<Ndb> {
        let header = parse_header(&data)?;
        if !matches!(header.crypt, crypt::NDB_CRYPT_NONE | crypt::NDB_CRYPT_PERMUTE | crypt::NDB_CRYPT_CYCLIC) {
            return Err(Error::Unsupported(format!("PST encryption method 0x{:02X}", header.crypt)));
        }
        Ok(Ndb {
            data: Arc::new(data),
            header,
            cache: Mutex::new(BlockCache { map: HashMap::new(), tick: 0, bytes: 0, budget: CACHE_BUDGET }),
            nbt_all: OnceLock::new(),
            bbt_all: OnceLock::new(),
        })
    }

    pub fn format(&self) -> Format {
        self.header.format
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    fn page(&self, ib: u64, ptype: u8) -> Result<PageView<'_>> {
        let fmt = self.format();
        let ps = fmt.page_size();
        let start = usize::try_from(ib).map_err(|_| Error::Format("page offset overflow".into()))?;
        let p = self
            .data
            .get(start..start.saturating_add(ps))
            .ok_or_else(|| Error::Format(format!("B-tree page at 0x{ib:X} beyond end of file")))?;
        let (area, trailer) = fmt.btpage_layout();
        if p[trailer] != ptype || p[trailer + 1] != ptype {
            return Err(Error::Format(format!(
                "B-tree page at 0x{ib:X}: type 0x{:02X}/0x{:02X}, expected 0x{ptype:02X}",
                p[trailer],
                p[trailer + 1]
            )));
        }
        let (count, size, level) = match fmt {
            Format::Ansi => (p[496] as usize, p[498] as usize, p[499]),
            Format::Unicode => (p[488] as usize, p[490] as usize, p[491]),
            Format::Unicode4k => (u16_at(p, 4056) as usize, p[4060] as usize, p[4061]),
        };
        // Bytes the entry parsers read: BTENTRY (key, bid, ib); BBTENTRY up to cRef; NBTENTRY up
        // to nidParent. [MS-PST] 2.2.2.7.7
        let min = match (level, ptype, fmt) {
            (1.., _, _) => 3 * fmt.id_size(),
            (0, PTYPE_NBT, Format::Ansi) => 16,
            (0, PTYPE_NBT, _) => 28,
            (0, _, Format::Ansi) => 12,
            (0, _, _) => 20,
        };
        if size < min || count * size > area {
            return Err(Error::Format(format!("B-tree page at 0x{ib:X}: bad entry layout {count}x{size}")));
        }
        Ok(PageView { entries: &p[..area], count, size, level })
    }

    /// Find the leaf entry whose key equals `key` (keys compared after `mask`).
    fn btree_find(&self, root: u64, ptype: u8, key: u64, mask: u64) -> Result<Option<&[u8]>> {
        let isz = self.format().id_size();
        let mut ib = root;
        for _ in 0..MAX_TREE_DEPTH {
            let p = self.page(ib, ptype)?;
            let key_of = |i: usize| uint_at(p.entry(i), 0, isz) & mask;
            // First index whose key is > target.
            let (mut lo, mut hi) = (0, p.count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if key_of(mid) <= key {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            if lo == 0 {
                return Ok(None);
            }
            let e = p.entry(lo - 1);
            if p.level == 0 {
                return Ok((key_of(lo - 1) == key).then_some(e));
            }
            ib = uint_at(e, 2 * isz, isz);
        }
        Err(Error::Format(format!("B-tree deeper than {MAX_TREE_DEPTH} levels")))
    }

    /// Visit every leaf entry reachable from `root`, in page order. Each page is read at most
    /// once, so cyclic or shared child pointers cannot loop or blow up; damaged pages are
    /// skipped and the first error is returned once everything reachable has been visited.
    fn walk_btree(&self, root: u64, ptype: u8, f: &mut dyn FnMut(&[u8])) -> Result<()> {
        let mut w = Walk { seen: HashSet::new(), err: None, f };
        self.walk_page(root, ptype, 0, &mut w);
        w.err.map_or(Ok(()), Err)
    }

    fn walk_page(&self, ib: u64, ptype: u8, depth: usize, w: &mut Walk<'_>) {
        let page = if depth > MAX_TREE_DEPTH {
            Err(Error::Format(format!("B-tree deeper than {MAX_TREE_DEPTH} levels")))
        } else if !w.seen.insert(ib) {
            Err(Error::Format(format!("B-tree page at 0x{ib:X} referenced more than once")))
        } else {
            self.page(ib, ptype)
        };
        let p = match page {
            Ok(p) => p,
            Err(e) => {
                w.err.get_or_insert(e);
                return;
            }
        };
        let isz = self.format().id_size();
        for i in 0..p.count {
            let e = p.entry(i);
            if p.level == 0 {
                (w.f)(e);
            } else {
                self.walk_page(uint_at(e, 2 * isz, isz), ptype, depth + 1, w);
            }
        }
    }

    fn parse_node_entry(&self, e: &[u8]) -> NodeEntry {
        match self.format() {
            Format::Ansi => NodeEntry {
                nid: u32_at(e, 0),
                bid_data: u32_at(e, 4) as u64,
                bid_sub: u32_at(e, 8) as u64,
                parent: u32_at(e, 12),
            },
            _ => NodeEntry { nid: u32_at(e, 0), bid_data: u64_at(e, 8), bid_sub: u64_at(e, 16), parent: u32_at(e, 24) },
        }
    }

    fn parse_block_entry(&self, e: &[u8]) -> BlockEntry {
        match self.format() {
            Format::Ansi => {
                BlockEntry { bid: u32_at(e, 0) as u64, ib: u32_at(e, 4) as u64, cb: u16_at(e, 8), refs: u16_at(e, 10) }
            }
            _ => BlockEntry { bid: u64_at(e, 0), ib: u64_at(e, 8), cb: u16_at(e, 16), refs: u16_at(e, 18) },
        }
    }

    /// Look up a node in the NBT.
    pub fn node(&self, nid: u32) -> Result<NodeEntry> {
        let h = &self.header;
        let miss = match self.btree_find(h.nbt_ib, PTYPE_NBT, nid as u64, 0xFFFF_FFFF) {
            Ok(Some(e)) => return Ok(self.parse_node_entry(e)),
            Ok(None) => Error::NotFound(format!("node 0x{nid:X}")),
            Err(e) => e,
        };
        let all = self.nbt_all.get_or_init(|| {
            let mut m = HashMap::new();
            // Damaged pages are skipped; keep everything reachable.
            let _ = self.walk_btree(h.nbt_ib, PTYPE_NBT, &mut |e| {
                let n = self.parse_node_entry(e);
                m.insert(n.nid, n);
            });
            m
        });
        all.get(&nid).copied().ok_or(miss)
    }

    /// Look up a block in the BBT (the reserved low bit of `bid` is ignored).
    pub fn block_entry(&self, bid: u64) -> Result<BlockEntry> {
        let h = &self.header;
        let miss = match self.btree_find(h.bbt_ib, PTYPE_BBT, bid & !1, !1) {
            Ok(Some(e)) => return Ok(self.parse_block_entry(e)),
            Ok(None) => Error::Format(format!("block 0x{bid:X} not in BBT")),
            Err(e) => e,
        };
        let all = self.bbt_all.get_or_init(|| {
            let mut m = HashMap::new();
            let _ = self.walk_btree(h.bbt_ib, PTYPE_BBT, &mut |e| {
                let b = self.parse_block_entry(e);
                m.insert(b.bid & !1, b);
            });
            m
        });
        all.get(&(bid & !1)).copied().ok_or(miss)
    }

    /// Every NBT leaf entry in page order (diagnostics / recovery scans). Errors if any page
    /// is damaged.
    pub fn all_nodes(&self) -> Result<Vec<NodeEntry>> {
        let mut out = Vec::new();
        self.walk_btree(self.header.nbt_ib, PTYPE_NBT, &mut |e| out.push(self.parse_node_entry(e)))?;
        Ok(out)
    }

    /// Every BBT leaf entry in page order (diagnostics). Errors if any page is damaged.
    pub fn all_blocks(&self) -> Result<Vec<BlockEntry>> {
        let mut out = Vec::new();
        self.walk_btree(self.header.bbt_ib, PTYPE_BBT, &mut |e| out.push(self.parse_block_entry(e)))?;
        Ok(out)
    }

    /// Read and decode one block (decrypt external blocks, inflate 4K compressed blocks).
    pub fn block(&self, bid: u64) -> Result<Block> {
        let key = bid & !1;
        if key == 0 {
            return Err(Error::Format("reference to null block".into()));
        }
        if let Some(b) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).get(key) {
            return Ok(b);
        }
        let e = self.block_entry(key)?;
        let start = usize::try_from(e.ib).map_err(|_| Error::Format("block offset overflow".into()))?;
        let end = start.saturating_add(e.cb as usize);
        if end > self.data.len() {
            return Err(Error::Format(format!("block 0x{bid:X} at 0x{:X}+{} beyond end of file", e.ib, e.cb)));
        }
        let internal = (e.bid | bid) & 2 != 0;
        let inflated = if self.format() == Format::Unicode4k { self.inflated_size(start, e.cb as usize) } else { None };
        let crypt = if internal { crypt::NDB_CRYPT_NONE } else { self.header.crypt };
        let raw = &self.data[start..end];
        let block = match (inflated, crypt) {
            (None, crypt::NDB_CRYPT_NONE) => Block(Repr::Mapped(self.data.clone(), start..end)),
            (None, c) => {
                let mut v = raw.to_vec();
                self.decrypt(&mut v, c, key);
                Block::from_vec(v)
            }
            (Some(n), crypt::NDB_CRYPT_NONE) => Block::from_vec(inflate(raw, n, bid)?),
            (Some(n), c) => {
                // Compressed + encrypted is not documented: try decrypt→inflate, then
                // inflate→decrypt (the order that looks like a zlib stream goes first).
                let decrypt_first = || {
                    let mut v = raw.to_vec();
                    self.decrypt(&mut v, c, key);
                    inflate(&v, n, bid)
                };
                let inflate_first = || {
                    inflate(raw, n, bid).map(|mut v| {
                        self.decrypt(&mut v, c, key);
                        v
                    })
                };
                let v = if looks_like_zlib(raw) {
                    inflate_first().or_else(|_| decrypt_first())?
                } else {
                    decrypt_first().or_else(|_| inflate_first())?
                };
                Block::from_vec(v)
            }
        };
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, block.clone());
        Ok(block)
    }

    fn decrypt(&self, v: &mut [u8], method: u8, key: u64) {
        match method {
            crypt::NDB_CRYPT_PERMUTE => crypt::permute_decode(v),
            crypt::NDB_CRYPT_CYCLIC => crypt::cyclic(v, key as u32),
            _ => {}
        }
    }

    /// 4K blocks carry their inflated size in the trailer; `Some` when it differs from `cb`.
    fn inflated_size(&self, start: usize, cb: usize) -> Option<usize> {
        let fmt = self.format();
        let t = start.checked_add(fmt.block_alloc_size(cb) - fmt.block_trailer_size())?;
        let tr = self.data.get(t..t.checked_add(24)?)?;
        let n = u16_at(tr, 18) as usize;
        (n != 0 && n != cb).then_some(n)
    }

    /// Leaf (external) block ids of a data tree, in order. Never empty.
    ///
    /// The leaves' on-disk sizes must add up to no more than the file: a legitimate tree
    /// stores each leaf once, while (X)XBLOCKs that repeat a child (or point several BIDs at
    /// the same bytes) would otherwise let a small file expand into unbounded reads.
    pub fn data_tree(&self, bid: u64) -> Result<Vec<u64>> {
        if bid & 2 == 0 {
            return Ok(vec![bid]);
        }
        let mut out = Vec::new();
        self.collect_data(bid, 2, &mut out, &mut self.data.len())?;
        Ok(out)
    }

    /// `max_level`: 2 at the root (XXBLOCK allowed), 1 below an XXBLOCK, 0 below an XBLOCK.
    /// `budget`: file bytes not yet claimed by a leaf of this tree.
    fn collect_data(&self, bid: u64, max_level: u8, out: &mut Vec<u64>, budget: &mut usize) -> Result<()> {
        if bid & 2 == 0 {
            let size = self.format().block_alloc_size(self.block_entry(bid)?.cb as usize);
            *budget = budget.checked_sub(size).ok_or_else(|| {
                Error::Format(format!("data tree references more bytes than the file holds (0x{bid:X})"))
            })?;
            out.push(bid);
            return Ok(());
        }
        let b = self.block(bid)?;
        if b.len() < 8 || b[0] != 0x01 {
            return Err(Error::Format(format!("block 0x{bid:X} is not an XBLOCK/XXBLOCK")));
        }
        let level = b[1];
        if level == 0 || level > max_level {
            return Err(Error::Format(format!("XBLOCK 0x{bid:X} has level {level} where at most {max_level} fits")));
        }
        let n = u16_at(&b, 2) as usize;
        let isz = self.format().id_size();
        if n == 0 || 8 + n * isz > b.len() {
            return Err(Error::Format(format!("XBLOCK 0x{bid:X} has bad entry count {n}")));
        }
        for i in 0..n {
            self.collect_data(uint_at(&b, 8 + i * isz, isz), level - 1, out, budget)?;
        }
        Ok(())
    }

    /// Logical (decoded) size of a data tree without reading its leaf blocks: XBLOCK
    /// `lcbTotal`, or the block size (inflated size for compressed 4K blocks).
    pub fn data_size(&self, bid: u64) -> Result<u64> {
        if bid == 0 {
            return Ok(0);
        }
        if bid & 2 != 0 {
            let b = self.block(bid)?;
            if b.len() < 8 || b[0] != 0x01 {
                return Err(Error::Format(format!("block 0x{bid:X} is not an XBLOCK/XXBLOCK")));
            }
            return Ok(u32_at(&b, 4) as u64);
        }
        let e = self.block_entry(bid)?;
        let inflated = match self.format() {
            Format::Unicode4k => usize::try_from(e.ib).ok().and_then(|ib| self.inflated_size(ib, e.cb as usize)),
            _ => None,
        };
        Ok(inflated.unwrap_or(e.cb as usize) as u64)
    }

    /// Whole contents of a data tree (single blocks are not copied).
    pub fn read_data(&self, bid: u64) -> Result<Block> {
        if bid == 0 {
            return Ok(Block::empty());
        }
        if bid & 2 == 0 {
            return self.block(bid);
        }
        let ids = self.data_tree(bid)?;
        // Never more than the declared `lcbTotal`; capped pre-allocation (blocks ≤ 64 KiB).
        let total = usize::try_from(self.data_size(bid)?).unwrap_or(usize::MAX);
        let mut v = Vec::with_capacity(total.min(ids.len() << 16));
        for id in ids {
            let b = self.block(id)?;
            if v.len() + b.len() > total {
                return Err(Error::Format(format!("data tree 0x{bid:X} exceeds its declared size {total}")));
            }
            v.extend_from_slice(&b);
        }
        Ok(Block::from_vec(v))
    }

    /// Flattened, nid-sorted subnode tree.
    pub fn subnodes(&self, bid_sub: u64) -> Result<Vec<SubnodeEntry>> {
        let mut out = Vec::new();
        if bid_sub != 0 {
            self.collect_subnodes(bid_sub, 1, &mut out)?;
            out.sort_by_key(|e| e.nid);
        }
        Ok(out)
    }

    /// `max_level`: 1 at the root (SIBLOCK allowed), 0 below a SIBLOCK.
    fn collect_subnodes(&self, bid: u64, max_level: u8, out: &mut Vec<SubnodeEntry>) -> Result<()> {
        let b = self.block(bid)?;
        let isz = self.format().id_size();
        let hdr = if isz == 4 { 4 } else { 8 };
        if b.len() < hdr || b[0] != 0x02 {
            return Err(Error::Format(format!("block 0x{bid:X} is not an SLBLOCK/SIBLOCK")));
        }
        let level = b[1];
        if level > max_level {
            return Err(Error::Format(format!(
                "subnode block 0x{bid:X} has level {level} where at most {max_level} fits"
            )));
        }
        let n = u16_at(&b, 2) as usize;
        let esz = if level == 0 { 3 * isz } else { 2 * isz };
        if hdr + n * esz > b.len() {
            return Err(Error::Format(format!("subnode block 0x{bid:X} entry count {n} overflows block")));
        }
        // Each SLENTRY is stored once on disk; more than fit in the file means a SIBLOCK
        // repeats its children.
        if level == 0 && out.len() + n > self.data.len() / esz {
            return Err(Error::Format(format!("subnode tree references more entries than the file holds (0x{bid:X})")));
        }
        for i in 0..n {
            let o = hdr + i * esz;
            if level == 0 {
                out.push(SubnodeEntry {
                    nid: u32_at(&b, o),
                    bid_data: uint_at(&b, o + isz, isz),
                    bid_sub: uint_at(&b, o + 2 * isz, isz),
                });
            } else {
                self.collect_subnodes(uint_at(&b, o + isz, isz), 0, out)?;
            }
        }
        Ok(())
    }
}

fn looks_like_zlib(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] & 0x0F == 8 && b[0] >> 4 <= 7 && (u16::from(b[0]) << 8 | u16::from(b[1])) % 31 == 0
}

/// Inflate a 4K-format block (zlib stream, or raw deflate) to exactly `expected` bytes.
fn inflate(raw: &[u8], expected: usize, bid: u64) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let res = if looks_like_zlib(raw) {
        flate2::read::ZlibDecoder::new(raw).take(expected as u64).read_to_end(&mut out)
    } else {
        flate2::read::DeflateDecoder::new(raw).take(expected as u64).read_to_end(&mut out)
    };
    match res {
        Ok(_) if out.len() == expected => Ok(out),
        Ok(_) => Err(Error::Format(format!("block 0x{bid:X} inflated to {} bytes, expected {expected}", out.len()))),
        Err(e) => Err(Error::Format(format!("block 0x{bid:X} inflate failed: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_layouts() {
        assert_eq!(Format::Unicode.hid_parts(0x0001_0040), (1, 2));
        assert_eq!(Format::Ansi.hid_parts(0x20), (0, 1));
        let h = Format::Unicode4k.make_hid(3, 5000);
        assert_eq!(Format::Unicode4k.hid_parts(h), (3, 5000));
        assert_eq!(h & 0x1F, 0);
    }

    #[test]
    fn block_alloc_sizes() {
        assert_eq!(Format::Unicode.block_alloc_size(48), 64);
        assert_eq!(Format::Unicode.block_alloc_size(49), 128);
        assert_eq!(Format::Ansi.block_alloc_size(52), 64);
        assert_eq!(Format::Unicode.block_alloc_size(8176), 8192);
        assert_eq!(Format::Unicode4k.block_alloc_size(488), 512);
        assert_eq!(Format::Unicode4k.block_alloc_size(489), 1024);
    }

    #[test]
    fn rejects_bad_headers() {
        assert!(matches!(parse_header(b"nope"), Err(Error::Format(_))));
        let mut h = vec![0u8; 600];
        h[..4].copy_from_slice(MAGIC);
        h[10] = 99;
        assert!(matches!(parse_header(&h), Err(Error::Unsupported(_))));
        h[10] = 23;
        let p = parse_header(&h).unwrap();
        assert_eq!(p.format, Format::Unicode);
        h[10] = 36;
        assert_eq!(parse_header(&h).unwrap().format, Format::Unicode4k);
        h[10] = 14;
        assert_eq!(parse_header(&h[..520]).unwrap().format, Format::Ansi);
        h[10] = 23;
        assert!(matches!(parse_header(&h[..530]), Err(Error::Format(_))));
    }

    #[test]
    fn inflate_validates_size() {
        use std::io::Write;
        let data = vec![7u8; 3000];
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&data).unwrap();
        let comp = z.finish().unwrap();
        assert_eq!(inflate(&comp, 3000, 1).unwrap(), data);
        assert!(inflate(&comp, 3001, 1).is_err());
        assert!(inflate(&comp[..comp.len() / 2], 3000, 1).is_err());
        let mut d = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        d.write_all(&data).unwrap();
        assert_eq!(inflate(&d.finish().unwrap(), 3000, 1).unwrap(), data);
    }
}
