//! Search query language → tantivy queries.
//!
//! Grammar (Gmail-like): whitespace-separated clauses are ANDed; `a OR b` binds tighter than
//! the implicit AND; `-x` negates a clause or a parenthesised group; `"..."` is a phrase;
//! `term~` / `term~N` is fuzzy (edit distance N ≤ 2); `field:value` restricts to a field:
//! `from:` `to:` (to/cc/bcc) `cc:` `bcc:` `subject:` `body:` `attachment:` `has:attachment`
//! `is:read|unread|flagged|starred|draft|deleted|encrypted|signed`
//! `is:appointment|meeting|contact|task` `label:` `in:`
//! `before:YYYY-MM-DD` `after:YYYY-MM-DD` `larger:5M` `smaller:100K`.
//! Bare terms search subject^3, from^2, to, body, item details, attachment names and
//! attachment text.
//!
//! Calendar, contact and task items map onto the same fields: an appointment's organizer is
//! a sender (`from:`), its attendees (required, optional, resources) are recipients (`to:`),
//! and so are a contact's e-mail addresses. Location, recurrence, company, job title,
//! department, phone numbers, postal addresses, web page and task owner are item details
//! (bare words only). `is:appointment` matches calendar entries and meeting messages,
//! `is:meeting` only the latter. `before:`/`after:` compare an appointment's start and a
//! task's due date (the message date for everything else).

use std::ops::Bound;

use mail_model::{Error, Result};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, RangeQuery, TermQuery,
};
use tantivy::schema::{Field, IndexRecordOption};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::Term;

use crate::schema::Fields;
use crate::util::{parse_date_ms, parse_size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldName {
    From,
    To,
    Cc,
    Bcc,
    Subject,
    Body,
    Attachment,
    Has,
    Is,
    Label,
    In,
    Before,
    After,
    Larger,
    Smaller,
}

impl FieldName {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "from" => Self::From,
            "to" => Self::To,
            "cc" => Self::Cc,
            "bcc" => Self::Bcc,
            "subject" => Self::Subject,
            "body" => Self::Body,
            "attachment" | "filename" => Self::Attachment,
            "has" => Self::Has,
            "is" => Self::Is,
            "label" => Self::Label,
            "in" => Self::In,
            "before" | "older" => Self::Before,
            "after" | "newer" => Self::After,
            "larger" => Self::Larger,
            "smaller" => Self::Smaller,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leaf {
    pub field: Option<FieldName>,
    pub value: String,
    /// `None`: exact match. `Some(None)`: fuzzy with a length-based edit distance
    /// (resolved at translation). `Some(Some(n))`: fuzzy with distance `n` (≤ 2).
    pub fuzzy: Option<Option<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Leaf(Leaf),
}

#[derive(Debug, PartialEq, Eq)]
enum Tok {
    LParen,
    RParen,
    Or,
    /// `-` directly before `(`.
    Neg,
    Item {
        neg: bool,
        leaf: Leaf,
    },
}

fn lex(input: &str) -> Vec<Tok> {
    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' {
            toks.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            toks.push(Tok::RParen);
            i += 1;
            continue;
        }
        let mut neg = false;
        if c == '-' && i + 1 < n && !chars[i + 1].is_whitespace() {
            neg = true;
            i += 1;
            if chars[i] == '(' {
                toks.push(Tok::Neg);
                continue;
            }
        }
        // Optional `field:` prefix (only recognised names; `http://x` stays a plain word).
        let mut field = None;
        let mut j = i;
        while j < n && chars[j].is_ascii_alphabetic() {
            j += 1;
        }
        if j > i && j + 1 < n && chars[j] == ':' && !chars[j + 1].is_whitespace() {
            let name: String = chars[i..j].iter().collect();
            if let Some(f) = FieldName::parse(&name) {
                field = Some(f);
                i = j + 1;
            }
        }
        let mut value = String::new();
        let mut quoted = false;
        if i < n && chars[i] == '"' {
            quoted = true;
            i += 1;
            while i < n && chars[i] != '"' {
                value.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote (or past the end)
        } else {
            while i < n && !chars[i].is_whitespace() && chars[i] != ')' {
                value.push(chars[i]);
                i += 1;
            }
        }
        let mut fuzzy = None;
        if !quoted {
            if let Some(pos) = value.rfind('~') {
                let tail = &value[pos + 1..];
                if pos > 0 && (tail.is_empty() || (tail.len() == 1 && tail.as_bytes()[0].is_ascii_digit())) {
                    fuzzy = Some(tail.parse::<u8>().ok().map(|d| d.min(2)));
                    value.truncate(pos);
                }
            }
        }
        if !quoted && !neg && field.is_none() {
            if value == "OR" || value == "|" {
                toks.push(Tok::Or);
                continue;
            }
            if value == "AND" || value == "&" {
                continue;
            }
        }
        if value.trim().is_empty() {
            continue;
        }
        toks.push(Tok::Item { neg, leaf: Leaf { field, value, fuzzy } });
    }
    toks
}

/// Deepest parenthesis nesting accepted. Parsing and translation recurse once per level,
/// so unbounded nesting (`((((…`) would overflow the stack.
const MAX_DEPTH: usize = 32;

pub(crate) fn parse(input: &str) -> Result<Node> {
    let toks = lex(input);
    let mut depth = 0usize;
    for t in &toks {
        match t {
            Tok::LParen => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(bad(format!("query nests parentheses deeper than {MAX_DEPTH} levels")));
                }
            }
            // A stray `)` at the top level is ignored by the parser, as here.
            Tok::RParen => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let mut pos = 0;
    Ok(parse_seq(&toks, &mut pos, 0))
}

fn parse_seq(toks: &[Tok], pos: &mut usize, depth: u32) -> Node {
    let mut items = Vec::new();
    while *pos < toks.len() {
        match toks[*pos] {
            Tok::RParen => {
                *pos += 1;
                if depth > 0 {
                    break;
                }
            }
            Tok::Or => *pos += 1, // stray OR (leading, or after another OR)
            _ => {
                if let Some(node) = parse_or(toks, pos, depth) {
                    items.push(node);
                }
            }
        }
    }
    if items.len() == 1 {
        items.pop().unwrap()
    } else {
        Node::And(items)
    }
}

fn parse_or(toks: &[Tok], pos: &mut usize, depth: u32) -> Option<Node> {
    let first = parse_unary(toks, pos, depth)?;
    let mut alts = vec![first];
    while *pos < toks.len() && toks[*pos] == Tok::Or {
        *pos += 1;
        if *pos < toks.len() && !matches!(toks[*pos], Tok::RParen | Tok::Or) {
            if let Some(n) = parse_unary(toks, pos, depth) {
                alts.push(n);
            }
        }
    }
    Some(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Or(alts) })
}

fn parse_unary(toks: &[Tok], pos: &mut usize, depth: u32) -> Option<Node> {
    let tok = toks.get(*pos)?;
    *pos += 1;
    match tok {
        Tok::Neg => {
            if toks.get(*pos) == Some(&Tok::LParen) {
                *pos += 1;
                Some(Node::Not(Box::new(parse_seq(toks, pos, depth + 1))))
            } else {
                None
            }
        }
        Tok::LParen => Some(parse_seq(toks, pos, depth + 1)),
        Tok::Item { neg, leaf } => {
            let node = Node::Leaf(leaf.clone());
            Some(if *neg { Node::Not(Box::new(node)) } else { node })
        }
        Tok::RParen | Tok::Or => None,
    }
}

// ---------------------------------------------------------------------------------------------

pub(crate) struct Translator<'a> {
    pub f: &'a Fields,
    pub analyzer: TextAnalyzer,
}

type Q = Box<dyn Query>;

fn or(mut qs: Vec<Q>) -> Option<Q> {
    match qs.len() {
        0 => None,
        1 => qs.pop(),
        _ => Some(Box::new(BooleanQuery::new(qs.into_iter().map(|q| (Occur::Should, q)).collect()))),
    }
}

fn term(field: Field, value: &str) -> Q {
    Box::new(TermQuery::new(Term::from_field_text(field, value), IndexRecordOption::Basic))
}

fn bad(msg: String) -> Error {
    Error::Other(msg)
}

impl Translator<'_> {
    /// `None` means the node constrains nothing (e.g. only punctuation); an empty query string
    /// therefore matches every document.
    pub(crate) fn translate(&mut self, node: &Node) -> Result<Option<Q>> {
        match node {
            Node::Leaf(l) => self.leaf(l),
            Node::Not(inner) => Ok(self.translate(inner)?.map(|q| -> Q {
                Box::new(BooleanQuery::new(vec![(Occur::Must, Box::new(AllQuery)), (Occur::MustNot, q)]))
            })),
            Node::Or(alts) => {
                let mut qs = Vec::new();
                for a in alts {
                    if let Some(q) = self.translate(a)? {
                        qs.push(q);
                    }
                }
                Ok(or(qs))
            }
            Node::And(items) => {
                let mut clauses: Vec<(Occur, Q)> = Vec::new();
                for it in items {
                    match it {
                        Node::Not(inner) => {
                            if let Some(q) = self.translate(inner)? {
                                clauses.push((Occur::MustNot, q));
                            }
                        }
                        other => {
                            if let Some(q) = self.translate(other)? {
                                clauses.push((Occur::Must, q));
                            }
                        }
                    }
                }
                if clauses.is_empty() {
                    return Ok(None);
                }
                if !clauses.iter().any(|(o, _)| *o == Occur::Must) {
                    clauses.push((Occur::Must, Box::new(AllQuery)));
                }
                if clauses.len() == 1 {
                    return Ok(clauses.pop().map(|(_, q)| q));
                }
                Ok(Some(Box::new(BooleanQuery::new(clauses))))
            }
        }
    }

    /// Analyzed tokens of `text` with their positions. Positions keep the gaps left by
    /// dropped (over-long) tokens, exactly as at indexing time, so phrases still line up.
    fn tokens(&mut self, text: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let mut stream = self.analyzer.token_stream(text);
        stream.process(&mut |t| out.push((t.position, t.text.clone())));
        out
    }

    /// Analyzed text match on one field: term, phrase, or fuzzy term(s).
    fn text(&self, field: Field, tokens: &[(usize, String)], fuzzy: Option<Option<u8>>) -> Option<Q> {
        match (tokens, fuzzy) {
            ([], _) => None,
            (_, Some(d)) => {
                let mut qs: Vec<(Occur, Q)> = tokens
                    .iter()
                    .map(|(_, s)| {
                        let dist = d.unwrap_or(if s.chars().count() <= 5 { 1 } else { 2 });
                        (Occur::Must, Box::new(FuzzyTermQuery::new(Term::from_field_text(field, s), dist, true)) as Q)
                    })
                    .collect();
                Some(if qs.len() == 1 { qs.pop().unwrap().1 } else { Box::new(BooleanQuery::new(qs)) })
            }
            ([(_, s)], None) => {
                Some(Box::new(TermQuery::new(Term::from_field_text(field, s), IndexRecordOption::WithFreqs)))
            }
            (_, None) => {
                let first = tokens[0].0;
                let terms = tokens.iter().map(|(p, s)| (p - first, Term::from_field_text(field, s))).collect();
                Some(Box::new(PhraseQuery::new_with_offset(terms)))
            }
        }
    }

    fn text_fields(&mut self, fields: &[(Field, f32)], value: &str, fuzzy: Option<Option<u8>>) -> Option<Q> {
        let tokens = self.tokens(value);
        let qs = fields
            .iter()
            .filter_map(|&(f, boost)| {
                let q = self.text(f, &tokens, fuzzy)?;
                Some(if boost == 1.0 { q } else { Box::new(BoostQuery::new(q, boost)) as Q })
            })
            .collect();
        or(qs)
    }

    fn addresses(&mut self, text_fields: &[Field], addr_fields: &[Field], leaf: &Leaf) -> Option<Q> {
        let weighted: Vec<(Field, f32)> = text_fields.iter().map(|&f| (f, 1.0)).collect();
        let mut qs: Vec<Q> = self.text_fields(&weighted, &leaf.value, leaf.fuzzy).into_iter().collect();
        if leaf.value.contains('@') && leaf.fuzzy.is_none() {
            let exact = leaf.value.trim().trim_matches(['<', '>']).to_lowercase();
            qs.extend(addr_fields.iter().map(|&f| term(f, &exact)));
        }
        or(qs)
    }

    fn leaf(&mut self, l: &Leaf) -> Result<Option<Q>> {
        let f = self.f;
        let v = l.value.trim();
        let lower = v.to_lowercase();
        Ok(match l.field {
            None => self.text_fields(
                &[
                    (f.subject, 3.0),
                    (f.from, 2.0),
                    (f.to, 1.0),
                    (f.body, 1.0),
                    (f.details, 1.0),
                    (f.attachment_names, 1.0),
                    (f.attachment_text, 1.0),
                ],
                v,
                l.fuzzy,
            ),
            Some(FieldName::From) => self.addresses(&[f.from], &[f.from_addr], l),
            Some(FieldName::To) => self.addresses(&[f.to, f.cc, f.bcc], &[f.to_addr, f.cc_addr, f.bcc_addr], l),
            Some(FieldName::Cc) => self.addresses(&[f.cc], &[f.cc_addr], l),
            Some(FieldName::Bcc) => self.addresses(&[f.bcc], &[f.bcc_addr], l),
            Some(FieldName::Subject) => self.text_fields(&[(f.subject, 1.0)], v, l.fuzzy),
            Some(FieldName::Body) => self.text_fields(&[(f.body, 1.0)], v, l.fuzzy),
            Some(FieldName::Attachment) => {
                self.text_fields(&[(f.attachment_names, 1.0), (f.attachment_text, 1.0)], v, l.fuzzy)
            }
            Some(FieldName::Has) => match lower.as_str() {
                "attachment" | "attachments" | "attach" => Some(Box::new(TermQuery::new(
                    Term::from_field_bool(f.has_attachment, true),
                    IndexRecordOption::Basic,
                ))),
                _ => return Err(bad(format!("unknown has: value `{v}` (use has:attachment)"))),
            },
            Some(FieldName::Is) => {
                let word = match lower.as_str() {
                    "starred" | "flagged" => "flagged",
                    w @ ("read" | "unread" | "draft" | "deleted" | "encrypted" | "signed" | "appointment"
                    | "meeting" | "contact" | "task") => w,
                    _ => return Err(bad(format!("unknown is: value `{v}`"))),
                };
                Some(term(f.is, word))
            }
            Some(FieldName::Label) => Some(term(f.labels, &lower)),
            Some(FieldName::In) => or(vec![term(f.folder_name, &lower), term(f.labels, &lower)]),
            Some(FieldName::Before | FieldName::After) => {
                let ms = parse_date_ms(v).ok_or_else(|| bad(format!("invalid date `{v}` (use YYYY-MM-DD)")))?;
                let t = Term::from_field_i64(f.start, ms);
                Some(Box::new(if l.field == Some(FieldName::Before) {
                    RangeQuery::new(Bound::Unbounded, Bound::Excluded(t))
                } else {
                    RangeQuery::new(Bound::Included(t), Bound::Unbounded)
                }))
            }
            Some(FieldName::Larger | FieldName::Smaller) => {
                let n = parse_size(v).ok_or_else(|| bad(format!("invalid size `{v}` (e.g. 5M, 200K)")))?;
                let t = Term::from_field_u64(f.size, n);
                Some(Box::new(if l.field == Some(FieldName::Larger) {
                    RangeQuery::new(Bound::Included(t), Bound::Unbounded)
                } else {
                    RangeQuery::new(Bound::Unbounded, Bound::Excluded(t))
                }))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(field: Option<FieldName>, value: &str) -> Node {
        Node::Leaf(Leaf { field, value: value.into(), fuzzy: None })
    }

    fn parse_ok(q: &str) -> Node {
        parse(q).unwrap_or_else(|e| panic!("{q}: {e}"))
    }

    #[test]
    fn precedence_and_negation() {
        let n = parse_ok("a OR b c -d");
        assert_eq!(
            n,
            Node::And(vec![
                Node::Or(vec![leaf(None, "a"), leaf(None, "b")]),
                leaf(None, "c"),
                Node::Not(Box::new(leaf(None, "d"))),
            ])
        );
    }

    #[test]
    fn fields_phrases_fuzzy_groups() {
        let n = parse_ok(r#"from:"Alice Smith" subject:report~ -(x OR y) http://e.com body: "OR" "a~""#);
        let Node::And(items) = n else { panic!("{n:?}") };
        assert_eq!(items[0], leaf(Some(FieldName::From), "Alice Smith"));
        assert_eq!(
            items[1],
            Node::Leaf(Leaf { field: Some(FieldName::Subject), value: "report".into(), fuzzy: Some(None) })
        );
        assert_eq!(items[2], Node::Not(Box::new(Node::Or(vec![leaf(None, "x"), leaf(None, "y")]))));
        assert_eq!(items[3], leaf(None, "http://e.com"));
        // `body:` with nothing after it is just the word.
        assert_eq!(items[4], leaf(None, "body:"));
        // Quoting turns operators and `~` into plain text.
        assert_eq!(items[5], leaf(None, "OR"));
        assert_eq!(items[6], leaf(None, "a~"));
        assert_eq!(items.len(), 7);
    }

    #[test]
    fn malformed_input_parses_and_translates() {
        let (_, fields) = crate::schema::build();
        let mut tr = Translator { f: &fields, analyzer: crate::schema::analyzer() };
        for q in [
            "",
            "   ",
            "OR",
            "a OR",
            "OR a",
            "((",
            "))",
            "-",
            "-(",
            "-)",
            "\"unterminated",
            "\"\"",
            "from:\"\"",
            "~",
            "a~9",
            "x:y:z",
            "(a (b OR) c",
            "é~1",
            "日本語 OR Ελληνικά",
            "a) OR (b",
        ] {
            let node = parse_ok(q);
            tr.translate(&node).unwrap_or_else(|e| panic!("{q}: {e}"));
        }
        assert_eq!(parse_ok("a~9"), Node::Leaf(Leaf { field: None, value: "a".into(), fuzzy: Some(Some(2)) }));
        assert_eq!(parse_ok(""), Node::And(vec![]));
    }

    #[test]
    fn nesting_is_bounded() {
        let ok = format!("{}a{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert_eq!(parse_ok(&ok), leaf(None, "a"));
        // Siblings do not add up: depth is the nesting, not the number of groups.
        parse_ok(&"(a) ".repeat(MAX_DEPTH * 4));
        // Deeper than the limit (would overflow the stack if parsed recursively) is an error.
        assert!(parse(&"(".repeat(200_000)).is_err());
        assert!(parse(&"-(".repeat(MAX_DEPTH + 1)).is_err());
    }
}
