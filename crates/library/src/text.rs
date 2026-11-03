//! Plain-text rendering of message bodies (for `stv show`, text export).

use mail_model::{Address, Body};

/// Best plain-text rendering of a body: text part, else HTML stripped to text, else RTF text.
pub fn body_text(body: &Body) -> String {
    if let Some(t) = body.text.as_deref().filter(|t| !t.trim().is_empty()) {
        return t.to_owned();
    }
    if let Some(h) = body.html.as_deref() {
        return html_to_text(h);
    }
    if let Some(r) = body.rtf.as_deref() {
        return mapi_props::rtf::to_text(r);
    }
    String::new()
}

/// `Name <email>`, or whichever half exists.
pub fn format_address(a: &Address) -> String {
    match (a.name.as_deref().filter(|n| !n.is_empty()), a.email.as_deref().filter(|e| !e.is_empty())) {
        (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
        (_, Some(e)) => e.to_owned(),
        (Some(n), None) => n.to_owned(),
        (None, None) => String::new(),
    }
}

pub fn format_addresses(list: &[Address]) -> String {
    let mut s = String::new();
    for a in list {
        let f = format_address(a);
        if f.is_empty() {
            continue;
        }
        if !s.is_empty() {
            s.push_str(", ");
        }
        s.push_str(&f);
    }
    s
}

/// Minimal HTML → text: drops script/style/head, maps block elements to line breaks,
/// decodes entities, collapses whitespace.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let n = html.len();
    while i < n {
        let rest = &html[i..];
        let c = rest.chars().next().unwrap_or('\0');
        match c {
            '<' => {
                if lower[i..].starts_with("<!--") {
                    i = lower[i..].find("-->").map_or(n, |p| i + p + 3);
                    continue;
                }
                let Some(end) = lower[i..].find('>').map(|p| i + p) else {
                    break;
                };
                let tag = &lower[i + 1..end];
                let closing = tag.starts_with('/');
                let name: String =
                    tag.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                i = end + 1;
                match name.as_str() {
                    "script" | "style" | "head" | "title" | "xml" if !closing => {
                        let close = format!("</{name}");
                        i = match lower[i..].find(&close) {
                            Some(p) => lower[i + p..].find('>').map_or(n, |q| i + p + q + 1),
                            None => n,
                        };
                    }
                    "br" => push_newline(&mut out, true),
                    "p" | "div" | "tr" | "table" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "pre"
                    | "ul" | "ol" | "hr" => push_newline(&mut out, false),
                    "li" => {
                        push_newline(&mut out, false);
                        if !closing {
                            out.push_str("- ");
                        }
                    }
                    "td" | "th" if !closing && !out.is_empty() && !out.ends_with([' ', '\n', '\t']) => {
                        out.push('\t');
                    }
                    _ => {}
                }
            }
            '&' => {
                let semi = rest.as_bytes()[..rest.len().min(12)].iter().position(|&b| b == b';');
                match semi.and_then(|s| decode_entity(&rest[1..s]).map(|ch| (s, ch))) {
                    Some((s, ch)) => {
                        out.push(ch);
                        i += s + 1;
                    }
                    None => {
                        out.push('&');
                        i += 1;
                    }
                }
            }
            c if c.is_whitespace() => {
                if !out.is_empty() && !out.ends_with([' ', '\n', '\t']) {
                    out.push(' ');
                }
                i += c.len_utf8();
            }
            c => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    tidy(&out)
}

fn push_newline(out: &mut String, force: bool) {
    while out.ends_with(' ') || out.ends_with('\t') {
        out.pop();
    }
    if force || !(out.is_empty() || out.ends_with('\n')) {
        out.push('\n');
    }
}

/// Trim trailing spaces per line, keep at most one blank line in a row, trim the ends.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    for line in s.lines() {
        let line = line.trim_end().trim_start_matches(' ');
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_owned()
}

fn decode_entity(e: &str) -> Option<char> {
    if let Some(num) = e.strip_prefix('#') {
        let v = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return char::from_u32(v).map(|c| if c == '\u{a0}' { ' ' } else { c });
    }
    Some(match e {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "deg" => '°',
        "times" => '×',
        "laquo" => '«',
        "raquo" => '»',
        _ => return None,
    })
}

/// Escape text for inclusion in HTML element content or a double-quoted attribute.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_and_decodes() {
        let html = "<html><head><title>T</title><style>p{color:red}</style></head>\
            <body><p>Hello&nbsp;<b>world</b> &amp; friends</p><!-- hidden --><div>Line&#33;<br>Next &#x263A;</div>\
            <script>alert(1)</script><ul><li>one</li><li>two</li></ul></body></html>";
        assert_eq!(html_to_text(html), "Hello world & friends\nLine!\nNext ☺\n- one\n- two");
    }

    #[test]
    fn unterminated_input_does_not_panic() {
        assert_eq!(html_to_text("a <b"), "a");
        assert_eq!(html_to_text("x &am"), "x &am");
        assert_eq!(html_to_text("<script>never closed"), "");
        assert_eq!(html_to_text("é&é"), "é&é");
    }
}
