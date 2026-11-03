//! `stv://` protocol (on Windows/Android reached as `http://stv.localhost/`).
//!
//! - `/cid/<source>/<locator>/<content-id>` → bytes of the attachment whose Content-ID
//!   (or, as a fallback some mailers rely on, filename) matches; used for `cid:` images.
//! - `/att/<source>/<locator>/<index>` → bytes of attachment `index` (image previews).
//! - `/thumb/<source>/<locator>/<index>` → JPEG picture of attachment `index` (longest side
//!   ≤ 256 px): images, and on every OS PDF first pages and OOXML/OpenDocument preview
//!   pictures, on macOS also Quick Look thumbnails; 404 when nothing can draw it, 503 when
//!   it could not be drawn now (renderer timed out, or too many requests waiting).
//! - `/preview/<source>/<locator>/<index>` → the same at up to 1024 px (preview pane).
//!
//! `<locator>` is the `Locator` JSON encoded as unpadded base64url; `<content-id>` is
//! percent-encoded. Responses are served with `nosniff` and a locked-down CSP so a
//! hostile attachment cannot become an active document.

use base64::Engine as _;
use mail_model::{AttachmentMeta, Locator, SourceId};
use percent_encoding::percent_decode_str;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{Manager, UriSchemeResponder};

use crate::preview;
use crate::AppState;

/// Answers `request`: at once, or for a picture not yet in memory, from a preview thread.
pub fn serve(app: &tauri::AppHandle, request: &Request<Vec<u8>>, responder: UriSchemeResponder) {
    match resolve(app, request.uri().path()) {
        Ok(Reply::Now(found)) => responder.respond(response(Ok(found))),
        Ok(Reply::Render(render)) => {
            preview::submit(move |admitted| responder.respond(response(if admitted { render() } else { Err(busy()) })))
        }
        Err(e) => responder.respond(response(Err(e))),
    }
}

fn response(result: Resolved) -> Response<Vec<u8>> {
    match result {
        Ok((mime, bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, "private, max-age=3600")
            .header("X-Content-Type-Options", "nosniff")
            .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox")
            .body(bytes),
        Err((status, msg)) => Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .header("X-Content-Type-Options", "nosniff")
            .body(msg.into_bytes()),
    }
    .unwrap_or_else(|_| Response::new(Vec::new()))
}

type Resolved = Result<(&'static str, Vec<u8>), (StatusCode, String)>;

enum Reply {
    /// Content type and body.
    Now((&'static str, Vec<u8>)),
    /// Loads and draws a picture; runs on a preview thread.
    Render(Box<dyn FnOnce() -> Resolved + Send>),
}

fn bad(msg: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.into())
}

fn resolve(app: &tauri::AppHandle, path: &str) -> Result<Reply, (StatusCode, String)> {
    let mut parts = path.trim_start_matches('/').splitn(4, '/');
    let (Some(kind), Some(sid), Some(loc), Some(key)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(bad("expected /<kind>/<source>/<locator>/<key>"));
    };
    let source = SourceId(sid.parse().map_err(|_| bad("bad source id"))?);
    let loc_json = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(loc.trim_end_matches('='))
        .map_err(|_| bad("bad locator encoding"))?;
    let locator: Locator = serde_json::from_slice(&loc_json).map_err(|_| bad("bad locator"))?;
    let key = percent_decode_str(key).decode_utf8_lossy();

    let state = app.state::<AppState>();
    if let Some(side) = match kind {
        "thumb" => Some(preview::THUMB_SIDE),
        "preview" => Some(preview::PREVIEW_SIDE),
        _ => None,
    } {
        let index: u32 = key.parse().map_err(|_| bad("bad attachment index"))?;
        let cache_key = format!("{kind}/{sid}/{loc}/{index}");
        if let Some(t) = preview::cached(&cache_key) {
            return if t.is_empty() { Err(no_preview()) } else { Ok(Reply::Now(("image/jpeg", t.to_vec()))) };
        }
        let atts = state.attachments(source, &locator).map_err(not_found)?;
        let meta =
            atts.iter().find(|a| a.index == index).filter(|a| preview::may_render(a)).ok_or_else(no_preview)?.clone();
        let app = app.clone();
        // Loading waits for a preview thread too, so at most a few attachments are in
        // memory however many tiles are requested.
        return Ok(Reply::Render(Box::new(move || {
            let bytes = app.state::<AppState>().lib.attachment(source, &locator, index).map_err(not_found)?;
            let cache_root =
                app.path().app_cache_dir().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            match preview::picture(&meta, &bytes, side, &cache_root) {
                preview::Picture::Jpeg(jpeg) => {
                    preview::remember(cache_key, &jpeg);
                    Ok(("image/jpeg", jpeg))
                }
                preview::Picture::Nothing => {
                    preview::remember(cache_key, &[]);
                    Err(no_preview())
                }
                preview::Picture::Unavailable => Err(busy()),
            }
        })));
    }
    let atts = state.attachments(source, &locator).map_err(not_found)?;
    let meta = match kind {
        "cid" => find_cid(&atts, &key),
        "att" => key.parse::<u32>().ok().and_then(|i| atts.iter().find(|a| a.index == i)),
        _ => return Err(bad("unknown resource kind")),
    }
    .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no attachment for {key}")))?;
    let bytes = state.lib.attachment(source, &locator, meta.index).map_err(not_found)?;
    Ok(Reply::Now((content_type(meta), bytes)))
}

fn not_found(e: mail_model::Error) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, e.to_string())
}

/// Nothing can draw the attachment; the frontend falls back to a text card or the type icon.
fn no_preview() -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, "no preview".into())
}

/// The picture could not be drawn now; a later request may succeed.
fn busy() -> (StatusCode, String) {
    (StatusCode::SERVICE_UNAVAILABLE, "preview busy".into())
}

fn normalize_cid(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix("cid:").unwrap_or(s);
    s.trim_start_matches('<').trim_end_matches('>')
}

fn find_cid<'a>(atts: &'a [AttachmentMeta], key: &str) -> Option<&'a AttachmentMeta> {
    let want = normalize_cid(key);
    atts.iter()
        .find(|a| a.content_id.as_deref().is_some_and(|c| normalize_cid(c).eq_ignore_ascii_case(want)))
        .or_else(|| atts.iter().find(|a| a.filename.as_deref().is_some_and(|f| f.eq_ignore_ascii_case(want))))
}

/// Raster image types served as themselves (declared MIME essence or file extension).
/// Everything else, notably SVG and HTML, is opaque bytes, so the webview never renders
/// an attachment as an active document.
const SERVED_IMAGES: &[(&str, &[&str])] = &[
    ("image/png", &["png"]),
    ("image/jpeg", &["jpg", "jpeg", "jfif", "jpe"]),
    ("image/gif", &["gif"]),
    ("image/bmp", &["bmp"]),
    ("image/webp", &["webp"]),
    ("image/avif", &["avif"]),
    ("image/x-icon", &["ico"]),
    ("image/tiff", &["tif", "tiff"]),
];

fn content_type(meta: &AttachmentMeta) -> &'static str {
    let declared = meta.mime_type.as_deref().map(|m| m.split(';').next().unwrap_or(m).trim());
    let ext = meta.filename.as_deref().and_then(|f| f.rsplit_once('.')).map(|(_, e)| e);
    SERVED_IMAGES
        .iter()
        .find(|(mime, _)| declared.is_some_and(|d| d.eq_ignore_ascii_case(mime)))
        .or_else(|| {
            SERVED_IMAGES.iter().find(|(_, exts)| ext.is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e))))
        })
        .map_or("application/octet-stream", |(mime, _)| mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(index: u32, cid: Option<&str>, name: Option<&str>, mime: Option<&str>) -> AttachmentMeta {
        AttachmentMeta {
            index,
            content_id: cid.map(Into::into),
            filename: name.map(Into::into),
            mime_type: mime.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn cid_matching_ignores_brackets_and_case() {
        let atts = [att(0, Some("<Logo@Example>"), Some("logo.png"), None), att(1, None, Some("chart.gif"), None)];
        assert_eq!(find_cid(&atts, "logo@example").map(|a| a.index), Some(0));
        assert_eq!(find_cid(&atts, "cid:<LOGO@example>").map(|a| a.index), Some(0));
        assert_eq!(find_cid(&atts, "chart.gif").map(|a| a.index), Some(1));
        assert!(find_cid(&atts, "missing").is_none());
    }

    #[test]
    fn active_types_are_not_served_as_themselves() {
        assert_eq!(content_type(&att(0, None, Some("x.svg"), Some("image/svg+xml"))), "application/octet-stream");
        // A parameter must not smuggle SVG past the check.
        assert_eq!(content_type(&att(0, None, None, Some("image/svg+xml; charset=utf-8"))), "application/octet-stream");
        assert_eq!(content_type(&att(0, None, Some("x.png"), Some("IMAGE/PNG; name=x.png"))), "image/png");
        assert_eq!(content_type(&att(0, None, Some("x.html"), Some("text/html"))), "application/octet-stream");
        assert_eq!(content_type(&att(0, None, Some("photo.JPG"), Some("application/octet-stream"))), "image/jpeg");
        assert_eq!(content_type(&att(0, None, None, Some("image/png"))), "image/png");
    }
}
