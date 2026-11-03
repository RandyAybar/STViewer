//! First pages of PDF attachments, rasterised by `hayro` in a short-lived worker process:
//! this executable started with [`WORKER_ARG`], reading the PDF on stdin and writing the
//! JPEG to stdout.
//!
//! hayro is memory-safe, but it trusts the sizes a file declares: a few hundred bytes can
//! describe a gigapixel fax image or a deflate stream that inflates to gigabytes, and a
//! failed allocation aborts the whole process. So the worker runs with an allocation
//! budget ([`Allocator`]) and a wall-clock limit; whatever happens to it, the app only
//! sees "no picture".

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed};
use std::time::Duration;

use image::{DynamicImage, RgbaImage};

use super::{to_jpeg, TimedOut, MAX_DOCUMENT_BYTES, PREVIEW_SIDE};

/// First argument that turns this executable into a PDF worker; the second is the side.
const WORKER_ARG: &str = "--stv-pdf-worker";
/// Memory a worker may hold at once: the file, its decoded streams and images, the pixmap.
const WORKER_MEMORY: isize = 1 << 30;
/// Wall-clock limit of a worker, after which it exits with [`TIMED_OUT`].
const WORKER_TIME: Duration = Duration::from_secs(20);
/// Exit status of a worker that ran out of time (the system may be busy): not final.
const TIMED_OUT: u8 = 3;
/// Larger worker output is not a picture this module produced.
const MAX_OUTPUT: u64 = 16 << 20;

/// First page of `pdf` as a JPEG fitting `max_side`, drawn by a worker process. `Ok(None)`
/// when it cannot be drawn (malformed, over budget, or no worker could be started).
pub fn first_page(pdf: &[u8], max_side: u32) -> Result<Option<Vec<u8>>, TimedOut> {
    let Ok(exe) = std::env::current_exe() else { return Ok(None) };
    let mut cmd = Command::new(exe);
    cmd.arg(WORKER_ARG).arg(max_side.to_string()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `CREATE_NO_WINDOW`: debug builds are console programs.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(mut child) = cmd.spawn() else { return Ok(None) };
    // The worker reads all of its input before writing anything, so writing everything
    // first cannot deadlock. A worker that already died just closes the pipe.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(pdf);
    }
    let mut jpeg = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout.take(MAX_OUTPUT + 1).read_to_end(&mut jpeg);
    }
    if jpeg.len() as u64 > MAX_OUTPUT {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    }
    // Bounded by the worker's own time limit.
    match child.wait() {
        Ok(status) if status.success() && !jpeg.is_empty() => Ok(Some(jpeg)),
        Ok(status) if status.code() == Some(TIMED_OUT.into()) => Err(TimedOut),
        _ => Ok(None),
    }
}

/// Runs the PDF worker when this process was started as one (`None`: run the app).
pub fn worker_main() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != WORKER_ARG {
        return None;
    }
    std::thread::spawn(|| {
        std::thread::sleep(WORKER_TIME);
        std::process::exit(TIMED_OUT.into());
    });
    LIMITED.store(true, Relaxed);
    let side = args.next().and_then(|s| s.to_str()?.parse::<u32>().ok()).filter(|s| (1..=PREVIEW_SIDE).contains(s));
    let mut pdf = Vec::new();
    let read = std::io::stdin().lock().take(MAX_DOCUMENT_BYTES as u64 + 1).read_to_end(&mut pdf);
    let jpeg = match (side, read) {
        (Some(side), Ok(n)) if n <= MAX_DOCUMENT_BYTES => render(pdf, side),
        _ => None,
    };
    let written = jpeg.is_some_and(|jpeg| std::io::stdout().lock().write_all(&jpeg).is_ok());
    Some(if written { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// The worker's job: first page rasterised so its longest side is `max_side`, as JPEG.
fn render(pdf: Vec<u8>, max_side: u32) -> Option<Vec<u8>> {
    use hayro::hayro_interpret::InterpreterSettings;
    use hayro::hayro_syntax::Pdf;
    use hayro::vello_cpu::color::palette::css::WHITE;
    use hayro::{PixmapSettings, RenderCache, RenderSettings};

    let pdf = Pdf::new(pdf).ok()?;
    let pages = pdf.pages();
    let page = pages.first()?;
    let (w, h) = page.render_dimensions();
    if !(w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0) {
        return None;
    }
    // Small pages are enlarged at most 4×; a sliver that would be under a pixel wide is
    // not drawn.
    let scale = (max_side as f32 / w.max(h)).min(4.0);
    if w.min(h) * scale < 1.0 {
        return None;
    }
    let settings = PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE };
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &settings,
    );
    // On an opaque background premultiplied RGBA equals straight RGBA.
    let rgba =
        RgbaImage::from_raw(u32::from(pixmap.width()), u32::from(pixmap.height()), pixmap.data_as_u8_slice().to_vec())?;
    to_jpeg(&DynamicImage::ImageRgba8(rgba), max_side)
}

// ---- allocation budget ----

/// Set in a worker process: allocations are charged against [`REMAINING`].
static LIMITED: AtomicBool = AtomicBool::new(false);
static REMAINING: AtomicIsize = AtomicIsize::new(WORKER_MEMORY);

/// The process allocator: the system allocator, which in a worker process refuses
/// allocations beyond [`WORKER_MEMORY`] (the process then aborts, as on any failed
/// allocation). In the app it adds one relaxed load per call.
pub struct Allocator;

fn charge(size: usize) -> bool {
    if !LIMITED.load(Relaxed) {
        return true;
    }
    let Ok(size) = isize::try_from(size) else { return false };
    // A compare-exchange loop rather than `fetch_update`, which newer toolchains deprecate
    // under a name the minimum supported Rust does not have.
    let mut left = REMAINING.load(Relaxed);
    loop {
        let Some(next) = left.checked_sub(size).filter(|n| *n >= 0) else { return false };
        match REMAINING.compare_exchange_weak(left, next, Relaxed, Relaxed) {
            Ok(_) => return true,
            Err(actual) => left = actual,
        }
    }
}

fn refund(size: usize) {
    if LIMITED.load(Relaxed) {
        // `Layout` sizes never exceed `isize::MAX`.
        REMAINING.fetch_add(size as isize, Relaxed);
    }
}

// SAFETY: every call is forwarded to `System` unchanged, or refused with null before it
// reaches `System`, which `GlobalAlloc` allows.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !charge(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded with the caller's guarantees.
        let ptr = unsafe { System.alloc(layout) };
        if ptr.is_null() {
            refund(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !charge(layout.size()) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded with the caller's guarantees.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if ptr.is_null() {
            refund(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded with the caller's guarantees.
        unsafe { System.dealloc(ptr, layout) };
        refund(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_size = layout.size();
        let growth = new_size.saturating_sub(old_size);
        if !charge(growth) {
            return std::ptr::null_mut();
        }
        // SAFETY: forwarded with the caller's guarantees.
        let ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if ptr.is_null() {
            refund(growth);
        } else {
            refund(old_size.saturating_sub(new_size));
        }
        ptr
    }
}
