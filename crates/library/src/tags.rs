//! User tags on emails and attachments, persisted as `tags.json` in the data directory.
//!
//! Each tagged item keeps a snapshot of its carrying message header (and attachment row)
//! taken when it was first tagged, so the Tagged view lists instantly without reopening
//! archives (which may sit on slow network shares).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use mail_model::{AttachmentRow, Error, Header, Locator, Result, SourceId};
use serde::{Deserialize, Serialize};

use crate::jsonfile;

pub(crate) const FILE_NAME: &str = "tags.json";
const VERSION: u32 = 1;

/// Colors handed to tags created without one, in order.
const PALETTE: [&str; 10] =
    ["#e5484d", "#f76b15", "#ffc53d", "#46a758", "#12a594", "#0090ff", "#3e63dd", "#8e4ec6", "#d6409f", "#8d8d8d"];

/// What can be tagged: an email (`attachment: None`) or one attachment of an email.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TagTarget {
    pub source: SourceId,
    pub message: Locator,
    pub attachment: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    /// CSS color `#rrggbb`.
    pub color: String,
    /// Tagged items (all sources) carrying this tag.
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaggedItem {
    pub target: TagTarget,
    pub tags: Vec<String>,
    /// When the item was first tagged, UTC ms.
    pub added: i64,
    /// Header of the carrying email (snapshot).
    pub header: Header,
    /// The attachment row (snapshot) when the target is an attachment.
    pub attachment: Option<AttachmentRow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TagDef {
    name: String,
    color: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct TagStore {
    version: u32,
    tags: Vec<TagDef>,
    items: Vec<TaggedItem>,
    #[serde(skip)]
    path: PathBuf,
}

/// Case-insensitive tag name equality, without allocating.
fn same_name(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

fn clean_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() {
        return Err(Error::Other("tag name is empty".into()));
    }
    Ok(n.to_string())
}

/// Tag colors are rendered as CSS by the app, so only `#rgb` / `#rrggbb` are accepted.
fn clean_color(color: &str) -> Result<String> {
    let c = color.trim();
    let hex = c.strip_prefix('#').unwrap_or("");
    if matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(c.to_ascii_lowercase())
    } else {
        Err(Error::Other(format!("tag color {color:?} is not #rgb or #rrggbb")))
    }
}

impl TagStore {
    pub(crate) fn load(path: &Path) -> Result<TagStore> {
        let mut store = jsonfile::load::<TagStore>(path)?.unwrap_or_default();
        if store.version > VERSION {
            return Err(Error::Unsupported(format!("{} was written by a newer STViewer", path.display())));
        }
        store.version = VERSION;
        store.path = path.to_path_buf();
        Ok(store)
    }

    fn save(&self) -> Result<()> {
        jsonfile::save(&self.path, self)
    }

    fn def(&self, name: &str) -> Option<&TagDef> {
        self.tags.iter().find(|t| same_name(&t.name, name))
    }

    pub(crate) fn tags(&self) -> Vec<Tag> {
        let mut counts: HashMap<String, u64> = HashMap::new();
        for name in self.items.iter().flat_map(|i| &i.tags) {
            *counts.entry(name.to_lowercase()).or_default() += 1;
        }
        self.tags
            .iter()
            .map(|t| Tag {
                name: t.name.clone(),
                color: t.color.clone(),
                count: counts.get(&t.name.to_lowercase()).copied().unwrap_or(0),
            })
            .collect()
    }

    pub(crate) fn create(&mut self, name: &str, color: Option<&str>) -> Result<Tag> {
        let name = clean_name(name)?;
        if self.def(&name).is_some() {
            return Err(Error::Other(format!("tag \"{name}\" already exists")));
        }
        let color = match color {
            Some(c) => clean_color(c)?,
            None => PALETTE[self.tags.len() % PALETTE.len()].to_string(),
        };
        self.tags.push(TagDef { name: name.clone(), color: color.clone() });
        self.save()?;
        Ok(Tag { name, color, count: 0 })
    }

    pub(crate) fn rename(&mut self, old: &str, new: &str) -> Result<()> {
        let new = clean_name(new)?;
        let Some(i) = self.tags.iter().position(|t| same_name(&t.name, old)) else {
            return Err(Error::NotFound(format!("tag \"{old}\"")));
        };
        if !same_name(old, &new) && self.def(&new).is_some() {
            return Err(Error::Other(format!("tag \"{new}\" already exists")));
        }
        let old_name = std::mem::replace(&mut self.tags[i].name, new.clone());
        for item in &mut self.items {
            for t in item.tags.iter_mut().filter(|t| same_name(t, &old_name)) {
                *t = new.clone();
            }
        }
        self.save()
    }

    pub(crate) fn delete(&mut self, name: &str) -> Result<()> {
        let before = self.tags.len();
        self.tags.retain(|t| !same_name(&t.name, name));
        if self.tags.len() == before {
            return Err(Error::NotFound(format!("tag \"{name}\"")));
        }
        for item in &mut self.items {
            item.tags.retain(|t| !same_name(t, name));
        }
        self.items.retain(|i| !i.tags.is_empty());
        self.save()
    }

    /// Distinct targets not tagged yet (they need a snapshot before `apply`).
    pub(crate) fn unknown<'a>(&'a self, targets: &'a [TagTarget]) -> Vec<&'a TagTarget> {
        let mut seen: HashSet<&TagTarget> = self.items.iter().map(|i| &i.target).collect();
        targets.iter().filter(|&t| seen.insert(t)).collect()
    }

    /// Add `tag` to `targets` (creating the tag if needed). `snapshots` must cover every
    /// target `unknown` reported; targets without one are skipped.
    pub(crate) fn apply(
        &mut self,
        targets: &[TagTarget],
        tag: &str,
        snapshots: Vec<(TagTarget, Header, Option<AttachmentRow>)>,
        now: i64,
    ) -> Result<()> {
        let tag = clean_name(tag)?;
        let tag = match self.def(&tag) {
            Some(d) => d.name.clone(),
            None => {
                let color = PALETTE[self.tags.len() % PALETTE.len()].to_string();
                self.tags.push(TagDef { name: tag.clone(), color });
                tag
            }
        };
        // A concurrent `set_tag` may have added some of these targets meanwhile.
        let fresh: Vec<bool> = {
            let mut seen: HashSet<&TagTarget> = self.items.iter().map(|i| &i.target).collect();
            snapshots.iter().map(|(t, ..)| seen.insert(t)).collect()
        };
        for ((target, header, attachment), fresh) in snapshots.into_iter().zip(fresh) {
            if fresh {
                self.items.push(TaggedItem { target, tags: Vec::new(), added: now, header, attachment });
            }
        }
        let wanted: HashSet<&TagTarget> = targets.iter().collect();
        for item in self.items.iter_mut().filter(|i| wanted.contains(&i.target)) {
            if !item.tags.iter().any(|t| same_name(t, &tag)) {
                item.tags.push(tag.clone());
            }
        }
        self.save()
    }

    pub(crate) fn remove(&mut self, targets: &[TagTarget], tag: &str) -> Result<()> {
        let wanted: HashSet<&TagTarget> = targets.iter().collect();
        for item in self.items.iter_mut().filter(|i| wanted.contains(&i.target)) {
            item.tags.retain(|t| !same_name(t, tag));
        }
        self.items.retain(|i| !i.tags.is_empty());
        self.save()
    }

    pub(crate) fn tags_of(&self, targets: &[TagTarget]) -> Vec<Vec<String>> {
        let by_target: HashMap<&TagTarget, &Vec<String>> = self.items.iter().map(|i| (&i.target, &i.tags)).collect();
        targets.iter().map(|t| by_target.get(t).map(|&tags| tags.clone()).unwrap_or_default()).collect()
    }

    /// Newest-tagged first; both filters optional.
    pub(crate) fn tagged(&self, source: Option<SourceId>, tag: Option<&str>) -> Vec<TaggedItem> {
        let mut out: Vec<TaggedItem> = self
            .items
            .iter()
            .filter(|i| source.is_none_or(|s| i.target.source == s))
            .filter(|i| tag.is_none_or(|t| i.tags.iter().any(|n| same_name(n, t))))
            .cloned()
            .collect();
        out.sort_by_key(|i| std::cmp::Reverse(i.added));
        out
    }

    pub(crate) fn count(&self, source: SourceId) -> u64 {
        self.items.iter().filter(|i| i.target.source == source).count() as u64
    }

    /// Forget every item of a removed source.
    pub(crate) fn drop_source(&mut self, source: SourceId) -> Result<()> {
        let before = self.items.len();
        self.items.retain(|i| i.target.source != source);
        if self.items.len() != before {
            self.save()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &Path) -> TagStore {
        TagStore::load(&dir.join(FILE_NAME)).unwrap()
    }

    fn target(nid: u32) -> TagTarget {
        TagTarget { source: SourceId(1), message: Locator::Pst { nid }, attachment: None }
    }

    #[test]
    fn colors_must_be_hex() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store(tmp.path());
        assert_eq!(s.create("a", Some("#A0B1C2")).unwrap().color, "#a0b1c2");
        assert_eq!(s.create("b", Some("#fff")).unwrap().color, "#fff");
        for bad in ["red", "#12345", "url(x)", "#ggg", "#fff;x:y"] {
            assert!(s.create("c", Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(s.tags().len(), 2);
    }

    #[test]
    fn duplicate_targets_are_tagged_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = store(tmp.path());
        let targets = [target(1), target(1), target(2)];
        let unknown: Vec<TagTarget> = s.unknown(&targets).into_iter().cloned().collect();
        assert_eq!(unknown, [target(1), target(2)]);
        // Snapshots may repeat a target (e.g. two racing `set_tag` calls).
        let snaps = [target(1), target(1), target(2)].map(|t| (t, Header::default(), None)).to_vec();
        s.apply(&targets, "Ärger", snaps, 0).unwrap();
        s.apply(&[target(1)], "ärger", Vec::new(), 1).unwrap();
        assert_eq!(s.tagged(None, Some("ÄRGER")).len(), 2);
        assert_eq!(s.tags_of(&[target(1), target(3)]), [vec!["Ärger".to_string()], vec![]]);
        assert_eq!(s.tags()[0].count, 2);
    }
}
