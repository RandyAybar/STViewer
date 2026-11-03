//! Mailboxes split across several PST/OST files (`name.001.pst`, `name.002.pst`, ...),
//! as produced by Microsoft Purview / eDiscovery exports and size-capped archiving.
//!
//! [`MultiPartSource`] presents the parts as one source: folder trees are merged by
//! folder path, counts are summed, and sorted pages are merged across parts. A merged
//! page never loads more than the rows it returns: the merge order of each folder and
//! sort is computed once (one byte-pair per message) and every part keeps its own order,
//! so the rows a page needs from one part are always a contiguous range of that part.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mail_model::{
    Error, Fingerprint, Folder, FolderId, FolderKind, Header, Locator, MailSource, Message, PropertyView, Result, Sort,
    SortKey, SourceKind,
};

use crate::{discover, lock};

/// A file of a split archive: its group key (lowercased name without the part
/// number) and the part number. Recognises `<base><sep><1-4 digits>.pst|ost` with
/// `sep` one of `.` `_` `-` or space.
pub(crate) fn part_key(path: &Path) -> Option<(String, u32)> {
    let (base, ext, n) = split_name(path)?;
    Some((format!("{}{}", base.to_lowercase(), ext.to_lowercase()), n))
}

/// `(base, ".pst"/".ost", part number)` with `base` in its original case.
fn split_name(path: &Path) -> Option<(&str, &str, u32)> {
    let name = path.file_name()?.to_str()?;
    // `split_at_checked`: a name ending in multi-byte characters has no 4-byte ASCII suffix.
    let (stem, ext) = name.split_at_checked(name.len().checked_sub(4)?)?;
    if !ext.eq_ignore_ascii_case(".pst") && !ext.eq_ignore_ascii_case(".ost") {
        return None;
    }
    let digits_start = stem.len() - stem.bytes().rev().take_while(u8::is_ascii_digit).count();
    let digits = &stem[digits_start..];
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let sep = stem[..digits_start].chars().next_back()?;
    if !matches!(sep, '.' | '_' | '-' | ' ') {
        return None;
    }
    let base = &stem[..digits_start - sep.len_utf8()];
    if base.is_empty() {
        return None;
    }
    Some((base, ext, digits.parse().ok()?))
}

/// A discovered source: one file/directory, or the files of one split archive.
pub(crate) enum Found {
    One(PathBuf, SourceKind),
    Parts(Vec<PathBuf>, SourceKind),
}

/// Group PST/OST files that are parts of one archive. A group needs at least two
/// files numbered contiguously from 0 or 1 with no duplicates, so independent
/// archives that merely end in a number (`Archive_2023.pst`, `Archive_2024.pst`)
/// stay separate. Parts may live in different directories (Purview spreads them over
/// `PSTs.001.*`, `PSTs.002.*`, ...). Output keeps discovery order; a group appears
/// where its first file was found.
pub(crate) fn group(found: Vec<(PathBuf, SourceKind)>) -> Vec<Found> {
    let mut by_key: HashMap<String, Vec<(u32, usize)>> = HashMap::new();
    for (i, (p, k)) in found.iter().enumerate() {
        if matches!(k, SourceKind::Pst | SourceKind::Ost) {
            if let Some((key, n)) = part_key(p) {
                by_key.entry(key).or_default().push((n, i));
            }
        }
    }
    // For each grouped file: the group's member indices (in part order), stored at the
    // first-discovered member; `usize::MAX` marks the other members.
    let mut leader: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut follower = vec![false; found.len()];
    for (_, mut v) in by_key {
        v.sort_unstable();
        let contiguous = v.len() >= 2 && v[0].0 <= 1 && v.windows(2).all(|w| w[1].0 == w[0].0 + 1);
        if !contiguous {
            continue;
        }
        let first = v.iter().map(|&(_, i)| i).min().expect("non-empty group");
        for &(_, i) in &v {
            follower[i] = i != first;
        }
        leader.insert(first, v.into_iter().map(|(_, i)| i).collect());
    }
    let mut out = Vec::with_capacity(found.len());
    for (i, (p, k)) in found.iter().enumerate() {
        if follower[i] {
            continue;
        }
        match leader.get(&i) {
            Some(members) => out.push(Found::Parts(members.iter().map(|&m| found[m].0.clone()).collect(), *k)),
            None => out.push(Found::One(p.clone(), *k)),
        }
    }
    out
}

/// Merged folder tree of all parts.
struct Tree {
    /// Parents before children; index 0 is the merged root (`FolderId("")`).
    folders: Vec<Folder>,
    /// Per merged folder: the `(part, part folder id)` pairs it is made of.
    members: Vec<Vec<(usize, FolderId)>>,
    index: HashMap<FolderId, usize>,
    /// Per part: part folder id → merged folder index.
    to_merged: Vec<HashMap<FolderId, usize>>,
}

impl Tree {
    fn lookup(&self, id: &FolderId) -> Result<usize> {
        self.index.get(id).copied().ok_or_else(|| Error::NotFound(format!("folder {:?}", id.0)))
    }

    fn map_header(&self, part: usize, mut h: Header) -> Header {
        h.locator = h.locator.map(|l| Locator::Part { part: part as u32, inner: Box::new(l) });
        h.folder = h.folder.map(|f| match self.to_merged[part].get(&f) {
            Some(&i) => self.folders[i].id.clone(),
            None => f,
        });
        h
    }
}

/// Folder path segment; `/` separates segments in merged folder ids.
fn escape_segment(name: &str) -> String {
    name.replace('%', "%25").replace('/', "%2F")
}

pub(crate) struct MultiPartSource {
    parts: Vec<Box<dyn MailSource>>,
    paths: Vec<PathBuf>,
    kind: SourceKind,
    name: String,
    tree: Mutex<Option<Arc<Tree>>>,
    /// Merge order per (merged folder, sort key discriminant, descending): the member
    /// index of each merged row.
    orders: Mutex<HashMap<OrderKey, Arc<Vec<u16>>>>,
}

/// `(merged folder index, sort key discriminant, descending)`.
type OrderKey = (usize, u8, bool);

impl MultiPartSource {
    /// Open every part (in parallel; each open reads only headers and B-tree roots).
    pub(crate) fn open(paths: &[PathBuf], kind: SourceKind) -> Result<MultiPartSource> {
        if paths.len() > usize::from(u16::MAX) {
            return Err(Error::Unsupported(format!("{} parts is more than supported", paths.len())));
        }
        let opened: Vec<Result<Box<dyn MailSource>>> = std::thread::scope(|s| {
            // Parts are PST/OST, so the mbox opener (and its cache location) is never used.
            let handles: Vec<_> =
                paths.iter().map(|p| s.spawn(move || discover::open_kind(p, kind, stv_mbox::opener()))).collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(Error::Other("part reader panicked".into()))))
                .collect()
        });
        let mut parts = Vec::with_capacity(opened.len());
        for (p, r) in paths.iter().zip(opened) {
            parts.push(r.map_err(|e| match e {
                Error::NotFound(_) => Error::NotFound(format!("archive part missing: {}", p.display())),
                e => e,
            })?);
        }
        let base = split_name(&paths[0]).map_or_else(
            || paths[0].file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            |(b, _, _)| b.to_owned(),
        );
        Ok(MultiPartSource {
            name: format!("{base} ({} parts)", paths.len()),
            parts,
            paths: paths.to_vec(),
            kind,
            tree: Mutex::new(None),
            orders: Mutex::new(HashMap::new()),
        })
    }

    fn tree(&self) -> Result<Arc<Tree>> {
        let mut slot = lock(&self.tree);
        if let Some(t) = slot.as_ref() {
            return Ok(Arc::clone(t));
        }
        let t = Arc::new(self.build_tree()?);
        *slot = Some(Arc::clone(&t));
        Ok(t)
    }

    fn build_tree(&self) -> Result<Tree> {
        let root = FolderId(String::new());
        let mut folders = vec![Folder {
            id: root.clone(),
            parent: None,
            name: self.name.clone(),
            kind: FolderKind::Mail,
            message_count: Some(0),
            unread_count: Some(0),
        }];
        let mut members: Vec<Vec<(usize, FolderId)>> = vec![Vec::new()];
        let mut index = HashMap::from([(root, 0usize)]);
        let mut to_merged = Vec::with_capacity(self.parts.len());

        for (p, src) in self.parts.iter().enumerate() {
            let mut map: HashMap<FolderId, usize> = HashMap::new();
            for f in src.folders()? {
                // A part's root (and anything whose parent is unknown) folds into the merged root.
                let parent = f.parent.as_ref().and_then(|par| map.get(par).copied());
                let m = match parent {
                    None => 0,
                    Some(pi) => {
                        let seg = escape_segment(&f.name);
                        let parent_id = &folders[pi].id.0;
                        let id = FolderId(if parent_id.is_empty() { seg } else { format!("{parent_id}/{seg}") });
                        match index.get(&id) {
                            Some(&m) => m,
                            None => {
                                folders.push(Folder {
                                    id: id.clone(),
                                    parent: Some(folders[pi].id.clone()),
                                    name: f.name.clone(),
                                    kind: f.kind,
                                    message_count: Some(0),
                                    unread_count: Some(0),
                                });
                                members.push(Vec::new());
                                index.insert(id, folders.len() - 1);
                                folders.len() - 1
                            }
                        }
                    }
                };
                let merged = &mut folders[m];
                merged.message_count = merged.message_count.zip(f.message_count).map(|(a, b)| a + b);
                merged.unread_count = merged.unread_count.zip(f.unread_count).map(|(a, b)| a + b);
                members[m].push((p, f.id.clone()));
                map.insert(f.id, m);
            }
            to_merged.push(map);
        }
        Ok(Tree { folders, members, index, to_merged })
    }

    fn split(&self, loc: &Locator) -> Result<(usize, Locator)> {
        let (p, inner) = match loc {
            Locator::Part { part, inner } => (*part as usize, (**inner).clone()),
            Locator::Embedded { parent, attachment } => {
                let (p, parent) = self.split(parent)?;
                (p, Locator::Embedded { parent: Box::new(parent), attachment: *attachment })
            }
            other => return Err(Error::NotFound(format!("locator {other:?} names no archive part"))),
        };
        if p >= self.parts.len() {
            return Err(Error::NotFound(format!("archive part {p} of {}", self.parts.len())));
        }
        Ok((p, inner))
    }

    /// Merge order of a folder under `sort` (cached): fetches each member's full sorted
    /// header list once, keeps only the sort keys, and k-way merges them.
    fn order(&self, idx: usize, members: &[(usize, FolderId)], counts: &[u64], sort: Sort) -> Result<Arc<Vec<u16>>> {
        let key = (idx, sort.key as u8, sort.descending);
        let total: u64 = counts.iter().sum();
        let cached = lock(&self.orders).get(&key).cloned();
        if let Some(o) = cached.filter(|o| o.len() as u64 == total) {
            return Ok(o);
        }
        let keyed: Vec<Result<Vec<SortVal>>> = std::thread::scope(|s| {
            let handles: Vec<_> = members
                .iter()
                .zip(counts)
                .map(|((p, f), &c)| {
                    let src = &self.parts[*p];
                    s.spawn(move || -> Result<Vec<SortVal>> {
                        Ok(src.list(f, 0..c, sort)?.iter().map(|h| sort_val(h, sort.key)).collect())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(Error::Other("part listing panicked".into()))))
                .collect()
        });
        let keyed = keyed.into_iter().collect::<Result<Vec<_>>>()?;

        let mut pos = vec![0usize; keyed.len()];
        let mut order = Vec::with_capacity(keyed.iter().map(Vec::len).sum());
        loop {
            let mut best: Option<usize> = None;
            for (m, keys) in keyed.iter().enumerate() {
                let Some(k) = keys.get(pos[m]) else { continue };
                let better = match best {
                    None => true,
                    Some(b) => {
                        let ord = k.cmp(&keyed[b][pos[b]]);
                        if sort.descending {
                            ord == Ordering::Greater
                        } else {
                            ord == Ordering::Less
                        }
                    }
                };
                if better {
                    best = Some(m);
                }
            }
            let Some(m) = best else { break };
            order.push(m as u16);
            pos[m] += 1;
        }
        let order = Arc::new(order);
        lock(&self.orders).insert(key, Arc::clone(&order));
        Ok(order)
    }
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortVal {
    Int(i64),
    Text(String),
}

fn sort_val(h: &Header, key: SortKey) -> SortVal {
    match key {
        SortKey::Date | SortKey::Native => SortVal::Int(h.date.unwrap_or(i64::MIN)),
        SortKey::Start => SortVal::Int(h.start_ms().unwrap_or(i64::MIN)),
        SortKey::Size => SortVal::Int(i64::try_from(h.size).unwrap_or(i64::MAX)),
        SortKey::From => SortVal::Text(
            h.from
                .as_ref()
                .and_then(|a| a.name.as_deref().filter(|n| !n.trim().is_empty()).or(a.email.as_deref()))
                .unwrap_or("")
                .trim()
                .to_lowercase(),
        ),
        SortKey::Subject => SortVal::Text(h.subject.trim().to_lowercase()),
    }
}

impl MailSource for MultiPartSource {
    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn display_name(&self) -> String {
        self.name.clone()
    }

    /// Combined identity: total size, newest mtime, and a hash over every part's
    /// fingerprint, so a change to any part (or the part list) re-syncs the index.
    fn fingerprint(&self) -> Result<Fingerprint> {
        let mut size = 0u64;
        let mut mtime = i64::MIN;
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for src in &self.parts {
            let f = src.fingerprint()?;
            size += f.size;
            mtime = mtime.max(f.mtime);
            for v in [f.head_hash, f.size] {
                hash = (hash ^ v).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        Ok(Fingerprint { path: self.paths[0].clone(), size, mtime, head_hash: hash })
    }

    fn folders(&self) -> Result<Vec<Folder>> {
        Ok(self.tree()?.folders.clone())
    }

    fn count(&self, folder: &FolderId) -> Result<u64> {
        let tree = self.tree()?;
        let idx = tree.lookup(folder)?;
        tree.members[idx].iter().map(|(p, f)| self.parts[*p].count(f)).sum()
    }

    fn list(&self, folder: &FolderId, range: Range<u64>, sort: Sort) -> Result<Vec<Header>> {
        let tree = self.tree()?;
        let idx = tree.lookup(folder)?;
        let members = &tree.members[idx];
        let counts = members.iter().map(|(p, f)| self.parts[*p].count(f)).collect::<Result<Vec<u64>>>()?;
        let total: u64 = counts.iter().sum();
        let range = range.start.min(total)..range.end.min(total);
        if range.is_empty() {
            return Ok(Vec::new());
        }

        if sort.key == SortKey::Native || members.len() == 1 {
            // Part order: the folder's rows from part 0, then part 1, ...
            let mut out = Vec::with_capacity((range.end - range.start) as usize);
            let mut base = 0;
            for ((p, f), &c) in members.iter().zip(&counts) {
                let (s, e) = (range.start.max(base), range.end.min(base + c));
                if s < e {
                    out.extend(
                        self.parts[*p].list(f, s - base..e - base, sort)?.into_iter().map(|h| tree.map_header(*p, h)),
                    );
                }
                base += c;
            }
            return Ok(out);
        }

        let order = self.order(idx, members, &counts, sort)?;
        let (start, end) = (range.start as usize, range.end as usize);
        let mut skip = vec![0u64; members.len()];
        for &m in &order[..start] {
            skip[m as usize] += 1;
        }
        let mut take = vec![0u64; members.len()];
        for &m in &order[start..end] {
            take[m as usize] += 1;
        }
        let mut pages = Vec::with_capacity(members.len());
        for (m, (p, f)) in members.iter().enumerate() {
            let rows =
                if take[m] == 0 { Vec::new() } else { self.parts[*p].list(f, skip[m]..skip[m] + take[m], sort)? };
            pages.push(rows.into_iter());
        }
        let mut out = Vec::with_capacity(end - start);
        for &m in &order[start..end] {
            if let Some(h) = pages[m as usize].next() {
                out.push(tree.map_header(members[m as usize].0, h));
            }
        }
        Ok(out)
    }

    fn iter_headers(&self) -> Box<dyn Iterator<Item = Result<Header>> + Send + '_> {
        let tree = match self.tree() {
            Ok(t) => t,
            Err(e) => return Box::new(std::iter::once(Err(e))),
        };
        Box::new(self.parts.iter().enumerate().flat_map(move |(p, src)| {
            let tree = Arc::clone(&tree);
            src.iter_headers().map(move |r| r.map(|h| tree.map_header(p, h)))
        }))
    }

    fn message(&self, loc: &Locator) -> Result<Message> {
        let (p, inner) = self.split(loc)?;
        let mut m = self.parts[p].message(&inner)?;
        m.header = self.tree()?.map_header(p, m.header);
        Ok(m)
    }

    fn attachment(&self, loc: &Locator, index: u32) -> Result<Vec<u8>> {
        let (p, inner) = self.split(loc)?;
        self.parts[p].attachment(&inner, index)
    }

    fn raw_mime(&self, loc: &Locator) -> Result<Option<Vec<u8>>> {
        let (p, inner) = self.split(loc)?;
        self.parts[p].raw_mime(&inner)
    }

    fn properties(&self, loc: &Locator) -> Result<Vec<PropertyView>> {
        let (p, inner) = self.split(loc)?;
        self.parts[p].properties(&inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_names() {
        let k = |s: &str| part_key(Path::new(s));
        assert_eq!(k("user@example.com.001.pst"), Some(("user@example.com.pst".into(), 1)));
        assert_eq!(k("Mail_02.OST"), Some(("mail.ost".into(), 2)));
        assert_eq!(k("box-0.pst"), Some(("box.pst".into(), 0)));
        assert_eq!(k("backup 3.pst"), Some(("backup.pst".into(), 3)));
        assert_eq!(k("archive.pst"), None);
        assert_eq!(k("archive2.pst"), None, "number needs a separator");
        assert_eq!(k("x.12345.pst"), None, "at most 4 digits");
        assert_eq!(k(".001.pst"), None, "needs a base name");
        assert_eq!(k("mail.001.mbox"), None);
        // PSTs are sniffed by content, so any name reaches here; multi-byte endings must not panic.
        assert_eq!(k("メール"), None);
        assert_eq!(k("x日本"), None);
        assert_eq!(k("日本 1.pst"), Some(("日本.pst".into(), 1)));
    }

    fn found(names: &[&str]) -> Vec<(PathBuf, SourceKind)> {
        names.iter().map(|n| (PathBuf::from(n), SourceKind::Pst)).collect()
    }

    fn shape(v: Vec<Found>) -> Vec<Vec<String>> {
        v.into_iter()
            .map(|f| match f {
                Found::One(p, _) => vec![p.display().to_string()],
                Found::Parts(ps, _) => ps.iter().map(|p| p.display().to_string()).collect(),
            })
            .collect()
    }

    #[test]
    fn grouping() {
        // Parts in different directories group in part order, at the first part's position.
        let g = group(found(&["d2/a.002.pst", "other.pst", "d1/a.001.pst", "d3/a.003.pst"]));
        assert_eq!(shape(g), vec![vec!["d1/a.001.pst", "d2/a.002.pst", "d3/a.003.pst"], vec!["other.pst"]]);
        // Year-numbered archives are independent.
        assert_eq!(shape(group(found(&["Archive_2023.pst", "Archive_2024.pst"]))).len(), 2);
        // A gap or a duplicate part number means it is not one split archive.
        assert_eq!(shape(group(found(&["b.001.pst", "b.003.pst"]))).len(), 2);
        assert_eq!(shape(group(found(&["x/c.001.pst", "y/c.001.pst", "c.002.pst"]))).len(), 3);
        // A single numbered file stays single.
        assert_eq!(shape(group(found(&["d.001.pst"]))), vec![vec!["d.001.pst"]]);
        // Case-insensitive grouping.
        assert_eq!(shape(group(found(&["E.001.PST", "e.002.pst"]))).len(), 1);
    }
}
