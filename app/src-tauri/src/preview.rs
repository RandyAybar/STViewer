//! Preview images of attachments: grid thumbnails and the larger picture in the preview
//! pane, both served as JPEG by the `stv://` protocol.
//!
//! Renderers, first success wins:
//! - images: the `image` crate; on macOS, ImageIO for what it cannot read (CCITT fax and
//!   palette TIFFs, HEIC);
//! - macOS: Quick Look for PDF, Office, iWork, OpenDocument and plain-text files — the
//!   thumbnails Finder shows, drawn in Apple's sandboxed generator processes;
//! - PDF: the first page, rasterised by `hayro` in a worker process with memory and time
//!   limits (see [`pdf`]);
//! - OOXML / OpenDocument: the preview picture the authoring app saved inside the file.
//!
//! Anything else has no picture; the frontend then shows a text card or the type icon.
//!
//! Speed: results are cached in memory and on disk (keyed by the attachment's content,
//! so copies of one file render once). Requests run on dedicated preview threads,
//! newest first, so the tiles on screen go before tiles already scrolled away (and
//! requests far behind are turned away); Quick Look, which works out of process, gets
//! more parallel slots than the in-process decoders.

mod pdf;
mod queue;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::time::SystemTime;

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageReader, Limits, Rgb, RgbImage};
use mail_model::{AttachmentCategory, AttachmentMeta};
use sha2::{Digest, Sha256};

pub use pdf::{worker_main, Allocator};
pub use queue::submit;

/// Grid tile size (longest side).
pub const THUMB_SIDE: u32 = 256;
/// Preview pane size (longest side).
pub const PREVIEW_SIDE: u32 = 1024;

/// Images beyond this are not decoded (protects memory on hostile/huge files).
const MAX_PIXELS: u64 = 80_000_000;
/// Larger documents are not rendered: a tile is not worth parsing a 100 MB file.
const MAX_DOCUMENT_BYTES: usize = 64 << 20;
/// Larger image files are not decoded (an 80-megapixel photo compresses well below this).
const MAX_IMAGE_BYTES: usize = 128 << 20;
/// Largest embedded preview picture read out of an OOXML/OpenDocument package.
const MAX_EMBEDDED_BYTES: u64 = 16 << 20;
const MEMORY_CACHE_BYTES: usize = 64 << 20;
/// The disk cache is trimmed back to 3/4 of this at startup and after every
/// [`DISK_CACHE_BYTES`]` / 4` written.
const DISK_CACHE_BYTES: u64 = 512 << 20;
/// Part of every disk cache key: bump when renderers change what they draw.
const RENDERER_VERSION: u8 = 1;

/// Outcome of drawing one attachment.
pub enum Picture {
    Jpeg(Vec<u8>),
    /// No renderer can draw it; remembered, so it is not retried.
    Nothing,
    /// A renderer gave up (Quick Look timed out); may succeed next time.
    Unavailable,
}

// ---- memory cache (per request URL; an empty entry means "nothing to draw") ----

#[derive(Default)]
struct Cache {
    map: HashMap<String, Arc<[u8]>>,
    bytes: usize,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);

/// Picture remembered for a request URL by [`remember`]; empty means "nothing to draw".
pub fn cached(key: &str) -> Option<Arc<[u8]>> {
    CACHE.lock().unwrap_or_else(|p| p.into_inner()).map.get(key).cloned()
}

/// Keeps the picture (or, if empty, "nothing to draw") for a request URL in memory.
pub fn remember(key: String, jpeg: &[u8]) {
    let mut c = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if c.bytes + jpeg.len() > MEMORY_CACHE_BYTES {
        // Rare and cheap to refill from the disk cache: start over.
        c.map.clear();
        c.bytes = 0;
    }
    c.bytes += jpeg.len();
    c.map.insert(key, Arc::from(jpeg));
}

// ---- render slots ----

/// Bounded concurrency in which the newest waiter goes first: the grid requests the tiles
/// on screen last, so they render before requests for tiles that were scrolled past.
struct Pool {
    max: usize,
    state: Mutex<PoolState>,
    turn: Condvar,
}

struct PoolState {
    running: usize,
    next_ticket: u64,
    waiting: BTreeSet<u64>,
}

/// One slot of a [`Pool`], released on drop.
struct Permit<'a>(&'a Pool);

impl Pool {
    const fn new(max: usize) -> Pool {
        Pool {
            max,
            state: Mutex::new(PoolState { running: 0, next_ticket: 0, waiting: BTreeSet::new() }),
            turn: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn acquire(&self) -> Permit<'_> {
        let mut s = self.lock();
        let ticket = s.next_ticket;
        s.next_ticket += 1;
        s.waiting.insert(ticket);
        while !(s.running < self.max && s.waiting.last() == Some(&ticket)) {
            s = self.turn.wait(s).unwrap_or_else(|p| p.into_inner());
        }
        s.waiting.remove(&ticket);
        s.running += 1;
        // A further free slot now belongs to the next-newest waiter.
        self.turn.notify_all();
        Permit(self)
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.lock().running -= 1;
        self.0.turn.notify_all();
    }
}

/// In-process decoding. Each may allocate up to `MAX_PIXELS * 4` bytes, and a grid of
/// hostile files would otherwise decode all at once on the blocking pool.
static DECODERS: Pool = Pool::new(2);
/// PDF worker processes, each allowed up to 1 GiB; separate from [`DECODERS`] so slow
/// PDFs do not hold up images. Starting a process dominates a typical page (about 70 of
/// 85 ms measured on Windows), so more run at once than there are in-process decoders.
static PDF_WORKERS: Pool = Pool::new(4);
/// Quick Look renders in its own processes; measured on 64 Office files, throughput
/// keeps rising up to about 8 requests in flight.
#[cfg(target_os = "macos")]
static QUICK_LOOK: Pool = Pool::new(8);

// ---- disk cache (content-addressed JPEGs under `<app cache>/thumbs`) ----

/// Picture of `bytes` at `max_side`: from the disk cache under `cache_root`, else
/// rendered and stored there (definite "nothing to draw" results too, as empty files).
pub fn picture(meta: &AttachmentMeta, bytes: &[u8], max_side: u32, cache_root: &Path) -> Picture {
    if bytes.len() > size_limit(meta) {
        return Picture::Nothing;
    }
    let path = disk_path(&cache_root.join("thumbs"), meta, bytes, max_side);
    if let Ok(hit) = fs::read(&path) {
        // Recently used entries survive trimming.
        let _ = fs::File::options().write(true).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
        return if hit.is_empty() { Picture::Nothing } else { Picture::Jpeg(hit) };
    }
    let picture = render(meta, bytes, max_side, &cache_root.join("preview"));
    let stored: &[u8] = match &picture {
        Picture::Jpeg(jpeg) => jpeg,
        Picture::Nothing => &[],
        Picture::Unavailable => return picture,
    };
    store(cache_root, &path, stored);
    picture
}

/// Best effort: written whole under a name of its own, then renamed, so a reader never
/// sees half a file, even while another request stores the same picture. Every
/// [`DISK_CACHE_BYTES`]` / 4` stored, the cache is trimmed in the background.
fn store(cache_root: &Path, path: &Path, bytes: &[u8]) {
    static NEXT_TMP: AtomicU64 = AtomicU64::new(0);
    static STORED: AtomicU64 = AtomicU64::new(0);
    let Some(dir) = path.parent() else { return };
    let tmp = path.with_extension(format!("{}-{}.tmp", std::process::id(), NEXT_TMP.fetch_add(1, Relaxed)));
    if fs::create_dir_all(dir).and_then(|()| fs::write(&tmp, bytes)).and_then(|()| fs::rename(&tmp, path)).is_err() {
        let _ = fs::remove_file(&tmp);
        return;
    }
    let size = disk_size(bytes.len() as u64);
    if STORED.fetch_add(size, Relaxed) + size > DISK_CACHE_BYTES / 4 {
        STORED.store(0, Relaxed);
        let root = cache_root.to_path_buf();
        let _ = std::thread::Builder::new().name("stv-preview-trim".into()).spawn(move || trim_disk_cache(&root));
    }
}

/// Disk space an entry of `len` bytes takes: whole 4 KiB clusters, at least one, as an
/// empty "nothing to draw" entry still costs a directory record.
fn disk_size(len: u64) -> u64 {
    len.div_ceil(4096).max(1) * 4096
}

/// `<dir>/<2 hex>/<64 hex>.jpg`, hashed over everything that decides the picture: the
/// renderer version, size, file name extension, declared type and the bytes.
fn disk_path(dir: &Path, meta: &AttachmentMeta, bytes: &[u8], max_side: u32) -> PathBuf {
    let mut h = Sha256::new();
    h.update([RENDERER_VERSION]);
    h.update(max_side.to_le_bytes());
    h.update(extension(meta).as_bytes());
    h.update([0]);
    h.update(meta.mime_type.as_deref().unwrap_or("").trim().to_ascii_lowercase().as_bytes());
    h.update([0]);
    h.update(bytes);
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    dir.join(&hex[..2]).join(format!("{hex}.jpg"))
}

/// Deletes the least recently used entries once the disk cache exceeds its budget.
pub fn trim_disk_cache(cache_root: &Path) {
    let mut entries: Vec<(SystemTime, u64, PathBuf)> = fs::read_dir(cache_root.join("thumbs"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|d| fs::read_dir(d.path()).into_iter().flatten().flatten())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, disk_size(m.len()), e.path()))
        })
        .collect();
    let mut total: u64 = entries.iter().map(|e| e.1).sum();
    if total <= DISK_CACHE_BYTES {
        return;
    }
    entries.sort_unstable_by_key(|e| e.0);
    for (_, len, path) in entries {
        if total <= DISK_CACHE_BYTES / 4 * 3 {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

/// Category of the attachment, from its file name and declared type.
fn category(meta: &AttachmentMeta) -> AttachmentCategory {
    AttachmentCategory::classify(&extension(meta), meta.mime_type.as_deref(), meta.kind)
}

/// Whether [`render`] could draw this attachment at all, checked before loading its bytes
/// (with the size the container declares; [`picture`] checks the real one).
pub fn may_render(meta: &AttachmentMeta) -> bool {
    let drawable = match category(meta) {
        // SVG is a document that may reference remote content, not a raster image.
        AttachmentCategory::Image => !is_svg(meta),
        AttachmentCategory::Pdf => true,
        AttachmentCategory::Document
        | AttachmentCategory::Spreadsheet
        | AttachmentCategory::Presentation
        | AttachmentCategory::Text => quick_look_extension(meta).is_some() || is_package(meta),
        _ => false,
    };
    drawable && meta.size <= size_limit(meta) as u64
}

/// Largest file of this kind that is rendered.
fn size_limit(meta: &AttachmentMeta) -> usize {
    if category(meta) == AttachmentCategory::Image {
        MAX_IMAGE_BYTES
    } else {
        MAX_DOCUMENT_BYTES
    }
}

/// Whether the file is an OOXML or OpenDocument package, which may carry a preview picture.
fn is_package(meta: &AttachmentMeta) -> bool {
    const PACKAGES: &[&str] = &[
        "docx", "docm", "dotx", "dotm", "xlsx", "xlsm", "xlsb", "xltx", "xltm", "pptx", "pptm", "ppsx", "ppsm", "potx",
        "potm", "odt", "ott", "ods", "ots", "odp", "otp", "odg",
    ];
    PACKAGES.contains(&extension(meta).as_str())
}

fn is_svg(meta: &AttachmentMeta) -> bool {
    extension(meta) == "svg"
        || meta.mime_type.as_deref().is_some_and(|m| m.trim().to_ascii_lowercase().starts_with("image/svg"))
}

/// Lowercase extension of the attachment's file name ("" if none).
fn extension(meta: &AttachmentMeta) -> String {
    meta.filename
        .as_deref()
        .and_then(|f| f.rsplit(['/', '\\']).next())
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, e)| e.trim_end_matches(['.', ' ']).to_ascii_lowercase())
        .unwrap_or_default()
}

/// Draws `bytes` scaled to fit `max_side`. `scratch` is a private per-user directory for
/// the copy Quick Look needs.
fn render(meta: &AttachmentMeta, bytes: &[u8], max_side: u32, scratch: &Path) -> Picture {
    let category = category(meta);
    let jpeg = match category {
        AttachmentCategory::Image if is_svg(meta) => None,
        AttachmentCategory::Image => {
            let _slot = DECODERS.acquire();
            // ImageIO decodes a JPEG at reduced size (or takes its embedded thumbnail),
            // several times faster than a full decode; for other formats the `image`
            // crate is faster.
            let img = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                native_image(bytes, max_side).or_else(|| decode(bytes))
            } else {
                decode(bytes).or_else(|| native_image(bytes, max_side))
            };
            img.and_then(|img| to_jpeg(&img, max_side))
        }
        AttachmentCategory::Pdf
        | AttachmentCategory::Document
        | AttachmentCategory::Spreadsheet
        | AttachmentCategory::Presentation
        | AttachmentCategory::Text => {
            let mut timed_out = false;
            // Quick Look pictures are already at most `max_side`: no slot needed to encode.
            let mut jpeg = match quick_look(meta, bytes, max_side, scratch) {
                Ok(img) => img.and_then(|img| to_jpeg(&img, max_side)),
                Err(TimedOut) => {
                    timed_out = true;
                    None
                }
            };
            if jpeg.is_none() && category == AttachmentCategory::Pdf {
                let _slot = PDF_WORKERS.acquire();
                match pdf::first_page(bytes, max_side) {
                    Ok(drawn) => jpeg = drawn,
                    Err(TimedOut) => timed_out = true,
                }
            }
            if jpeg.is_none() && is_package(meta) {
                let _slot = DECODERS.acquire();
                jpeg = embedded_preview(bytes).and_then(|img| to_jpeg(&img, max_side));
            }
            if jpeg.is_none() && timed_out {
                return Picture::Unavailable;
            }
            jpeg
        }
        _ => None,
    };
    jpeg.map_or(Picture::Nothing, Picture::Jpeg)
}

/// Downscale, flatten transparency onto white, encode JPEG.
fn to_jpeg(img: &DynamicImage, max_side: u32) -> Option<Vec<u8>> {
    let rgba = if img.width() > max_side || img.height() > max_side {
        img.thumbnail(max_side, max_side).to_rgba8()
    } else {
        img.to_rgba8()
    };
    let flat = RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = u16::from(p[3]);
        let mix = |c: u8| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8;
        Rgb([mix(p[0]), mix(p[1]), mix(p[2])])
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 82).encode_image(&flat).ok()?;
    Some(out)
}

fn decode(bytes: &[u8]) -> Option<DynamicImage> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_PIXELS * 4);
    reader.limits(limits.clone());
    let (w, h) = reader.into_dimensions().ok()?;
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.limits(limits);
    reader.decode().ok()
}

/// The preview picture an OOXML (`docProps/thumbnail.*`) or OpenDocument
/// (`Thumbnails/thumbnail.png`) package carries. WMF/EMF previews are not decodable.
fn embedded_preview(bytes: &[u8]) -> Option<DynamicImage> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    // Part names as the authoring apps write them (OOXML part names are case-insensitive,
    // but producers use these spellings).
    let name =
        ["docProps/thumbnail.jpeg", "docProps/thumbnail.jpg", "docProps/thumbnail.png", "Thumbnails/thumbnail.png"]
            .into_iter()
            .find(|n| zip.index_for_name(n).is_some())?;
    let entry = zip.by_name(name).ok()?;
    if entry.size() > MAX_EMBEDDED_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    entry.take(MAX_EMBEDDED_BYTES).read_to_end(&mut buf).ok()?;
    decode(&buf)
}

/// Extensions handed to Quick Look: documents and plain text only. Markup (HTML, SVG,
/// XML, web archives) is excluded, as rendering it could fetch remote content.
#[cfg(target_os = "macos")]
const QUICK_LOOK_EXTENSIONS: &[&str] = &[
    "pdf", "doc", "docx", "docm", "dot", "dotx", "odt", "rtf", "pages", "xls", "xlsx", "xlsm", "xlt", "xltx", "ods",
    "csv", "numbers", "ppt", "pptx", "pptm", "pps", "ppsx", "pot", "potx", "odp", "key", "txt", "log", "md", "json",
];

/// Extension of the copy handed to Quick Look, which picks its generator by extension;
/// `None` if Quick Look is not used for this attachment.
#[cfg(target_os = "macos")]
fn quick_look_extension(meta: &AttachmentMeta) -> Option<String> {
    match extension(meta) {
        e if QUICK_LOOK_EXTENSIONS.contains(&e.as_str()) => Some(e),
        // Plain text often has no extension.
        e if e.is_empty() && meta.mime_type.as_deref().is_some_and(|m| m.trim().starts_with("text/plain")) => {
            Some("txt".into())
        }
        _ => None,
    }
}

#[cfg(not(target_os = "macos"))]
fn quick_look_extension(_: &AttachmentMeta) -> Option<String> {
    None
}

/// A renderer gave up within its time limit (the system may be busy): not a final answer.
struct TimedOut;

#[cfg(target_os = "macos")]
fn quick_look(
    meta: &AttachmentMeta,
    bytes: &[u8],
    max_side: u32,
    scratch: &Path,
) -> Result<Option<DynamicImage>, TimedOut> {
    let Some(ext) = quick_look_extension(meta) else { return Ok(None) };
    let _slot = QUICK_LOOK.acquire();
    let Ok(dir) = crate::files::new_open_dir(scratch) else { return Ok(None) };
    let path = dir.join(format!("attachment.{ext}"));
    let png = match fs::write(&path, bytes) {
        Ok(()) => native::quick_look_png(&path, max_side),
        Err(_) => Ok(None),
    };
    let _ = fs::remove_dir_all(&dir);
    Ok(png?.and_then(|png| decode(&png)))
}

#[cfg(not(target_os = "macos"))]
fn quick_look(_: &AttachmentMeta, _: &[u8], _: u32, _: &Path) -> Result<Option<DynamicImage>, TimedOut> {
    Ok(None)
}

#[cfg(target_os = "macos")]
fn native_image(bytes: &[u8], max_side: u32) -> Option<DynamicImage> {
    decode(&native::image_png(bytes, max_side)?)
}

#[cfg(not(target_os = "macos"))]
fn native_image(_: &[u8], _: u32) -> Option<DynamicImage> {
    None
}

/// ImageIO (the decoder behind WebKit and Preview) and Quick Look Thumbnailing.
#[cfg(target_os = "macos")]
mod native {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::AnyThread;
    use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFMutableData, CFNumber, CFString, CFType, CGSize};
    use objc2_core_graphics::CGImage;
    use objc2_foundation::{NSError, NSURL};
    use objc2_image_io::{
        kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
        kCGImageSourceThumbnailMaxPixelSize, CGImageDestination, CGImageSource,
    };
    use objc2_quick_look_thumbnailing::{
        QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes, QLThumbnailGenerator,
        QLThumbnailRepresentation,
    };

    /// Quick Look gives up on a document after this long (its generator runs out of process).
    const QUICK_LOOK_TIMEOUT: Duration = Duration::from_secs(15);

    /// Formats ImageIO may decode here (by type identifier): the raster formats mail
    /// carries. ImageIO also reads PDF, PSD, camera raw and more, none of which are
    /// handed to it as images.
    const IMAGE_TYPES: &[&str] = &[
        "public.jpeg",
        "public.png",
        "public.tiff",
        "public.heic",
        "public.heif",
        "com.compuserve.gif",
        "com.microsoft.bmp",
        "org.webmproject.webp",
    ];

    /// First frame scaled to fit `max_side` (EXIF orientation applied), as PNG. ImageIO
    /// decodes at the reduced size, so a huge image never allocates its full raster.
    pub fn image_png(bytes: &[u8], max_side: u32) -> Option<Vec<u8>> {
        let yes: &CFType = CFBoolean::new(true);
        let size = CFNumber::new_i32(max_side as i32);
        // SAFETY: the keys are ImageIO's exported constants; the values have the
        // documented types (CFBoolean, CFNumber); every call gets valid CF objects.
        unsafe {
            let src = CGImageSource::with_data(&CFData::from_bytes(bytes), None)?;
            // Identifying the type reads only the header; nothing is decoded yet.
            let kind = src.r#type()?.to_string();
            if !IMAGE_TYPES.contains(&kind.as_str()) {
                return None;
            }
            let opts = CFDictionary::<CFString, CFType>::from_slices(
                &[
                    kCGImageSourceCreateThumbnailFromImageAlways,
                    kCGImageSourceCreateThumbnailWithTransform,
                    kCGImageSourceThumbnailMaxPixelSize,
                ],
                &[yes, yes, &size],
            );
            let img = src.thumbnail_at_index(0, Some(opts.as_ref()))?;
            png(&img)
        }
    }

    /// Quick Look thumbnail (never the generic file icon) of the file at `path`, as PNG.
    pub fn quick_look_png(path: &Path, max_side: u32) -> Result<Option<Vec<u8>>, super::TimedOut> {
        let Some(url) = NSURL::from_file_path(path) else { return Ok(None) };
        let side = f64::from(max_side);
        let (tx, rx) = mpsc::channel::<Option<Vec<u8>>>();
        // Called once, on a Quick Look queue; the image is encoded there so only bytes
        // cross threads.
        let done = RcBlock::new(move |rep: *mut QLThumbnailRepresentation, _err: *mut NSError| {
            // SAFETY: Quick Look passes a valid representation or null.
            let png = unsafe { rep.as_ref() }.and_then(|rep| unsafe { png(&rep.CGImage()) });
            let _ = tx.send(png);
        });
        // SAFETY: valid file URL, positive size; the completion block matches the
        // documented signature and owns everything it touches.
        let (generator, request) = unsafe {
            let request = QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
                QLThumbnailGenerationRequest::alloc(),
                &url,
                CGSize::new(side, side),
                1.0,
                QLThumbnailGenerationRequestRepresentationTypes::Thumbnail,
            );
            let generator = QLThumbnailGenerator::sharedGenerator();
            generator.generateBestRepresentationForRequest_completionHandler(&request, &done);
            (generator, request)
        };
        // `done` holds the sender until this returns, so this only fails by timing out.
        rx.recv_timeout(QUICK_LOOK_TIMEOUT).map_err(|_| {
            // Frees the generator's resources; a late completion finds no receiver.
            // SAFETY: the request was submitted to this generator.
            unsafe { generator.cancelRequest(&request) };
            super::TimedOut
        })
    }

    /// # Safety
    /// `img` must be a valid image (any CGImage handed out by the frameworks is).
    unsafe fn png(img: &CGImage) -> Option<Vec<u8>> {
        let out = CFMutableData::new(None, 0)?;
        let dest = unsafe { CGImageDestination::with_data(&out, &CFString::from_static_str("public.png"), 1, None)? };
        unsafe {
            dest.add_image(img, None);
            dest.finalize().then(|| out.to_vec())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn newest_waiter_gets_the_next_slot() {
        static POOL: Pool = Pool::new(1);
        let order = Mutex::new(Vec::new());
        let held = POOL.acquire();
        std::thread::scope(|s| {
            for i in 0..4 {
                let order = &order;
                s.spawn(move || {
                    let _p = POOL.acquire();
                    order.lock().unwrap().push(i);
                });
                // Each waiter queues before the next one is started.
                while POOL.lock().waiting.len() < i + 1 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            drop(held);
        });
        assert_eq!(*order.lock().unwrap(), [3, 2, 1, 0]);
    }

    #[test]
    fn disk_cache_serves_repeats_and_remembers_undrawable_files() {
        let root = std::env::temp_dir().join(format!("stv-preview-test-{}", std::process::id()));
        let png = {
            let mut out = Vec::new();
            RgbImage::from_pixel(600, 300, Rgb([10, 120, 200]))
                .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        let image = AttachmentMeta { filename: Some("chart.png".into()), ..Default::default() };
        let Picture::Jpeg(first) = picture(&image, &png, THUMB_SIDE, &root) else { panic!("png not drawn") };
        assert_eq!(decode(&first).map(|i| (i.width(), i.height())), Some((256, 128)));
        // A second copy of the same file (another message) is read back, not re-rendered.
        let cached = disk_path(&root.join("thumbs"), &image, &png, THUMB_SIDE);
        fs::write(&cached, b"sentinel").unwrap();
        assert!(matches!(picture(&image, &png, THUMB_SIDE, &root), Picture::Jpeg(b) if b == b"sentinel"));

        let garbage = AttachmentMeta { filename: Some("broken.png".into()), ..Default::default() };
        assert!(matches!(picture(&garbage, b"not an image", THUMB_SIDE, &root), Picture::Nothing));
        assert_eq!(fs::read(disk_path(&root.join("thumbs"), &garbage, b"not an image", THUMB_SIDE)).unwrap(), b"");
        fs::remove_dir_all(&root).unwrap();
    }
}
