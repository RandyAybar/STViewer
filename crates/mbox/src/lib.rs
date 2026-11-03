//! mbox (mboxo/mboxrd/mboxcl/mboxcl2), Thunderbird profile trees, Gmail Takeout,
//! `.eml` files/directories and Apple Mail (`.mbox` bundles / `.emlx`) reader.
//!
//! - Single mbox files are memory-mapped only while scanning; message boundaries are found
//!   with a parallel `memmem` search and header blocks are parsed in parallel. The compact
//!   result (offsets + header blob per message) is persisted to a versioned sidecar cache
//!   under `dirs::cache_dir()/stviewer/mbox/` so reopening is near-instant and appended
//!   mailboxes only scan their tail.
//! - Messages are read on demand with positioned reads, so live mail clients can still
//!   rewrite/compact their files.

mod discover;
mod emlx;
mod hdr;
mod ical;
mod mime;
mod scan;
#[doc(hidden)]
pub mod smime;
mod source;
mod store;
mod tzid;
mod util;

use std::path::Path;

use mail_model::{MailSource, Result, SourceKind, SourceOpener};

pub use mime::{attachment_from_mime, header_from_mime, message_from_mime, properties_from_mime};
pub use source::{MboxSource, OpenOptions};

/// `SourceOpener` for every format in this crate.
#[derive(Clone, Debug, Default)]
pub struct MboxOpener {
    opts: OpenOptions,
}

impl MboxOpener {
    /// Opener with explicit sidecar-cache options (see [`OpenOptions`]).
    pub fn with_options(opts: OpenOptions) -> Self {
        MboxOpener { opts }
    }
}

impl SourceOpener for MboxOpener {
    fn sniff(&self, path: &Path) -> Option<SourceKind> {
        let meta = std::fs::metadata(path).ok()?;
        if meta.is_dir() {
            discover::sniff_dir(path)
        } else {
            discover::sniff_file(path)
        }
    }

    fn open(&self, path: &Path) -> Result<Box<dyn MailSource>> {
        Ok(Box::new(MboxSource::open(path, &self.opts)?))
    }
}

static OPENER: MboxOpener = MboxOpener { opts: OpenOptions { cache_dir: None, no_cache: false } };

/// Default opener (cache under `dirs::cache_dir()/stviewer/mbox`).
pub fn opener() -> &'static dyn SourceOpener {
    &OPENER
}
