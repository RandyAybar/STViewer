//! Test-only writers for `.msg` ([MS-OXMSG]) and TNEF ([MS-OXTNEF]) files, used to build
//! fixtures with exactly known content.
#![allow(dead_code)]

use std::io::{Cursor, Write};

use cfb::CompoundFile;

pub const PS_PUBLIC_STRINGS: [u8; 16] = ms_guid(0x0002_0329);
pub const PSETID_COMMON: [u8; 16] = ms_guid(0x0006_2008);
pub const IID_IMESSAGE: [u8; 16] = ms_guid(0x0002_0307);

pub const fn ms_guid(d1: u32) -> [u8; 16] {
    let b = d1.to_le_bytes();
    [b[0], b[1], b[2], b[3], 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46]
}

/// FILETIME for a Unix ms timestamp.
pub fn filetime(unix_ms: i64) -> u64 {
    ((unix_ms + 11_644_473_600_000) * 10_000) as u64
}

pub fn utf16z(s: &str) -> Vec<u8> {
    let mut v: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    v.extend_from_slice(&[0, 0]);
    v
}

#[derive(Clone, Debug)]
pub enum V {
    I16(i16),
    I32(i32),
    Bool(bool),
    F64(f64),
    I64(i64),
    Time(u64),
    Unicode(String),
    /// Raw 8-bit bytes (without terminator).
    String8(Vec<u8>),
    Binary(Vec<u8>),
    Guid([u8; 16]),
    MultiI32(Vec<i32>),
    MultiUnicode(Vec<String>),
    MultiString8(Vec<Vec<u8>>),
    MultiBinary(Vec<Vec<u8>>),
}

impl V {
    pub fn ty(&self) -> u16 {
        match self {
            V::I16(_) => 0x0002,
            V::I32(_) => 0x0003,
            V::Bool(_) => 0x000B,
            V::F64(_) => 0x0005,
            V::I64(_) => 0x0014,
            V::Time(_) => 0x0040,
            V::Unicode(_) => 0x001F,
            V::String8(_) => 0x001E,
            V::Binary(_) => 0x0102,
            V::Guid(_) => 0x0048,
            V::MultiI32(_) => 0x1003,
            V::MultiUnicode(_) => 0x101F,
            V::MultiString8(_) => 0x101E,
            V::MultiBinary(_) => 0x1102,
        }
    }

    fn fixed(&self) -> Option<[u8; 8]> {
        let mut b = [0u8; 8];
        match self {
            V::I16(v) => b[..2].copy_from_slice(&v.to_le_bytes()),
            V::I32(v) => b[..4].copy_from_slice(&v.to_le_bytes()),
            V::Bool(v) => b[0] = *v as u8,
            V::F64(v) => b.copy_from_slice(&v.to_le_bytes()),
            V::I64(v) => b.copy_from_slice(&v.to_le_bytes()),
            V::Time(v) => b.copy_from_slice(&v.to_le_bytes()),
            _ => return None,
        }
        Some(b)
    }
}

/// Property key used by the builders: a fixed id or a named property.
#[derive(Clone, Debug, PartialEq)]
pub enum K {
    Id(u16),
    Lid([u8; 16], u32),
    Name([u8; 16], String),
}

#[derive(Clone, Debug, Default)]
pub struct MsgB {
    pub props: Vec<(K, V)>,
    pub recipients: Vec<Vec<(K, V)>>,
    pub attachments: Vec<AttB>,
}

#[derive(Clone, Debug)]
pub enum AttB {
    File(Vec<(K, V)>),
    /// Attachment props plus the embedded message (PidTagAttachMethod 5 is added).
    Embedded(Vec<(K, V)>, MsgB),
}

impl MsgB {
    pub fn prop(mut self, id: u16, v: V) -> Self {
        self.props.push((K::Id(id), v));
        self
    }
    pub fn named(mut self, k: K, v: V) -> Self {
        self.props.push((k, v));
        self
    }
    pub fn with_attachment(mut self, a: AttB) -> Self {
        self.attachments.push(a);
        self
    }
    pub fn with_recipient(mut self, r: Vec<(K, V)>) -> Self {
        self.recipients.push(r);
        self
    }
}

#[derive(PartialEq)]
enum Kind {
    Top,
    Embedded,
}

struct Names {
    keys: Vec<K>,
}

impl Names {
    fn tag(&mut self, k: &K, ty: u16) -> u32 {
        let id = match k {
            K::Id(id) => *id,
            other => {
                let i = match self.keys.iter().position(|x| x == other) {
                    Some(i) => i,
                    None => {
                        self.keys.push(other.clone());
                        self.keys.len() - 1
                    }
                };
                0x8000 + i as u16
            }
        };
        (id as u32) << 16 | ty as u32
    }
}

fn collect_names(m: &MsgB, n: &mut Names) {
    for (k, v) in &m.props {
        n.tag(k, v.ty());
    }
    for r in &m.recipients {
        for (k, v) in r {
            n.tag(k, v.ty());
        }
    }
    for a in &m.attachments {
        let (AttB::File(p) | AttB::Embedded(p, _)) = a;
        for (k, v) in p {
            n.tag(k, v.ty());
        }
        if let AttB::Embedded(_, e) = a {
            collect_names(e, n);
        }
    }
}

type Cfb = CompoundFile<Cursor<Vec<u8>>>;

fn stream(cfb: &mut Cfb, path: &str, data: &[u8]) {
    let mut s = cfb.create_stream(path).unwrap();
    s.write_all(data).unwrap();
}

fn write_props(cfb: &mut Cfb, dir: &str, props: &[(K, V)], names: &mut Names, header: Vec<u8>) {
    let mut ps = header;
    for (k, v) in props {
        let tag = names.tag(k, v.ty());
        ps.extend_from_slice(&tag.to_le_bytes());
        ps.extend_from_slice(&6u32.to_le_bytes());
        if let Some(b) = v.fixed() {
            ps.extend_from_slice(&b);
            continue;
        }
        let name = format!("{dir}/__substg1.0_{tag:08X}");
        let size: u32 = match v {
            V::Unicode(s) => {
                let b = utf16z(s);
                stream(cfb, &name, &b);
                b.len() as u32
            }
            V::String8(s) => {
                let mut b = s.clone();
                b.push(0);
                stream(cfb, &name, &b);
                b.len() as u32
            }
            V::Binary(b) => {
                stream(cfb, &name, b);
                b.len() as u32
            }
            V::Guid(g) => {
                stream(cfb, &name, g);
                16
            }
            V::MultiI32(vs) => {
                let b: Vec<u8> = vs.iter().flat_map(|v| v.to_le_bytes()).collect();
                stream(cfb, &name, &b);
                b.len() as u32
            }
            V::MultiUnicode(vs) => {
                let mut lens = Vec::new();
                for (i, s) in vs.iter().enumerate() {
                    let b = utf16z(s);
                    lens.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    stream(cfb, &format!("{name}-{i:08X}"), &b);
                }
                stream(cfb, &name, &lens);
                lens.len() as u32
            }
            V::MultiString8(vs) => {
                let mut lens = Vec::new();
                for (i, s) in vs.iter().enumerate() {
                    let mut b = s.clone();
                    b.push(0);
                    lens.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    stream(cfb, &format!("{name}-{i:08X}"), &b);
                }
                stream(cfb, &name, &lens);
                lens.len() as u32
            }
            V::MultiBinary(vs) => {
                let mut lens = Vec::new();
                for (i, b) in vs.iter().enumerate() {
                    lens.extend_from_slice(&(b.len() as u32).to_le_bytes());
                    lens.extend_from_slice(&0u32.to_le_bytes());
                    stream(cfb, &format!("{name}-{i:08X}"), b);
                }
                stream(cfb, &name, &lens);
                lens.len() as u32
            }
            _ => unreachable!("fixed handled above"),
        };
        ps.extend_from_slice(&size.to_le_bytes());
        ps.extend_from_slice(&0u32.to_le_bytes());
    }
    stream(cfb, &format!("{dir}/__properties_version1.0"), &ps);
}

fn write_message(cfb: &mut Cfb, dir: &str, m: &MsgB, names: &mut Names, kind: Kind) {
    let mut header = vec![0u8; 8];
    for v in [m.recipients.len(), m.attachments.len(), m.recipients.len(), m.attachments.len()] {
        header.extend_from_slice(&(v as u32).to_le_bytes());
    }
    if kind == Kind::Top {
        header.extend_from_slice(&[0; 8]);
    }
    write_props(cfb, dir, &m.props, names, header);
    for (i, r) in m.recipients.iter().enumerate() {
        let d = format!("{dir}/__recip_version1.0_#{i:08X}");
        cfb.create_storage(&d).unwrap();
        write_props(cfb, &d, r, names, vec![0; 8]);
    }
    for (i, a) in m.attachments.iter().enumerate() {
        let d = format!("{dir}/__attach_version1.0_#{i:08X}");
        cfb.create_storage(&d).unwrap();
        match a {
            AttB::File(p) => write_props(cfb, &d, p, names, vec![0; 8]),
            AttB::Embedded(p, e) => {
                let mut p = p.clone();
                p.push((K::Id(0x3705), V::I32(5)));
                write_props(cfb, &d, &p, names, vec![0; 8]);
                // PidTagAttachDataObject entry pointing at the sub-storage.
                let ps_path = format!("{d}/__properties_version1.0");
                let mut ps = Vec::new();
                {
                    use std::io::Read;
                    cfb.open_stream(&ps_path).unwrap().read_to_end(&mut ps).unwrap();
                }
                ps.extend_from_slice(&0x3701_000Du32.to_le_bytes());
                ps.extend_from_slice(&6u32.to_le_bytes());
                ps.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
                ps.extend_from_slice(&0u32.to_le_bytes());
                stream(cfb, &ps_path, &ps);
                let sub = format!("{d}/__substg1.0_3701000D");
                cfb.create_storage(&sub).unwrap();
                write_message(cfb, &sub, e, names, Kind::Embedded);
            }
        }
    }
}

/// Serialise `m` as a `.msg` compound file.
pub fn build_msg(m: &MsgB) -> Vec<u8> {
    let mut names = Names { keys: Vec::new() };
    collect_names(m, &mut names);
    let mut cfb = CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    write_message(&mut cfb, "", m, &mut names, Kind::Top);

    // __nameid_version1.0: GUID stream (non-builtin GUIDs), entry stream, string stream.
    cfb.create_storage("/__nameid_version1.0").unwrap();
    let mut guids: Vec<[u8; 16]> = Vec::new();
    let mut entries = Vec::new();
    let mut strings = Vec::new();
    for (i, k) in names.keys.iter().enumerate() {
        let (guid, kind_bit, id_or_off) = match k {
            K::Lid(g, lid) => (*g, 0u32, *lid),
            K::Name(g, s) => {
                let off = strings.len() as u32;
                let b: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
                strings.extend_from_slice(&(b.len() as u32).to_le_bytes());
                strings.extend_from_slice(&b);
                while strings.len() % 4 != 0 {
                    strings.push(0);
                }
                (*g, 1, off)
            }
            K::Id(_) => unreachable!(),
        };
        let guid_index = if guid == ms_guid(0x0002_0328) {
            1
        } else if guid == PS_PUBLIC_STRINGS {
            2
        } else {
            let pos = guids.iter().position(|g| *g == guid).unwrap_or_else(|| {
                guids.push(guid);
                guids.len() - 1
            });
            3 + pos as u32
        };
        entries.extend_from_slice(&id_or_off.to_le_bytes());
        let info = (i as u32) << 16 | guid_index << 1 | kind_bit;
        entries.extend_from_slice(&info.to_le_bytes());
    }
    let g: Vec<u8> = guids.concat();
    stream(&mut cfb, "/__nameid_version1.0/__substg1.0_00020102", &g);
    stream(&mut cfb, "/__nameid_version1.0/__substg1.0_00030102", &entries);
    stream(&mut cfb, "/__nameid_version1.0/__substg1.0_00040102", &strings);
    cfb.flush().unwrap();
    cfb.into_inner().into_inner()
}

// ---------------------------------------------------------------- TNEF

pub const LVL_MSG: u8 = 1;
pub const LVL_ATT: u8 = 2;
pub const ATT_FROM: u32 = 0x0000_8000;
pub const ATT_SUBJECT: u32 = 0x0001_8004;
pub const ATT_DATE_SENT: u32 = 0x0003_8005;
pub const ATT_MESSAGE_STATUS: u32 = 0x0006_8007;
pub const ATT_MESSAGE_CLASS: u32 = 0x0007_8008;
pub const ATT_BODY: u32 = 0x0002_800C;
pub const ATT_PRIORITY: u32 = 0x0004_800D;
pub const ATT_ATTACH_DATA: u32 = 0x0006_800F;
pub const ATT_ATTACH_TITLE: u32 = 0x0001_8010;
pub const ATT_ATTACH_MODIFY_DATE: u32 = 0x0003_8013;
pub const ATT_ATTACH_REND_DATA: u32 = 0x0006_9002;
pub const ATT_MSG_PROPS: u32 = 0x0006_9003;
pub const ATT_RECIP_TABLE: u32 = 0x0006_9004;
pub const ATT_ATTACHMENT: u32 = 0x0006_9005;
pub const ATT_TNEF_VERSION: u32 = 0x0008_9006;
pub const ATT_OEM_CODEPAGE: u32 = 0x0006_9007;

#[derive(Default)]
pub struct TnefB {
    pub attrs: Vec<(u8, u32, Vec<u8>)>,
}

impl TnefB {
    pub fn new(oem_cp: u32) -> Self {
        let mut t = TnefB::default();
        t.attrs.push((LVL_MSG, ATT_TNEF_VERSION, 0x0001_0000u32.to_le_bytes().to_vec()));
        let mut cp = oem_cp.to_le_bytes().to_vec();
        cp.extend_from_slice(&[0; 4]);
        t.attrs.push((LVL_MSG, ATT_OEM_CODEPAGE, cp));
        t
    }
    pub fn attr(mut self, level: u8, id: u32, data: Vec<u8>) -> Self {
        self.attrs.push((level, id, data));
        self
    }
    pub fn build(&self) -> Vec<u8> {
        let mut out = 0x223E_9F78u32.to_le_bytes().to_vec();
        out.extend_from_slice(&1u16.to_le_bytes());
        for (lvl, id, data) in &self.attrs {
            out.push(*lvl);
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            let sum = data.iter().fold(0u16, |a, &b| a.wrapping_add(b as u16));
            out.extend_from_slice(&sum.to_le_bytes());
        }
        out
    }
}

pub fn sz(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

/// DTR structure for a UTC civil time.
pub fn dtr(y: u16, mo: u16, d: u16, h: u16, mi: u16, s: u16) -> Vec<u8> {
    [y, mo, d, h, mi, s, 1].iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// attFrom TRP structure.
pub fn trp(name: &str, addr: &str) -> Vec<u8> {
    let n = sz(name);
    let a = sz(addr);
    let mut v = Vec::new();
    v.extend_from_slice(&4u16.to_le_bytes());
    v.extend_from_slice(&((8 + n.len() + a.len() + 8) as u16).to_le_bytes());
    v.extend_from_slice(&(n.len() as u16).to_le_bytes());
    v.extend_from_slice(&(a.len() as u16).to_le_bytes());
    v.extend_from_slice(&n);
    v.extend_from_slice(&a);
    v.extend_from_slice(&[0; 8]);
    v
}

fn pad(v: &mut Vec<u8>) {
    v.resize(v.len().next_multiple_of(4), 0);
}

/// TNEF value for an Object property (`PidTagAttachDataObject`): IID + payload.
pub struct Obj(pub [u8; 16], pub Vec<u8>);

/// Encode a msgPropertyList ([MS-OXTNEF] 2.1.3.4). Objects use type 0x000D.
pub fn prop_list(props: &[(K, V)], objects: &[(u16, Obj)]) -> Vec<u8> {
    let mut out = ((props.len() + objects.len()) as u32).to_le_bytes().to_vec();
    let mut next_named = 0x8000u16;
    for (k, v) in props {
        let id = match k {
            K::Id(id) => *id,
            _ => {
                next_named += 1;
                next_named
            }
        };
        out.extend_from_slice(&((id as u32) << 16 | v.ty() as u32).to_le_bytes());
        match k {
            K::Id(_) => {}
            K::Lid(g, lid) => {
                out.extend_from_slice(g);
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&lid.to_le_bytes());
            }
            K::Name(g, s) => {
                out.extend_from_slice(g);
                out.extend_from_slice(&1u32.to_le_bytes());
                let b = utf16z(s);
                out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                out.extend_from_slice(&b);
                pad(&mut out);
            }
        }
        let var = |out: &mut Vec<u8>, vals: Vec<Vec<u8>>| {
            out.extend_from_slice(&(vals.len() as u32).to_le_bytes());
            for b in vals {
                out.extend_from_slice(&(b.len() as u32).to_le_bytes());
                out.extend_from_slice(&b);
                pad(out);
            }
        };
        match v {
            V::I16(x) => {
                out.extend_from_slice(&x.to_le_bytes());
                pad(&mut out);
            }
            V::Bool(x) => {
                out.extend_from_slice(&(*x as u16).to_le_bytes());
                pad(&mut out);
            }
            V::I32(x) => out.extend_from_slice(&x.to_le_bytes()),
            V::F64(x) => out.extend_from_slice(&x.to_le_bytes()),
            V::I64(x) => out.extend_from_slice(&x.to_le_bytes()),
            V::Time(x) => out.extend_from_slice(&x.to_le_bytes()),
            V::Guid(g) => out.extend_from_slice(g),
            V::Unicode(s) => var(&mut out, vec![utf16z(s)]),
            V::String8(s) => var(&mut out, vec![[s.as_slice(), &[0]].concat()]),
            V::Binary(b) => var(&mut out, vec![b.clone()]),
            V::MultiUnicode(vs) => var(&mut out, vs.iter().map(|s| utf16z(s)).collect()),
            V::MultiString8(vs) => var(&mut out, vs.iter().map(|s| [s.as_slice(), &[0]].concat()).collect()),
            V::MultiBinary(vs) => var(&mut out, vs.clone()),
            V::MultiI32(vs) => {
                out.extend_from_slice(&(vs.len() as u32).to_le_bytes());
                for x in vs {
                    out.extend_from_slice(&x.to_le_bytes());
                }
            }
        }
    }
    for (id, Obj(iid, data)) in objects {
        out.extend_from_slice(&((*id as u32) << 16 | 0x000D).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&((16 + data.len()) as u32).to_le_bytes());
        out.extend_from_slice(iid);
        out.extend_from_slice(data);
        pad(&mut out);
    }
    out
}

/// attRecipTable data.
pub fn recip_table(rows: &[Vec<(K, V)>]) -> Vec<u8> {
    let mut out = (rows.len() as u32).to_le_bytes().to_vec();
    for r in rows {
        out.extend_from_slice(&prop_list(r, &[]));
    }
    out
}

/// attAttachRendData for a file attachment.
pub fn rend_data_file() -> Vec<u8> {
    let mut v = 1u16.to_le_bytes().to_vec();
    v.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}
