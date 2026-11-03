//! Link targets of an HTML body, for agents checking where a message's links really go.
//!
//! A small tag scanner (no DOM): finds `<a href>` / `<area href>`, takes the visible text
//! (or `alt`), unwraps Microsoft SafeLinks and Proofpoint URL Defense rewriting, and flags
//! links whose visible text names a different host than the target — the classic
//! phishing tell.

use base64::Engine as _;
use percent_encoding::percent_decode_str;
use serde::Serialize;

/// Longest visible text kept per link.
const MAX_TEXT_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Link {
    /// Visible text (or the `alt` of an image map area), whitespace collapsed.
    pub text: String,
    /// The `href` as written (entities decoded).
    pub url: String,
    /// Lowercase scheme (`https`, `mailto`, ...), when the URL has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// Target behind a SafeLinks / URL Defense rewrite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_url: Option<String>,
    /// The text looks like a URL or domain whose host differs from the (unwrapped) target.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mismatch: bool,
}

/// Distinct links of `html` in document order, at most `cap`; `true` when more were cut.
pub fn extract(html: &str, cap: usize) -> (Vec<Link>, bool) {
    let mut out: Vec<Link> = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find('<') {
        let start = pos + rel;
        let rest = &lower[start + 1..];
        if rest.starts_with("!--") {
            pos = lower[start..].find("-->").map_or(lower.len(), |e| start + e + 3);
            continue;
        }
        let name_len = rest.bytes().take_while(u8::is_ascii_alphanumeric).count();
        let name = &rest[..name_len];
        let Some(tag_end) = tag_end(html, start + 1 + name_len) else { break };
        pos = tag_end + 1;
        if name == "script" || name == "style" {
            pos = lower[pos..].find(&format!("</{name}")).map_or(lower.len(), |e| pos + e);
            continue;
        }
        if name != "a" && name != "area" {
            continue;
        }
        let attrs = &html[start + 1 + name_len..tag_end];
        let Some(href) = attr(attrs, "href").map(|h| decode_entities(h.trim())) else { continue };
        if href.is_empty() || href.starts_with('#') {
            continue;
        }
        let text = if name == "a" {
            let close = lower[pos..].find("</a").map_or(lower.len(), |e| pos + e);
            let inner = stv_index::extract::html_to_text(&html[pos..close]);
            pos = close;
            inner
        } else {
            attr(attrs, "alt").map(decode_entities).unwrap_or_default()
        };
        let link = make(clean_text(&text), href);
        if out.iter().any(|l| l.url == link.url && l.text == link.text) {
            continue;
        }
        if out.len() == cap {
            return (out, true);
        }
        out.push(link);
    }
    (out, false)
}

fn make(text: String, url: String) -> Link {
    let scheme = url
        .split_once(':')
        .map(|(s, _)| s)
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b)))
        .map(str::to_ascii_lowercase);
    let original_url = unwrap_rewriter(&url);
    let target = original_url.as_deref().unwrap_or(&url);
    let mismatch = match (text_host(&text), url_host(target)) {
        (Some(shown), Some(real)) => !same_site(&shown, &real),
        _ => false,
    };
    Link { text, url, scheme, original_url, mismatch }
}

/// Index of the `>` closing the tag whose attributes start at `from` (quotes respected).
fn tag_end(html: &str, from: usize) -> Option<usize> {
    let mut quote = None;
    for (i, b) in html.bytes().enumerate().skip(from) {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// Value of attribute `name` in a tag's attribute text (quoted or bare).
fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && !b"=>/".contains(&bytes[i]) {
            i += 1;
        }
        let key = &attrs[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            if i == key_start {
                i += 1;
            }
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let (value, next) = match bytes.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let end = attrs[i + 1..].find(q as char).map_or(attrs.len(), |e| i + 1 + e);
                (&attrs[(i + 1).min(end)..end], end + 1)
            }
            _ => {
                let end = attrs[i..].find(|c: char| c.is_ascii_whitespace() || c == '>').map_or(attrs.len(), |e| i + e);
                (&attrs[i..end], end)
            }
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
        i = next;
    }
    None
}

/// Decodes the HTML entities that occur in URLs and short texts.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                _ => {
                    let n = name.strip_prefix('#')?;
                    let code = match n.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                        None => n.parse().ok()?,
                    };
                    char::from_u32(code)?
                }
            };
            Some((c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn clean_text(s: &str) -> String {
    let mut out = String::new();
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    if let Some((i, _)) = out.char_indices().nth(MAX_TEXT_CHARS) {
        out.truncate(i);
        out.push('…');
    }
    out
}

/// Host of an absolute `scheme://host...` URL, lowercased, without credentials or port.
fn url_host(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b)) {
        return None;
    }
    host_of(rest)
}

/// `host[:port][/...]` with optional `user@` → lowercase host.
fn host_of(s: &str) -> Option<String> {
    let authority = s.split(['/', '?', '#', '\\']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') { host } else { host.split(':').next()? };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Host named by link text that looks like a URL or a bare domain (`www.example.org`,
/// `https://portal.example.org/login`); `None` for ordinary words.
fn text_host(text: &str) -> Option<String> {
    let t = text.trim().trim_matches(|c: char| matches!(c, '<' | '>' | '[' | ']' | '(' | ')' | '"' | '\''));
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    let host = match t.split_once("://") {
        Some(_) => url_host(t)?,
        None => host_of(t)?,
    };
    let labels: Vec<&str> = host.split('.').collect();
    let tld = labels.last()?;
    let looks_like_domain = labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        && tld.len() >= 2
        && tld.bytes().all(|b| b.is_ascii_alphabetic());
    looks_like_domain.then_some(host)
}

/// Same host, ignoring `www.`, or one is a subdomain of the other.
fn same_site(a: &str, b: &str) -> bool {
    let a = a.strip_prefix("www.").unwrap_or(a);
    let b = b.strip_prefix("www.").unwrap_or(b);
    a == b || a.ends_with(&format!(".{b}")) || b.ends_with(&format!(".{a}"))
}

/// The real target behind Microsoft SafeLinks or Proofpoint URL Defense (v2, v3).
fn unwrap_rewriter(url: &str) -> Option<String> {
    let host = url_host(url)?;
    let query = url.split_once('?').map(|(_, q)| q.split('#').next().unwrap_or(q));
    let param =
        |name: &str| query?.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v));
    if host.ends_with("safelinks.protection.outlook.com") {
        let v = param("url")?;
        return Some(percent_decode_str(&v.replace('+', " ")).decode_utf8_lossy().into_owned());
    }
    if host == "urldefense.proofpoint.com" && url.contains("/v2/") {
        let v = param("u")?.replace('-', "%").replace('_', "/");
        return Some(percent_decode_str(&v).decode_utf8_lossy().into_owned());
    }
    if host == "urldefense.com" {
        let rest = url.split_once("/v3/__")?.1;
        let (target, tail) = rest.split_once("__;")?;
        if !target.contains('*') {
            return Some(target.to_string());
        }
        // `*` placeholders are filled, in order, from the base64url token after `__;`.
        let token = tail.split("!!").next()?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token.trim_end_matches('=')).ok()?;
        let mut fill = std::str::from_utf8(&bytes).ok()?.chars();
        let mut out = String::with_capacity(target.len());
        for c in target.chars() {
            if c == '*' {
                out.push(fill.next()?);
            } else {
                out.push(c);
            }
        }
        return Some(out);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links_text_and_mismatches() {
        let html = r##"<p>Hi <A class=x HREF="http://login.evil.example/x?a=1&amp;b=2">https://portal.example.org</a>
            <a href='https://www.example.org/help'>example.org</a> <a href="mailto:help@example.org">Mail us</a>
            <a href="#top">top</a><!-- <a href="http://hidden.example/">x</a> -->
            <map><area href="https://maps.example.net/" alt="Map"></map>
            <a href=https://bare.example.net/p>click <b>here</b></a></p>"##;
        let (links, cut) = extract(html, 200);
        assert!(!cut);
        let urls: Vec<&str> = links.iter().map(|l| l.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "http://login.evil.example/x?a=1&b=2",
                "https://www.example.org/help",
                "mailto:help@example.org",
                "https://maps.example.net/",
                "https://bare.example.net/p"
            ]
        );
        assert_eq!(links[0].text, "https://portal.example.org");
        assert!(links[0].mismatch);
        assert!(!links[1].mismatch, "same site with www");
        assert_eq!(links[2].scheme.as_deref(), Some("mailto"));
        assert_eq!(links[3].text, "Map");
        assert_eq!(links[4].text, "click here");
        assert!(!links[4].mismatch, "ordinary words are not a host");
    }

    #[test]
    fn caps_and_dedups() {
        let html = "<a href=\"https://a.example/\">a</a>".repeat(3) + &"<a href=\"https://b.example/\">b</a>".repeat(2);
        assert_eq!(extract(&html, 200).0.len(), 2);
        let (links, cut) = extract(&html, 1);
        assert_eq!((links.len(), cut), (1, true));
    }

    #[test]
    fn unwraps_rewriters() {
        let safe = "https://eur01.safelinks.protection.outlook.com/?url=https%3A%2F%2Fevil.example%2Flogin&data=x";
        let l = make("https://bank.example.org".into(), safe.into());
        assert_eq!(l.original_url.as_deref(), Some("https://evil.example/login"));
        assert!(l.mismatch);
        let v2 = "https://urldefense.proofpoint.com/v2/url?u=https-3A__www.example.org_path&d=x";
        assert_eq!(unwrap_rewriter(v2).as_deref(), Some("https://www.example.org/path"));
        let v3 = "https://urldefense.com/v3/__https://example.org/a*b__;Iw!!token";
        assert_eq!(unwrap_rewriter(v3).as_deref(), Some("https://example.org/a#b"));
    }

    #[test]
    fn survives_malformed_html() {
        for html in
            ["<a href=\"x", "<a", "<", "<a href=>", "<!-- open", "<a href='https://x.example/'>", "&#xFFFFFFFF;"]
        {
            let _ = extract(html, 10);
        }
        assert_eq!(decode_entities("a&#x41;&#66;&bogus;&"), "aAB&bogus;&");
    }
}
