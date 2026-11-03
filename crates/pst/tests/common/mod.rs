//! Minimal PST/OST *writer* used only by tests: produces ANSI, Unicode and Unicode-4K files
//! (any encoding, optional zlib block compression) from scratch following [MS-PST], so every
//! reader code path (multi-level B-trees, XBLOCK data trees, SIBLOCK subnode trees, multi-block
//! heaps, row matrices spanning blocks, embedded messages) is exercised on known content.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Write;

use stv_pst::crypt;
use stv_pst::ndb::Format;

pub const PT_I16: u16 = 0x0002;
pub const PT_I32: u16 = 0x0003;
pub const PT_BOOL: u16 = 0x000B;
pub const PT_OBJ: u16 = 0x000D;
pub const PT_I64: u16 = 0x0014;
pub const PT_STR8: u16 = 0x001E;
pub const PT_STR: u16 = 0x001F;
pub const PT_TIME: u16 = 0x0040;
pub const PT_BIN: u16 = 0x0102;
pub const PT_MSTR: u16 = 0x101F;

/// FILETIME for UTC ms since the Unix epoch.
pub fn ft(ms: i64) -> u64 {
    mapi_props::unix_ms_to_filetime(ms)
}

pub fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A property value as written into a PC / TC cell.
#[derive(Clone, Debug)]
pub enum Val {
    I32(i32),
    I16(i16),
    Bool(bool),
    I64(i64),
    Time(i64),
    Str(String),
    Str8(Vec<u8>),
    Bin(Vec<u8>),
    MultiStr(Vec<String>),
    /// PtypObject referencing subnode `nid` of `size` bytes.
    Obj(u32, u32),
}

impl Val {
    pub fn ty(&self) -> u16 {
        match self {
            Val::I32(_) => PT_I32,
            Val::I16(_) => PT_I16,
            Val::Bool(_) => PT_BOOL,
            Val::I64(_) => PT_I64,
            Val::Time(_) => PT_TIME,
            Val::Str(_) => PT_STR,
            Val::Str8(_) => PT_STR8,
            Val::Bin(_) => PT_BIN,
            Val::MultiStr(_) => PT_MSTR,
            Val::Obj(..) => PT_OBJ,
        }
    }
    fn fixed(&self) -> Option<Vec<u8>> {
        Some(match self {
            Val::I32(v) => v.to_le_bytes().to_vec(),
            Val::I16(v) => v.to_le_bytes().to_vec(),
            Val::Bool(b) => vec![*b as u8],
            Val::I64(v) => v.to_le_bytes().to_vec(),
            Val::Time(ms) => ft(*ms).to_le_bytes().to_vec(),
            _ => return None,
        })
    }
    fn var_bytes(&self) -> Vec<u8> {
        match self {
            Val::Str(s) => utf16(s),
            Val::Str8(b) | Val::Bin(b) => b.clone(),
            Val::MultiStr(v) => {
                let parts: Vec<Vec<u8>> = v.iter().map(|s| utf16(s)).collect();
                let mut out = Vec::new();
                out.extend_from_slice(&(parts.len() as u32).to_le_bytes());
                let mut off = 4 + 4 * parts.len();
                for p in &parts {
                    out.extend_from_slice(&(off as u32).to_le_bytes());
                    off += p.len();
                }
                for p in parts {
                    out.extend_from_slice(&p);
                }
                out
            }
            Val::Obj(nid, size) => [nid.to_le_bytes(), size.to_le_bytes()].concat(),
            _ => unreachable!(),
        }
    }
}

pub type Props = Vec<(u16, Val)>;

/// Heap-on-node under construction.
pub struct HeapBuilder {
    fmt: Format,
    client_sig: u8,
    cap: usize,
    blocks: Vec<Vec<Vec<u8>>>,
    /// HNHDR `hidUserRoot` (set by the caller once the root structure is allocated).
    pub root: u32,
}

const MAX_ALLOC: usize = 3580;

impl HeapBuilder {
    pub fn new(fmt: Format, client_sig: u8, cap: usize) -> Self {
        HeapBuilder { fmt, client_sig, cap, blocks: vec![Vec::new()], root: 0 }
    }

    fn header_size(i: usize) -> usize {
        if i == 0 {
            12
        } else if i >= 8 && (i - 8).is_multiple_of(128) {
            66
        } else {
            2
        }
    }

    fn used(&self, i: usize) -> usize {
        let b = &self.blocks[i];
        let data: usize = b.iter().map(Vec::len).sum();
        Self::header_size(i) + data + (data % 2) + 4 + (b.len() + 2) * 2
    }

    pub fn alloc(&mut self, bytes: Vec<u8>) -> u32 {
        assert!(bytes.len() <= MAX_ALLOC);
        let last = self.blocks.len() - 1;
        if self.used(last) + bytes.len() + 2 > self.cap {
            self.blocks.push(Vec::new());
        }
        let bi = self.blocks.len() - 1;
        self.blocks[bi].push(bytes);
        self.fmt.make_hid(bi, self.blocks[bi].len())
    }

    pub fn build(self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (i, allocs) in self.blocks.iter().enumerate() {
            let hdr = Self::header_size(i);
            let mut b = vec![0u8; hdr];
            let mut offs = vec![hdr as u16];
            for a in allocs {
                b.extend_from_slice(a);
                offs.push(b.len() as u16);
            }
            if b.len() % 2 == 1 {
                b.push(0);
            }
            let ib_hnpm = b.len() as u16;
            b[0..2].copy_from_slice(&ib_hnpm.to_le_bytes());
            if i == 0 {
                b[2] = 0xEC;
                b[3] = self.client_sig;
                b[4..8].copy_from_slice(&self.root.to_le_bytes());
            }
            b.extend_from_slice(&(allocs.len() as u16).to_le_bytes());
            b.extend_from_slice(&0u16.to_le_bytes());
            for o in offs {
                b.extend_from_slice(&o.to_le_bytes());
            }
            out.push(b);
        }
        out
    }
}

/// Build a BTH over `recs` (sorted by key); returns the BTHHEADER hid.
pub fn build_bth(h: &mut HeapBuilder, key_size: usize, data_size: usize, mut recs: Vec<(Vec<u8>, Vec<u8>)>) -> u32 {
    recs.sort_by(|a, b| a.0.iter().rev().cmp(b.0.iter().rev()));
    let mut levels = 0u8;
    let mut root = 0u32;
    if !recs.is_empty() {
        let per = MAX_ALLOC / (key_size + data_size);
        let mut level: Vec<(Vec<u8>, u32)> = recs
            .chunks(per)
            .map(|c| {
                let bytes: Vec<u8> = c.iter().flat_map(|(k, d)| k.iter().chain(d.iter()).copied()).collect();
                (c[0].0.clone(), h.alloc(bytes))
            })
            .collect();
        while level.len() > 1 {
            levels += 1;
            let per = MAX_ALLOC / (key_size + 4);
            level = level
                .chunks(per)
                .map(|c| {
                    let bytes: Vec<u8> =
                        c.iter().flat_map(|(k, hid)| k.iter().copied().chain(hid.to_le_bytes())).collect();
                    (c[0].0.clone(), h.alloc(bytes))
                })
                .collect();
        }
        root = level[0].1;
    }
    let mut hdr = vec![0xB5, key_size as u8, data_size as u8, levels];
    hdr.extend_from_slice(&root.to_le_bytes());
    h.alloc(hdr)
}

/// A node's subnodes under construction: (nid, bid_data, bid_sub).
pub type Subs = Vec<(u32, u64, u64)>;

pub struct Writer {
    pub fmt: Format,
    pub crypt: u8,
    pub compress: bool,
    /// Largest data block payload.
    pub max_block: usize,
    /// Heap block capacity (≤ max_block); small values force multi-block heaps.
    pub heap_cap: usize,
    /// Max entries per B-tree page (forces multi-level trees when small).
    pub page_entries: Option<usize>,
    /// Max SLENTRYs per SLBLOCK (forces SIBLOCKs when small).
    pub sl_max: Option<usize>,
    /// Write folder contents tables only, without message nodes (fast huge-folder files).
    pub contents_only: bool,
    next_bid: u64,
    next_sub: u32,
    blocks: Vec<(u64, Vec<u8>)>,
    nodes: BTreeMap<u32, (u64, u64, u32)>,
}

impl Writer {
    pub fn new(fmt: Format, crypt: u8, compress: bool) -> Self {
        let max_block = match fmt {
            Format::Ansi => 8180,
            Format::Unicode => 8176,
            Format::Unicode4k => 65512,
        };
        Writer {
            fmt,
            crypt,
            compress,
            max_block,
            heap_cap: max_block,
            page_entries: None,
            sl_max: None,
            contents_only: false,
            next_bid: 4,
            next_sub: 1,
            blocks: Vec::new(),
            nodes: BTreeMap::new(),
        }
    }

    pub fn add_block(&mut self, data: Vec<u8>, internal: bool) -> u64 {
        assert!(data.len() <= self.max_block, "block too large");
        let bid = self.next_bid | if internal { 2 } else { 0 };
        self.next_bid += 4;
        self.blocks.push((bid, data));
        bid
    }

    /// One data block per chunk; XBLOCK/XXBLOCK when more than one.
    pub fn add_blocks(&mut self, chunks: Vec<Vec<u8>>) -> u64 {
        if chunks.is_empty() {
            return 0;
        }
        let total: usize = chunks.iter().map(Vec::len).sum();
        let bids: Vec<u64> = chunks.into_iter().map(|c| self.add_block(c, false)).collect();
        if bids.len() == 1 {
            return bids[0];
        }
        let isz = self.fmt.id_size();
        let per = (self.max_block - 8) / isz;
        let mk = |w: &mut Writer, level: u8, ids: &[u64], total: usize| {
            let mut b = vec![0x01, level];
            b.extend_from_slice(&(ids.len() as u16).to_le_bytes());
            b.extend_from_slice(&(total as u32).to_le_bytes());
            for &id in ids {
                b.extend_from_slice(&id.to_le_bytes()[..isz]);
            }
            w.add_block(b, true)
        };
        if bids.len() <= per {
            return mk(self, 1, &bids, total);
        }
        let x: Vec<u64> = bids.chunks(per).map(|c| mk(self, 1, c, 0)).collect();
        mk(self, 2, &x, total)
    }

    /// Split arbitrary bytes into max-size blocks.
    pub fn add_data(&mut self, data: &[u8]) -> u64 {
        if data.is_empty() {
            return self.add_block(Vec::new(), false);
        }
        let chunks = data.chunks(self.max_block).map(<[u8]>::to_vec).collect();
        self.add_blocks(chunks)
    }

    pub fn new_sub_nid(&mut self, ty: u32) -> u32 {
        let n = (self.next_sub << 5) | ty;
        self.next_sub += 1;
        n
    }

    pub fn add_subnodes(&mut self, mut subs: Subs) -> u64 {
        if subs.is_empty() {
            return 0;
        }
        subs.sort_by_key(|s| s.0);
        let isz = self.fmt.id_size();
        let hdr = if isz == 4 { 4 } else { 8 };
        let per = self.sl_max.unwrap_or((self.max_block - hdr) / (3 * isz));
        let sl = |w: &mut Writer, level: u8, ents: &[Vec<u64>]| {
            let mut b = vec![0x02, level];
            b.extend_from_slice(&(ents.len() as u16).to_le_bytes());
            b.resize(hdr, 0);
            for e in ents {
                for v in e {
                    b.extend_from_slice(&v.to_le_bytes()[..isz]);
                }
            }
            w.add_block(b, true)
        };
        let leaves: Vec<(u32, u64)> = subs
            .chunks(per)
            .map(|c| {
                let ents: Vec<Vec<u64>> = c.iter().map(|&(n, d, s)| vec![n as u64, d, s]).collect();
                (c[0].0, sl(self, 0, &ents))
            })
            .collect();
        if leaves.len() == 1 {
            return leaves[0].1;
        }
        let ents: Vec<Vec<u64>> = leaves.iter().map(|&(n, b)| vec![n as u64, b]).collect();
        sl(self, 1, &ents)
    }

    pub fn add_node(&mut self, nid: u32, bid_data: u64, bid_sub: u64, parent: u32) {
        self.nodes.insert(nid, (bid_data, bid_sub, parent));
    }

    /// Variable-size value: heap allocation, or a subnode when large.
    fn var_value(&mut self, h: &mut HeapBuilder, subs: &mut Subs, bytes: Vec<u8>) -> u32 {
        if bytes.is_empty() {
            return 0;
        }
        if bytes.len() <= MAX_ALLOC {
            h.alloc(bytes)
        } else {
            let nid = self.new_sub_nid(0x1F);
            let bid = self.add_data(&bytes);
            subs.push((nid, bid, 0));
            nid
        }
    }

    /// Property context; returns (data bid, subnodes so far — caller may add more).
    pub fn pc(&mut self, props: &Props, mut subs: Subs) -> (u64, Subs) {
        let mut h = HeapBuilder::new(self.fmt, 0xBC, self.heap_cap);
        let mut recs = Vec::new();
        for (id, v) in props {
            let hnid = match v.fixed() {
                Some(b) if b.len() <= 4 => {
                    let mut x = [0u8; 4];
                    x[..b.len()].copy_from_slice(&b);
                    u32::from_le_bytes(x)
                }
                Some(b) => h.alloc(b),
                None => self.var_value(&mut h, &mut subs, v.var_bytes()),
            };
            let mut d = v.ty().to_le_bytes().to_vec();
            d.extend_from_slice(&hnid.to_le_bytes());
            recs.push((id.to_le_bytes().to_vec(), d));
        }
        h.root = build_bth(&mut h, 2, 6, recs);
        let bid = self.add_blocks(h.build());
        (bid, subs)
    }

    /// Table context. `cols` are (id, type); rows are (row id, cells by column index).
    pub fn tc(&mut self, cols: &[(u16, u16)], rows: &[(u32, Vec<Option<Val>>)]) -> (u64, Subs) {
        let mut subs = Subs::new();
        let mut h = HeapBuilder::new(self.fmt, 0x7C, self.heap_cap);
        // Layout: LtpRowId @0, LtpRowVer @4, then 8-byte, 4-byte, 2-byte, 1-byte groups, CEB.
        let mut all: Vec<(u16, u16)> = vec![(0x67F2, PT_I32), (0x67F3, PT_I32)];
        all.extend_from_slice(cols);
        let size_of = |ty: u16| match ty {
            PT_I64 | PT_TIME => 8,
            PT_I16 => 2,
            PT_BOOL => 1,
            _ => 4,
        };
        let mut ib = vec![0usize; all.len()];
        let mut off = 8;
        ib[1] = 4;
        for want in [8, 4, 2, 1] {
            for (i, &(_, ty)) in all.iter().enumerate().skip(2) {
                if size_of(ty) == want {
                    ib[i] = off;
                    off += want;
                }
            }
        }
        let end4 = 8 + all.iter().skip(2).filter(|c| size_of(c.1) >= 4).map(|c| size_of(c.1)).sum::<usize>();
        let end2 = end4 + all.iter().skip(2).filter(|c| size_of(c.1) == 2).count() * 2;
        let end1 = end2 + all.iter().skip(2).filter(|c| size_of(c.1) == 1).count();
        let row_size = end1 + all.len().div_ceil(8);

        let mut matrix = Vec::with_capacity(rows.len() * row_size);
        for (rid, cells) in rows {
            let mut r = vec![0u8; row_size];
            r[0..4].copy_from_slice(&rid.to_le_bytes());
            r[end1] |= 0x80 | 0x40; // row id + row ver exist
            for (ci, cell) in cells.iter().enumerate() {
                let Some(v) = cell else { continue };
                let i = ci + 2;
                let bytes = match v.fixed() {
                    Some(b) => b,
                    None => {
                        let bytes = v.var_bytes();
                        self.var_value(&mut h, &mut subs, bytes).to_le_bytes().to_vec()
                    }
                };
                r[ib[i]..ib[i] + bytes.len()].copy_from_slice(&bytes);
                r[end1 + i / 8] |= 0x80 >> (i % 8);
            }
            matrix.extend_from_slice(&r);
        }
        let hnid_rows = if rows.is_empty() {
            0
        } else if matrix.len() <= MAX_ALLOC {
            h.alloc(matrix)
        } else {
            let per = self.max_block / row_size;
            let chunks = matrix.chunks(per * row_size).map(<[u8]>::to_vec).collect();
            let bid = self.add_blocks(chunks);
            let nid = self.new_sub_nid(0x1F);
            subs.push((nid, bid, 0));
            nid
        };
        let idx_size = if self.fmt == Format::Ansi { 2 } else { 4 };
        let recs = rows
            .iter()
            .enumerate()
            .map(|(i, (rid, _))| (rid.to_le_bytes().to_vec(), (i as u32).to_le_bytes()[..idx_size].to_vec()))
            .collect();
        let hid_index = build_bth(&mut h, 4, idx_size, recs);
        let mut info = vec![0x7C, all.len() as u8];
        for v in [end4, end2, end1, row_size] {
            info.extend_from_slice(&(v as u16).to_le_bytes());
        }
        info.extend_from_slice(&hid_index.to_le_bytes());
        info.extend_from_slice(&hnid_rows.to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes());
        for (i, &(id, ty)) in all.iter().enumerate() {
            info.extend_from_slice(&(((id as u32) << 16) | ty as u32).to_le_bytes());
            info.extend_from_slice(&(ib[i] as u16).to_le_bytes());
            info.push(size_of(ty) as u8);
            info.push(i as u8);
        }
        h.root = h.alloc(info);
        let bid = self.add_blocks(h.build());
        (bid, subs)
    }

    fn sig(ib: u64, bid: u64) -> u16 {
        let x = (ib ^ bid) as u32;
        ((x >> 16) as u16) ^ (x as u16)
    }

    /// Serialise the whole file.
    pub fn finish(mut self, client: &[u8; 2]) -> Vec<u8> {
        let fmt = self.fmt;
        let isz = fmt.id_size();
        let ps = fmt.page_size();
        let first = if fmt == Format::Unicode4k { 0x22000 + ps } else { 0x4600 + ps };
        let mut file = vec![0u8; first];
        let mut bbt: Vec<Vec<u8>> = Vec::new();
        let blocks = std::mem::take(&mut self.blocks);
        for (bid, data) in blocks {
            let mut stored = data.clone();
            let mut inflated = data.len();
            if self.compress && fmt == Format::Unicode4k && data.len() > 64 {
                let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                z.write_all(&data).unwrap();
                let c = z.finish().unwrap();
                if c.len() < data.len() {
                    stored = c;
                } else {
                    inflated = stored.len();
                }
            }
            if bid & 2 == 0 {
                match self.crypt {
                    crypt::NDB_CRYPT_PERMUTE => crypt::permute_encode(&mut stored),
                    crypt::NDB_CRYPT_CYCLIC => crypt::cyclic(&mut stored, (bid & !1) as u32),
                    _ => {}
                }
            }
            let ib = file.len() as u64;
            let cb = stored.len();
            let alloc = fmt.block_alloc_size(cb);
            let mut blk = stored.clone();
            blk.resize(alloc - fmt.block_trailer_size(), 0);
            let crc = crypt::crc(&stored);
            blk.extend_from_slice(&(cb as u16).to_le_bytes());
            blk.extend_from_slice(&Self::sig(ib, bid).to_le_bytes());
            match fmt {
                Format::Ansi => {
                    blk.extend_from_slice(&(bid as u32).to_le_bytes());
                    blk.extend_from_slice(&crc.to_le_bytes());
                }
                Format::Unicode => {
                    blk.extend_from_slice(&crc.to_le_bytes());
                    blk.extend_from_slice(&bid.to_le_bytes());
                }
                Format::Unicode4k => {
                    blk.extend_from_slice(&crc.to_le_bytes());
                    blk.extend_from_slice(&bid.to_le_bytes());
                    blk.extend_from_slice(&2u16.to_le_bytes());
                    blk.extend_from_slice(&(inflated as u16).to_le_bytes());
                    blk.extend_from_slice(&0u32.to_le_bytes());
                }
            }
            file.extend_from_slice(&blk);
            let mut e = Vec::new();
            e.extend_from_slice(&bid.to_le_bytes()[..isz]);
            e.extend_from_slice(&ib.to_le_bytes()[..isz]);
            e.extend_from_slice(&(cb as u16).to_le_bytes());
            e.extend_from_slice(&1u16.to_le_bytes());
            if isz == 8 {
                e.extend_from_slice(&0u32.to_le_bytes());
            }
            bbt.push(e);
        }
        let nbt: Vec<Vec<u8>> = self
            .nodes
            .iter()
            .map(|(&nid, &(d, s, p))| {
                let mut e = Vec::new();
                e.extend_from_slice(&(nid as u64).to_le_bytes()[..isz]);
                e.extend_from_slice(&d.to_le_bytes()[..isz]);
                e.extend_from_slice(&s.to_le_bytes()[..isz]);
                e.extend_from_slice(&p.to_le_bytes());
                if isz == 8 {
                    e.extend_from_slice(&0u32.to_le_bytes());
                }
                e
            })
            .collect();
        // Pad to page alignment before B-tree pages.
        let pad = file.len().div_ceil(ps) * ps;
        file.resize(pad, 0);
        let mut next_page_bid = 0x1_0000u64;
        let (bbt_ib, bbt_bid) = self.write_tree(&mut file, 0x80, bbt, &mut next_page_bid);
        let (nbt_ib, nbt_bid) = self.write_tree(&mut file, 0x81, nbt, &mut next_page_bid);
        let eof = file.len() as u64;

        // Header.
        file[0..4].copy_from_slice(b"!BDN");
        file[8..10].copy_from_slice(client);
        let ver: u16 = match fmt {
            Format::Ansi => 14,
            Format::Unicode => 23,
            Format::Unicode4k => 36,
        };
        file[10..12].copy_from_slice(&ver.to_le_bytes());
        file[12..14].copy_from_slice(&19u16.to_le_bytes());
        file[14] = 1;
        file[15] = 1;
        match fmt {
            Format::Ansi => {
                file[24..28].copy_from_slice(&(self.next_bid as u32).to_le_bytes());
                file[168..172].copy_from_slice(&(eof as u32).to_le_bytes());
                file[184..188].copy_from_slice(&(nbt_bid as u32).to_le_bytes());
                file[188..192].copy_from_slice(&(nbt_ib as u32).to_le_bytes());
                file[192..196].copy_from_slice(&(bbt_bid as u32).to_le_bytes());
                file[196..200].copy_from_slice(&(bbt_ib as u32).to_le_bytes());
                file[200] = 2;
                file[460] = 0x80;
                file[461] = self.crypt;
            }
            _ => {
                file[184..192].copy_from_slice(&eof.to_le_bytes());
                file[216..224].copy_from_slice(&nbt_bid.to_le_bytes());
                file[224..232].copy_from_slice(&nbt_ib.to_le_bytes());
                file[232..240].copy_from_slice(&bbt_bid.to_le_bytes());
                file[240..248].copy_from_slice(&bbt_ib.to_le_bytes());
                file[248] = 2;
                file[512] = 0x80;
                file[513] = self.crypt;
                file[516..524].copy_from_slice(&self.next_bid.to_le_bytes());
                let full = crypt::crc(&file[8..524]);
                file[524..528].copy_from_slice(&full.to_le_bytes());
            }
        }
        let partial = crypt::crc(&file[8..8 + 471]);
        file[4..8].copy_from_slice(&partial.to_le_bytes());
        file
    }

    /// Write B-tree pages bottom-up; returns (root ib, root page bid).
    fn write_tree(&self, file: &mut Vec<u8>, ptype: u8, leaves: Vec<Vec<u8>>, next_bid: &mut u64) -> (u64, u64) {
        let fmt = self.fmt;
        let isz = fmt.id_size();
        let (area, _) = match fmt {
            Format::Ansi => (496, 500),
            Format::Unicode => (488, 496),
            Format::Unicode4k => (4056, 4072),
        };
        let mut level = 0u8;
        let mut entries = leaves;
        loop {
            let esz = entries.first().map_or(3 * isz, Vec::len);
            let max = self.page_entries.unwrap_or(area / esz).min(area / esz).max(2);
            let chunks: Vec<Vec<Vec<u8>>> = if entries.is_empty() {
                vec![Vec::new()]
            } else {
                entries.chunks(max).map(<[Vec<u8>]>::to_vec).collect()
            };
            let mut parents = Vec::new();
            for c in &chunks {
                let ib = file.len() as u64;
                let bid = *next_bid;
                *next_bid += 4;
                let mut p = vec![0u8; fmt.page_size()];
                for (i, e) in c.iter().enumerate() {
                    p[i * esz..(i + 1) * esz].copy_from_slice(e);
                }
                match fmt {
                    Format::Ansi => {
                        p[496] = c.len() as u8;
                        p[497] = max as u8;
                        p[498] = esz as u8;
                        p[499] = level;
                        p[500] = ptype;
                        p[501] = ptype;
                        p[502..504].copy_from_slice(&Self::sig(ib, bid).to_le_bytes());
                        p[504..508].copy_from_slice(&(bid as u32).to_le_bytes());
                        let crc = crypt::crc(&p[..500]);
                        p[508..512].copy_from_slice(&crc.to_le_bytes());
                    }
                    Format::Unicode => {
                        p[488] = c.len() as u8;
                        p[489] = max as u8;
                        p[490] = esz as u8;
                        p[491] = level;
                        p[496] = ptype;
                        p[497] = ptype;
                        p[498..500].copy_from_slice(&Self::sig(ib, bid).to_le_bytes());
                        let crc = crypt::crc(&p[..496]);
                        p[500..504].copy_from_slice(&crc.to_le_bytes());
                        p[504..512].copy_from_slice(&bid.to_le_bytes());
                    }
                    Format::Unicode4k => {
                        p[4056..4058].copy_from_slice(&(c.len() as u16).to_le_bytes());
                        p[4058..4060].copy_from_slice(&(max as u16).to_le_bytes());
                        p[4060] = esz as u8;
                        p[4061] = level;
                        p[4072] = ptype;
                        p[4073] = ptype;
                        p[4074..4076].copy_from_slice(&Self::sig(ib, bid).to_le_bytes());
                        let crc = crypt::crc(&p[..4072]);
                        p[4076..4080].copy_from_slice(&crc.to_le_bytes());
                        p[4080..4088].copy_from_slice(&bid.to_le_bytes());
                    }
                }
                file.extend_from_slice(&p);
                let first_key = c.first().map_or(vec![0u8; isz], |e| e[..isz].to_vec());
                let mut be = first_key;
                be.extend_from_slice(&bid.to_le_bytes()[..isz]);
                be.extend_from_slice(&ib.to_le_bytes()[..isz]);
                parents.push((be, ib, bid));
            }
            if parents.len() == 1 {
                return (parents[0].1, parents[0].2);
            }
            entries = parents.into_iter().map(|p| p.0).collect();
            level += 1;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A small but complete message store.

pub const NID_STORE: u32 = 0x21;
pub const NID_NAMEMAP: u32 = 0x61;
pub const NID_ROOT: u32 = 0x122;
pub const NID_IPM: u32 = 0x8022;
pub const NID_INBOX: u32 = 0x8042;
pub const NID_BULK: u32 = 0x8062;
pub const NID_CAL: u32 = 0x8082;
pub const NID_SEARCH_ROOT: u32 = 0x80A2;

pub const KEYWORDS_ID: u16 = 0x8000;
/// Name-map ids of `PidLidAppointmentStartWhole` / `EndWhole` / `Location` (PSETID_Appointment).
pub const APPT_START_ID: u16 = 0x8002;
pub const APPT_END_ID: u16 = 0x8003;
pub const APPT_LOCATION_ID: u16 = 0x8004;
/// Name-map ids of `PidLidTaskDueDate` / `PidLidTaskOwner` (PSETID_Task). Not contents-table
/// columns: task rows must be completed from the message.
pub const TASK_DUE_ID: u16 = 0x8005;
pub const TASK_OWNER_ID: u16 = 0x8006;
pub const T0: i64 = 1_600_000_000_000; // 2020-09-13

pub struct MsgSpec {
    pub nid: u32,
    pub subject: String,
    pub from: String,
    pub from_smtp: String,
    pub to: Vec<(String, String)>,
    pub cc: Vec<(String, String)>,
    pub date: i64,
    pub size: i32,
    pub read: bool,
    pub body: String,
    pub html: Option<Vec<u8>>,
    pub categories: Vec<String>,
    pub attachments: Vec<AttSpec>,
    /// `PidTagMessageClass`.
    pub class: String,
    /// Further properties (e.g. the named appointment ids below).
    pub extra: Props,
}

pub enum AttSpec {
    File { name: String, mime: String, data: Vec<u8> },
    Embedded(Box<MsgSpec>),
}

pub fn simple_msg(nid: u32, i: usize) -> MsgSpec {
    MsgSpec {
        nid,
        subject: format!("Bulk message {i:04}"),
        from: format!("Sender {}", (i * 7) % 13),
        from_smtp: format!("sender{}@example.com", (i * 7) % 13),
        to: vec![("Bob".into(), "bob@example.com".into())],
        cc: vec![],
        date: T0 + ((i * 7919) % 1000) as i64 * 60_000,
        size: 1000 + ((i * 31) % 500) as i32,
        read: !i.is_multiple_of(3),
        body: format!("body {i}"),
        html: None,
        categories: vec![],
        attachments: vec![],
        class: "IPM.Note".into(),
        extra: vec![],
    }
}

fn msg_props(m: &MsgSpec) -> Props {
    let mut flags = if m.read { 1 } else { 0 };
    if !m.attachments.is_empty() {
        flags |= 0x10;
    }
    let mut p: Props = vec![
        (0x001A, Val::Str(m.class.clone())),
        (0x0037, Val::Str(m.subject.clone())),
        (0x0C1A, Val::Str(m.from.clone())),
        (0x0C1E, Val::Str("SMTP".into())),
        (0x0C1F, Val::Str(m.from_smtp.clone())),
        (0x0042, Val::Str(m.from.clone())),
        (0x0E04, Val::Str(m.to.iter().map(|t| t.0.clone()).collect::<Vec<_>>().join("; "))),
        (0x0E03, Val::Str(m.cc.iter().map(|t| t.0.clone()).collect::<Vec<_>>().join("; "))),
        (0x0E06, Val::Time(m.date)),
        (0x0039, Val::Time(m.date - 5000)),
        (0x0E07, Val::I32(flags)),
        (0x0E08, Val::I32(m.size)),
        (0x1000, Val::Str(m.body.clone())),
        (0x1035, Val::Str(format!("<{}@test>", m.nid))),
        (0x3FFD, Val::I32(1252)),
        (0x3FDE, Val::I32(65001)),
    ];
    if let Some(h) = &m.html {
        p.push((0x1013, Val::Bin(h.clone())));
    }
    if !m.categories.is_empty() {
        p.push((KEYWORDS_ID, Val::MultiStr(m.categories.clone())));
    }
    p.extend(m.extra.iter().cloned());
    p
}

/// Write message `m` either as a top-level node (`parent` = folder) or as a subnode; returns
/// (bid_data, bid_sub).
pub fn write_message(w: &mut Writer, m: &MsgSpec) -> (u64, u64) {
    let mut subs = Subs::new();
    // Recipient table.
    let rcols = [(0x0C15, PT_I32), (0x3001, PT_STR), (0x3002, PT_STR), (0x3003, PT_STR), (0x39FE, PT_STR)];
    let mut rrows = Vec::new();
    let recips = m.to.iter().map(|t| (1, t)).chain(m.cc.iter().map(|t| (2, t)));
    for (i, (ty, (n, e))) in recips.enumerate() {
        rrows.push((
            i as u32,
            vec![
                Some(Val::I32(ty)),
                Some(Val::Str(n.clone())),
                Some(Val::Str("SMTP".into())),
                Some(Val::Str(e.clone())),
                Some(Val::Str(e.clone())),
            ],
        ));
    }
    let (rbid, rsubs) = w.tc(&rcols, &rrows);
    let rsub = w.add_subnodes(rsubs);
    subs.push((0x692, rbid, rsub));

    // Attachments.
    if !m.attachments.is_empty() {
        let acols = [(0x0E20, PT_I32), (0x3704, PT_STR), (0x3705, PT_I32)];
        let mut arows = Vec::new();
        for a in &m.attachments {
            let anid = w.new_sub_nid(0x05);
            let (props, size, method, name, mut asubs) = match a {
                AttSpec::File { name, mime, data } => (
                    vec![
                        (0x3705, Val::I32(1)),
                        (0x3707, Val::Str(name.clone())),
                        (0x3704, Val::Str("SHORT~1.BIN".into())),
                        (0x370E, Val::Str(mime.clone())),
                        (0x0E20, Val::I32(data.len() as i32 + 100)),
                        (0x3701, Val::Bin(data.clone())),
                    ],
                    data.len() as i32,
                    1,
                    name.clone(),
                    Subs::new(),
                ),
                AttSpec::Embedded(inner) => {
                    let enid = w.new_sub_nid(0x04);
                    let (ebid, esub) = write_message(w, inner);
                    (
                        vec![
                            (0x3705, Val::I32(5)),
                            (0x3001, Val::Str(inner.subject.clone())),
                            (0x0E20, Val::I32(4096)),
                            (0x3701, Val::Obj(enid, 4096)),
                        ],
                        4096,
                        5,
                        inner.subject.clone(),
                        vec![(enid, ebid, esub)],
                    )
                }
            };
            let (abid, more) = w.pc(&props, Subs::new());
            asubs.extend(more);
            let asub = w.add_subnodes(asubs);
            subs.push((anid, abid, asub));
            arows.push((anid, vec![Some(Val::I32(size)), Some(Val::Str(name)), Some(Val::I32(method))]));
        }
        let (abid, asubs) = w.tc(&acols, &arows);
        let asub = w.add_subnodes(asubs);
        subs.push((0x671, abid, asub));
    }
    let (bid, subs) = w.pc(&msg_props(m), subs);
    let sub = w.add_subnodes(subs);
    (bid, sub)
}

fn contents_cols() -> Vec<(u16, u16)> {
    vec![
        (0x001A, PT_STR),
        (0x0037, PT_STR),
        (0x0042, PT_STR),
        (0x0E03, PT_STR),
        (0x0E04, PT_STR),
        (0x0E06, PT_TIME),
        (0x0E07, PT_I32),
        (0x0E08, PT_I32),
        (0x0039, PT_TIME),
        (KEYWORDS_ID, PT_MSTR),
        (APPT_START_ID, PT_TIME),
        (APPT_LOCATION_ID, PT_STR),
    ]
}

fn contents_row(m: &MsgSpec) -> (u32, Vec<Option<Val>>) {
    let props = msg_props(m);
    let get = |id: u16| props.iter().find(|p| p.0 == id).map(|p| p.1.clone());
    (m.nid, contents_cols().iter().map(|c| get(c.0)).collect())
}

pub fn write_folder(
    w: &mut Writer,
    nid: u32,
    parent: u32,
    name: &str,
    class: Option<&str>,
    children: &[u32],
    msgs: &[MsgSpec],
) {
    let mut props: Props = vec![
        (0x3001, Val::Str(name.into())),
        (0x3602, Val::I32(msgs.len() as i32)),
        (0x3603, Val::I32(msgs.iter().filter(|m| !m.read).count() as i32)),
        (0x360A, Val::Bool(!children.is_empty())),
    ];
    if let Some(c) = class {
        props.push((0x3613, Val::Str(c.into())));
    }
    let (bid, subs) = w.pc(&props, Subs::new());
    let sub = w.add_subnodes(subs);
    w.add_node(nid, bid, sub, parent);
    let hcols = [(0x3001, PT_STR)];
    let hrows: Vec<_> = children.iter().map(|&c| (c, vec![Some(Val::Str(format!("child {c:x}")))])).collect();
    let (hbid, hsubs) = w.tc(&hcols, &hrows);
    let hsub = w.add_subnodes(hsubs);
    w.add_node((nid & !0x1F) | 0x0D, hbid, hsub, parent);
    let rows: Vec<_> = msgs.iter().map(contents_row).collect();
    let (cbid, csubs) = w.tc(&contents_cols(), &rows);
    let csub = w.add_subnodes(csubs);
    w.add_node((nid & !0x1F) | 0x0E, cbid, csub, parent);
    let n = if w.contents_only { 0 } else { msgs.len() };
    for m in &msgs[..n] {
        let (b, s) = write_message(w, m);
        w.add_node(m.nid, b, s, nid);
    }
}

/// The reference store used by the synthetic tests.
pub struct Fixture {
    pub inbox: Vec<MsgSpec>,
    pub bulk: Vec<MsgSpec>,
    pub calendar: Vec<MsgSpec>,
}

pub fn big_attachment() -> Vec<u8> {
    // > 2 blocks even in the 4K format, compressible but not trivially.
    (0..150_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8 ^ (i / 1000) as u8).collect()
}

pub fn fixture(bulk_count: usize) -> Fixture {
    let inner_inner = MsgSpec {
        nid: 0,
        subject: "Level 2".into(),
        attachments: vec![AttSpec::File { name: "deep.txt".into(), mime: "text/plain".into(), data: b"deep".to_vec() }],
        ..simple_msg(0, 9999)
    };
    let inner = MsgSpec {
        nid: 0,
        subject: "Forwarded inner".into(),
        from: "Inner Sender".into(),
        body: "inner body".into(),
        attachments: vec![AttSpec::Embedded(Box::new(inner_inner))],
        ..simple_msg(0, 9998)
    };
    let inbox = vec![
        MsgSpec {
            nid: 0x200004,
            subject: "Quarterly report".into(),
            from: "Alice Smith".into(),
            from_smtp: "alice@example.com".into(),
            to: vec![("Bob Jones".into(), "bob@example.com".into()), ("Carol".into(), "carol@example.com".into())],
            cc: vec![("Dave".into(), "dave@example.com".into())],
            date: T0 + 3_600_000,
            size: 123_456,
            read: false,
            body: "See attached report. ".repeat(400), // > 3580 bytes: stored in a subnode
            html: Some("<html><body><p>See attached caf\u{e9}</p></body></html>".as_bytes().to_vec()),
            categories: vec!["Red".into(), "Finance".into()],
            attachments: vec![
                AttSpec::File {
                    name: "report.bin".into(),
                    mime: "application/octet-stream".into(),
                    data: big_attachment(),
                },
                AttSpec::File {
                    name: "note.txt".into(),
                    mime: "text/plain".into(),
                    data: b"hello attachment".to_vec(),
                },
                AttSpec::Embedded(Box::new(inner)),
            ],
            class: "IPM.Note".into(),
            extra: vec![],
        },
        MsgSpec {
            nid: 0x200024,
            subject: "RE: Lunch?".into(),
            from: "bob jones".into(),
            from_smtp: "bob@example.com".into(),
            date: T0,
            size: 900,
            ..simple_msg(0x200024, 1)
        },
    ];
    let bulk = (0..bulk_count).map(|i| simple_msg(0x200044 + (i as u32) * 0x20, i)).collect();
    // Created (delivered) before the second one but starting after it: Start and Date
    // orders differ.
    let appointment = |nid: u32, subject: &str, created: i64, start: i64, location: &str| MsgSpec {
        subject: subject.into(),
        date: created,
        to: vec![("Bob Jones".into(), "bob@example.com".into())],
        cc: vec![("Carol".into(), "carol@example.com".into())],
        class: "IPM.Appointment".into(),
        extra: vec![
            (APPT_START_ID, Val::Time(start)),
            (APPT_END_ID, Val::Time(start + 3_600_000)),
            (APPT_LOCATION_ID, Val::Str(location.into())),
        ],
        ..simple_msg(nid, 7000)
    };
    // A task: its due date is no contents-table column.
    let task = MsgSpec {
        subject: "File report".into(),
        date: T0 + 2 * 86_400_000,
        class: "IPM.Task".into(),
        extra: vec![(TASK_DUE_ID, Val::Time(task_due())), (TASK_OWNER_ID, Val::Str("Terry Task".into()))],
        ..simple_msg(0x300044, 7001)
    };
    let calendar = vec![
        appointment(0x300004, "Board meeting", T0, T0 + 30 * 86_400_000, "Room 4C"),
        appointment(0x300024, "Dentist", T0 + 86_400_000, T0 + 2 * 86_400_000, "Main St"),
        task,
    ];
    Fixture { inbox, bulk, calendar }
}

/// Due date of the fixture task: midnight (as if UTC) ten days after [`T0`].
pub fn task_due() -> i64 {
    (T0.div_euclid(86_400_000) + 10) * 86_400_000
}

/// Build the reference store in the given format.
pub fn build_store(mut w: Writer, fx: &Fixture, client: &[u8; 2]) -> Vec<u8> {
    // Store.
    let mut eid = vec![0u8; 20];
    eid.extend_from_slice(&NID_IPM.to_le_bytes());
    let store_props: Props = vec![(0x3001, Val::Str("Synthetic Store".into())), (0x35E0, Val::Bin(eid))];
    let (b, s) = w.pc(&store_props, Subs::new());
    let s = w.add_subnodes(s);
    w.add_node(NID_STORE, b, s, 0);
    // Name map: one string-named property (Keywords in PS_PUBLIC_STRINGS) → 0x8000.
    let mut strings = Vec::new();
    let kw = utf16("Keywords");
    strings.extend_from_slice(&(kw.len() as u32).to_le_bytes());
    strings.extend_from_slice(&kw);
    let mut entry = Vec::new();
    entry.extend_from_slice(&0u32.to_le_bytes()); // offset into string stream
    entry.extend_from_slice(&((2u16 << 1) | 1).to_le_bytes()); // PS_PUBLIC_STRINGS, named
    entry.extend_from_slice(&0u16.to_le_bytes()); // prop idx 0 → 0x8000
                                                  // A LID-named property in a custom GUID stream entry (index 3 → first stream GUID).
    entry.extend_from_slice(&0x8516u32.to_le_bytes());
    entry.extend_from_slice(&(3u16 << 1).to_le_bytes());
    entry.extend_from_slice(&1u16.to_le_bytes());
    // Appointment LIDs in the second stream GUID (index 4) → 0x8002.. (APPT_*_ID).
    for (idx, lid) in [(2u16, 0x820Du32), (3, 0x820E), (4, 0x8208)] {
        entry.extend_from_slice(&lid.to_le_bytes());
        entry.extend_from_slice(&(4u16 << 1).to_le_bytes());
        entry.extend_from_slice(&idx.to_le_bytes());
    }
    // Task LIDs in the third stream GUID (index 5) → 0x8005.. (TASK_*_ID).
    for (idx, lid) in [(5u16, 0x8105u32), (6, 0x811F)] {
        entry.extend_from_slice(&lid.to_le_bytes());
        entry.extend_from_slice(&(5u16 << 1).to_le_bytes());
        entry.extend_from_slice(&idx.to_le_bytes());
    }
    let mut guids = mapi_props::names::PSETID_COMMON.to_vec();
    guids.extend_from_slice(&mapi_props::names::PSETID_APPOINTMENT);
    guids.extend_from_slice(&mapi_props::names::PSETID_TASK);
    let nm: Props = vec![
        (0x0001, Val::I32(251)),
        (0x0002, Val::Bin(guids)),
        (0x0003, Val::Bin(entry)),
        (0x0004, Val::Bin(strings)),
    ];
    let (b, s) = w.pc(&nm, Subs::new());
    let s = w.add_subnodes(s);
    w.add_node(NID_NAMEMAP, b, s, 0);

    write_folder(&mut w, NID_ROOT, NID_ROOT, "", None, &[NID_IPM, NID_SEARCH_ROOT], &[]);
    write_folder(&mut w, NID_IPM, NID_ROOT, "Top of Personal Folders", None, &[NID_INBOX, NID_CAL], &[]);
    write_folder(&mut w, NID_INBOX, NID_IPM, "Inbox", Some("IPF.Note"), &[NID_BULK], &fx.inbox);
    write_folder(&mut w, NID_BULK, NID_INBOX, "Bulk", Some("IPF.Note"), &[], &fx.bulk);
    write_folder(&mut w, NID_CAL, NID_IPM, "Calendar", Some("IPF.Appointment"), &[], &fx.calendar);
    write_folder(&mut w, NID_SEARCH_ROOT, NID_ROOT, "Search Root", None, &[], &[]);
    w.finish(client)
}

/// Deterministic xorshift PRNG for corruption tests.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}
