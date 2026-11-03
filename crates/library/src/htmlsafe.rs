//! Neutralising untrusted email HTML for standalone export files, which a browser opens
//! with full privileges (unlike the app's sandboxed viewer).
//!
//! Two layers: a Content-Security-Policy `<meta>` that forbids scripts, plugins, frames,
//! form submission and every network fetch, and a tag filter that removes active
//! elements, event-handler attributes and `javascript:` URLs outright.

/// First element of every exported HTML file. Only inline styles and `data:` images (the
/// inlined `cid:` parts) are allowed; remote content (tracking pixels) is not loaded.
pub(crate) const CSP_META: &str = "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
     script-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; \
     img-src data:; style-src 'unsafe-inline'; font-src data:\">";

/// Elements dropped together with their content.
const DROP_WITH_CONTENT: [&str; 1] = ["script"];
/// Elements whose tags are dropped (their content stays).
const DROP_TAG: [&str; 10] =
    ["iframe", "frame", "frameset", "object", "embed", "applet", "base", "form", "link", "noscript"];
/// Attributes holding a URL that a `javascript:` / `vbscript:` / `data:` value turns active.
const URL_ATTRS: [&str; 7] = ["href", "src", "xlink:href", "action", "formaction", "background", "poster"];

/// `html` without scripts, frames, plugins, forms, `<base>`, `<link>`, `<meta http-equiv>`,
/// `on*` attributes and script URLs. Tags are tokenised like a browser does (quoted
/// attribute values may contain `>`), so markup cannot hide from the filter.
pub(crate) fn neutralize(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let b = lower.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(off) = lower[i..].find('<') {
        let lt = i + off;
        out.push_str(&html[i..lt]);
        let rest = &lower[lt + 1..];
        if rest.starts_with("!--") {
            // Comments: browsers also end one at `<!-->`, `<!--->` and `--!>`.
            let end = if rest.starts_with("!-->") {
                lt + 5
            } else if rest.starts_with("!--->") {
                lt + 6
            } else {
                let body = lt + 4;
                [lower[body..].find("-->").map(|p| body + p + 3), lower[body..].find("--!>").map(|p| body + p + 4)]
                    .into_iter()
                    .flatten()
                    .min()
                    .unwrap_or(html.len())
            };
            out.push_str(&html[lt..end]);
            i = end;
            continue;
        }
        if rest.starts_with(['!', '?']) {
            // Doctype / bogus comment: up to the next `>`.
            let end = lower[lt..].find('>').map_or(html.len(), |p| lt + p + 1);
            out.push_str(&html[lt..end]);
            i = end;
            continue;
        }
        let closing = rest.starts_with('/');
        let name_start = lt + 1 + usize::from(closing);
        let name_end = name_start + b[name_start..].iter().take_while(|c| c.is_ascii_alphanumeric()).count();
        if name_end == name_start {
            if closing {
                // `</ ...>` is a bogus comment to a browser: drop it.
                i = lower[lt..].find('>').map_or(html.len(), |p| lt + p + 1);
            } else {
                // `<` not starting a tag is text.
                out.push_str("&lt;");
                i = lt + 1;
            }
            continue;
        }
        let name = &lower[name_start..name_end];
        let tag = parse_tag(b, name_end);
        if !tag.closed {
            // A tag cut off by the end of input is dropped by browsers too.
            return out;
        }
        i = tag.end;
        if DROP_WITH_CONTENT.contains(&name) {
            if !closing {
                let close = format!("</{name}");
                i = match lower[i..].find(&close) {
                    Some(p) => parse_tag(b, i + p + close.len()).end,
                    None => html.len(),
                };
            }
            continue;
        }
        let refresh_or_csp = name == "meta" && tag.attrs.iter().any(|a| &lower[a.name.clone()] == "http-equiv");
        if DROP_TAG.contains(&name) || refresh_or_csp {
            continue;
        }
        if closing {
            out.push_str("</");
            out.push_str(&html[name_start..name_end]);
            out.push('>');
            continue;
        }
        out.push('<');
        out.push_str(&html[name_start..name_end]);
        for a in &tag.attrs {
            let attr_name = &lower[a.name.clone()];
            let value = a.value.clone().map(|v| &lower[v]).unwrap_or("");
            if attr_name.starts_with("on") || (URL_ATTRS.contains(&attr_name) && is_script_url(value)) {
                continue;
            }
            out.push(' ');
            out.push_str(&html[a.whole.clone()]);
        }
        out.push_str(if tag.self_closing { " />" } else { ">" });
    }
    out.push_str(&html[i..]);
    out
}

/// `javascript:` / `vbscript:` / `data:` once the whitespace and control characters
/// browsers ignore inside a scheme are removed (`java\tscript:`).
fn is_script_url(value: &str) -> bool {
    let scheme: String = value.chars().filter(|c| !c.is_ascii_whitespace() && !c.is_control()).take(11).collect();
    ["javascript:", "vbscript:", "data:"].iter().any(|s| scheme.starts_with(s))
}

struct Attr {
    name: std::ops::Range<usize>,
    /// Unquoted value.
    value: Option<std::ops::Range<usize>>,
    /// `name`, `name=value` or `name="value"` as written.
    whole: std::ops::Range<usize>,
}

struct Tag {
    attrs: Vec<Attr>,
    self_closing: bool,
    /// The tag ended with `>` (not cut off by the end of input).
    closed: bool,
    /// Byte offset just past the closing `>` (or the end of input).
    end: usize,
}

/// Attributes of a tag whose name ends at `from`, following the HTML tokenizer: names end
/// at whitespace, `/`, `>` or `=`; values are quoted, or run to whitespace or `>`.
fn parse_tag(b: &[u8], mut i: usize) -> Tag {
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            self_closing = b[i] == b'/';
            i += 1;
        }
        if i >= b.len() {
            return Tag { attrs, self_closing: false, closed: false, end: b.len() };
        }
        if b[i] == b'>' {
            return Tag { attrs, self_closing, closed: true, end: i + 1 };
        }
        self_closing = false;
        let start = i;
        // A leading `=` is part of the name.
        i += 1;
        while i < b.len() && !matches!(b[i], b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' | b'/' | b'>' | b'=') {
            i += 1;
        }
        let name = start..i;
        let mut j = i;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        let mut value = None;
        if j < b.len() && b[j] == b'=' {
            j += 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            match b.get(j) {
                Some(&q @ (b'"' | b'\'')) => {
                    let v = j + 1;
                    let close = b[v..].iter().position(|&c| c == q).map_or(b.len(), |p| v + p);
                    value = Some(v..close);
                    i = (close + 1).min(b.len());
                }
                _ => {
                    let v = j;
                    while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                        j += 1;
                    }
                    value = Some(v..j);
                    i = j;
                }
            }
        }
        attrs.push(Attr { name, value, whole: start..i });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_active_content() {
        let cases = [
            ("<p onclick=\"x()\" class=a>hi</p>", "<p class=a>hi</p>"),
            ("<SCRIPT src=x></script>ok<script>a</script >", "ok"),
            ("<a href=\" java\tscript:alert(1)\" title='t>'>l</a>", "<a title='t>'>l</a>"),
            ("<img src=x onerror=alert(1)/>", "<img src=x>"),
            ("<img src=\"cid:a\" />", "<img src=\"cid:a\" />"),
            ("<iframe src=x>fallback</iframe><form action=y><input></form>", "fallback<input>"),
            ("<meta http-equiv=refresh content='0;url=x'><meta charset=utf-8>", "<meta charset=utf-8>"),
            ("a < b <!-- c --> d", "a &lt; b <!-- c --> d"),
            // Browsers end this comment at `<!-->`; the script after it must not survive.
            ("<!--><script>x</script>-->", "<!-->-->"),
            ("<p title=\"<script>\">t</p></ p>", "<p title=\"<script>\">t</p>"),
            ("<script>never closed", ""),
            ("<div", ""),
        ];
        for (input, want) in cases {
            assert_eq!(neutralize(input), want, "{input}");
        }
    }

    #[test]
    fn keeps_ordinary_markup_and_text() {
        let html = "<!DOCTYPE html><html><head><style>p{}</style></head><body><p style='a:b'>Résumé &amp; café</p></body></html>";
        assert_eq!(neutralize(html), html);
    }
}
