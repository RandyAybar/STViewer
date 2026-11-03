//! Conversation view: a thread as one dated timeline with one entry per email. Copies of
//! the same email (Inbox + Deleted Items + Recoverable Items, several archive parts, ...)
//! are merged into a single entry that lists the other folders holding a copy.

use std::collections::HashMap;

use mail_model::{FolderId, Header, Locator, SourceId};
use serde::{Deserialize, Serialize};

/// Preview characters kept per entry.
const PREVIEW_CHARS: usize = 280;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationItem {
    pub source: SourceId,
    pub header: Header,
    /// Start of this email's own text, with quoted earlier messages removed.
    pub preview: String,
    /// Display names of other folders that hold a copy of this email.
    pub also_in: Vec<String>,
    /// This entry is (a copy of) the message the conversation was opened from.
    pub current: bool,
}

/// Folders whose copies should not represent an email when a better copy exists.
fn secondary_folder(name: &str) -> bool {
    let n = name.to_lowercase();
    ["recoverable", "deleted", "deletions", "purges", "versions", "discoveryholds", "junk"]
        .iter()
        .any(|k| n.contains(k))
}

fn norm_id(id: &str) -> String {
    id.trim().trim_start_matches('<').trim_end_matches('>').trim().to_lowercase()
}

/// Merge copies and order by date. `items` are thread members; `previews` aligns with
/// `items`; `folder_name` resolves a folder id of a source to its display path.
pub(crate) fn build(
    items: Vec<(SourceId, Header)>,
    previews: Vec<Option<String>>,
    current: (SourceId, &Locator),
    folder_name: &dyn Fn(SourceId, &FolderId) -> String,
) -> Vec<ConversationItem> {
    // Group copies: same Message-ID, else the item stands alone.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (i, (_, h)) in items.iter().enumerate() {
        match h.message_id.as_deref().map(norm_id).filter(|s| !s.is_empty()) {
            Some(id) => match by_id.get(&id) {
                Some(&g) => groups[g].push(i),
                None => {
                    by_id.insert(id, groups.len());
                    groups.push(vec![i]);
                }
            },
            None => groups.push(vec![i]),
        }
    }

    let is_current = |i: usize| items[i].0 == current.0 && items[i].1.locator.as_ref() == Some(current.1);
    let name_of = |i: usize| items[i].1.folder.as_ref().map(|f| folder_name(items[i].0, f)).unwrap_or_default();

    let mut out: Vec<ConversationItem> = groups
        .into_iter()
        .map(|g| {
            // Representative: the opened message, else a copy outside deleted/recoverable folders.
            let rep = g
                .iter()
                .copied()
                .find(|&i| is_current(i))
                .or_else(|| g.iter().copied().find(|&i| !secondary_folder(&name_of(i))))
                .unwrap_or(g[0]);
            let rep_folder = name_of(rep);
            let mut also_in: Vec<String> = g
                .iter()
                .filter(|&&i| i != rep)
                .map(|&i| name_of(i))
                .filter(|n| !n.is_empty() && *n != rep_folder)
                .collect();
            also_in.sort();
            also_in.dedup();
            let preview = g
                .iter()
                .find_map(|&i| previews.get(i).cloned().flatten().filter(|p| !p.trim().is_empty()))
                .map(|p| own_text(&p))
                .unwrap_or_default();
            ConversationItem {
                source: items[rep].0,
                header: items[rep].1.clone(),
                preview,
                also_in,
                current: g.iter().any(|&i| is_current(i)),
            }
        })
        .collect();
    out.sort_by_key(|c| c.header.date);
    out
}

/// The email's own words: text before the first quoted earlier message, whitespace
/// collapsed, cut to [`PREVIEW_CHARS`].
pub(crate) fn own_text(body: &str) -> String {
    let mut kept = String::new();
    for line in body.lines() {
        let t = line.trim();
        let lower = t.to_lowercase();
        let starts_quote = lower.starts_with("-----original message")
            || lower.starts_with("----- original message")
            || lower.starts_with("-----forwarded by")
            || lower.starts_with("----- forwarded by")
            || (t.len() >= 10 && t.chars().all(|c| c == '_'))
            || (lower.starts_with("from:") && !kept.trim().is_empty())
            || (lower.starts_with("on ") && lower.ends_with("wrote:"));
        if starts_quote {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        kept.push_str(t);
        kept.push(' ');
    }
    let collapsed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= PREVIEW_CHARS {
        collapsed
    } else {
        let cut: String = collapsed.chars().take(PREVIEW_CHARS).collect();
        format!("{}…", cut.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_text_drops_quoted_history() {
        let body =
            "Sounds good, see you then.\n\nThanks\n-----Original Message-----\nFrom: Bob\nSent: Monday\nOld text";
        assert_eq!(own_text(body), "Sounds good, see you then. Thanks");
        assert_eq!(own_text("Yes.\n\nOn Mon, Jan 1, 2024 at 9:00 Bob wrote:\n> earlier"), "Yes.");
        assert_eq!(own_text("Agreed\n________________________________\nFrom: Ann\nquoted"), "Agreed");
        assert_eq!(own_text("> only quoted\nreply below"), "reply below");
        // A leading From: line (forward header with no own text) is kept.
        assert_eq!(own_text("From: Carol\nSubject: x"), "From: Carol Subject: x");
        let long = "word ".repeat(200);
        assert!(own_text(&long).ends_with('…') && own_text(&long).chars().count() <= PREVIEW_CHARS + 1);
    }

    fn h(id: Option<&str>, folder: &str, date: i64, nid: u32) -> Header {
        Header {
            message_id: id.map(Into::into),
            folder: Some(FolderId(folder.into())),
            date: Some(date),
            locator: Some(Locator::Pst { nid }),
            subject: "RE: Plan".into(),
            ..Default::default()
        }
    }

    #[test]
    fn copies_merge_into_one_dated_entry() {
        let s = SourceId(1);
        let items = vec![
            (s, h(Some("<b@x>"), "Recoverable-Items/Deletions", 20, 1)),
            (s, h(Some("<a@x>"), "Inbox", 10, 2)),
            (s, h(Some("b@x"), "Sent-Items", 20, 3)),
            (s, h(None, "Inbox", 30, 4)),
        ];
        let previews = vec![Some("second\n> old".into()), Some("first".into()), None, Some("third".into())];
        let current = Locator::Pst { nid: 1 };
        let out = build(items, previews, (s, &current), &|_, f| f.0.clone());
        let summary: Vec<(i64, &str, bool, Vec<String>)> =
            out.iter().map(|c| (c.header.date.unwrap(), c.preview.as_str(), c.current, c.also_in.clone())).collect();
        assert_eq!(
            summary,
            vec![
                (10, "first", false, vec![]),
                // The opened copy represents the merged email; the other copy is listed.
                (20, "second", true, vec!["Sent-Items".to_string()]),
                (30, "third", false, vec![]),
            ]
        );
        // Without the opened copy, a non-recoverable folder represents the email.
        let items2 =
            vec![(s, h(Some("b@x"), "Recoverable-Items/Deletions", 20, 1)), (s, h(Some("b@x"), "Sent-Items", 20, 3))];
        let other = Locator::Pst { nid: 99 };
        let out2 = build(items2, vec![None, None], (s, &other), &|_, f| f.0.clone());
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].header.folder, Some(FolderId("Sent-Items".into())));
        assert_eq!(out2[0].also_in, vec!["Recoverable-Items/Deletions".to_string()]);
    }
}
