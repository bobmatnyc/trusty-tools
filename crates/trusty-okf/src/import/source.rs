//! The import source contract and the document-tree adapter (SPEC-OKF-01
//! §2.13 "Two adapter contracts", §2.14 rules 1, 8 and 9).
//!
//! Why: one engine serves every source, so the engine sees a source only
//! through DOC-55's `Connector` shape: a resumable listing with an explicit
//! `complete` flag, and a per-item fetch. The local tree is the one adapter
//! this crate implements; it needs no credential (B6).
//! What: [`ImportSource`] lists [`ListEntry`] pages and fetches one item's
//! bytes. [`DocTreeSource`] walks a policy-permitted root in sorted order
//! and never writes under it. A hidden path (a `.` segment, the policy's
//! name and flag rules) is listed once as `hidden` and not descended; a
//! symbolic link is listed as `symlink` and never read, even when it points
//! inside the root; a file over [`MAX_ITEM_BYTES`] is `too_large`. `fetch`
//! re-checks every directory segment for a link, then opens the file once
//! with `O_NOFOLLOW | O_NONBLOCK` and takes its type and size from that
//! handle (`fstat`) before it reads the same handle. So a link, FIFO or
//! larger file swapped in after the listing, or between the check and the
//! read, is refused or never read. Slice S8 of #10746; race fix in S9.
//! Test: `c71_import_leaves_the_source_tree_unchanged`,
//! `c81_symlinks_are_reported_and_never_read`,
//! `fetch_never_reads_a_symlink_swapped_in_before_the_read`,
//! `fetch_never_blocks_on_a_fifo_swapped_in_before_the_read`,
//! `fetch_size_check_and_read_act_on_one_inode`.

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use walkdir::WalkDir;

use crate::OkfError;
use crate::build::policy::{DocStorePolicy, hidden_segment_below};
use crate::bundle::paths::io_err;
use crate::extract::ExtractError;

/// The input cap per item (§2.16 limits; the doc-store `MAX_FILE_BYTES`).
pub const MAX_ITEM_BYTES: u64 = 8 * 1024 * 1024;

/// One listed item (DOC-55 §5.1 descriptor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    /// The source's immutable item ID; for a tree, the source-relative path.
    pub item_id: String,
    /// The item's own timestamp, ISO-8601, when known.
    pub timestamp: Option<String>,
    /// Why the item is not read, when it is not (§2.14 rule 8).
    pub skip: Option<String>,
}

/// One page of a listing (§2.13 "Listing completeness").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// The page's items.
    pub entries: Vec<ListEntry>,
    /// Where the next page starts; `None` when no page remains.
    pub cursor: Option<String>,
    /// The adapter's own completeness flag. It alone never makes a listing
    /// exhausted; the engine decides.
    pub complete: bool,
}

/// DOC-55's `Connector`, as the import engine sees a source.
pub trait ImportSource {
    /// One page of the listing, from `cursor`.
    fn list(&mut self, cursor: Option<&str>) -> Result<Listing, OkfError>;
    /// One item's bytes. A failure fails that item only (DOC-55 E3).
    fn fetch(&mut self, item_id: &str) -> Result<Vec<u8>, ExtractError>;
}

/// The local document-tree adapter (§2.14).
#[derive(Debug, Clone)]
pub struct DocTreeSource {
    root: PathBuf,
}

impl DocTreeSource {
    /// Open `dir` under `policy`; the root is canonicalised once (rule 9).
    ///
    /// Errors: `PolicyRefused` when the policy does not permit `dir`.
    pub fn open(dir: &Path, policy: &DocStorePolicy) -> Result<DocTreeSource, OkfError> {
        Ok(DocTreeSource {
            root: policy.permit(dir)?,
        })
    }

    /// The canonical source root.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn entry(rel: String, timestamp: Option<String>, skip: Option<&str>) -> ListEntry {
    ListEntry {
        item_id: rel,
        timestamp,
        skip: skip.map(str::to_string),
    }
}

impl ImportSource for DocTreeSource {
    /// The whole tree in one complete page, path-sorted.
    fn list(&mut self, _cursor: Option<&str>) -> Result<Listing, OkfError> {
        let mut out = Listing {
            complete: true,
            ..Listing::default()
        };
        let mut walk = WalkDir::new(&self.root)
            .follow_links(false)
            .sort_by_file_name()
            .min_depth(1)
            .into_iter();
        while let Some(next) = walk.next() {
            let e = next.map_err(|e| {
                let path = e
                    .path()
                    .map_or_else(|| self.root.clone(), Path::to_path_buf);
                io_err(&path, std::io::Error::other(e.to_string()))
            })?;
            let rel = rel_of(&self.root, e.path());
            let ft = e.file_type();
            if ft.is_symlink() {
                out.entries.push(entry(rel, None, Some("symlink")));
                continue;
            }
            if hidden_segment_below(&self.root, e.path()).is_some() {
                out.entries.push(entry(rel, None, Some("hidden")));
                if ft.is_dir() {
                    walk.skip_current_dir();
                }
                continue;
            }
            if ft.is_dir() {
                continue;
            }
            let meta = e
                .metadata()
                .map_err(|err| io_err(e.path(), std::io::Error::other(err.to_string())))?;
            let ts = modified_iso(&meta);
            let skip = if !ft.is_file() {
                Some("unsupported_format")
            } else if meta.len() > MAX_ITEM_BYTES {
                Some("too_large")
            } else {
                None
            };
            out.entries.push(entry(rel, ts, skip));
        }
        Ok(out)
    }

    fn fetch(&mut self, item_id: &str) -> Result<Vec<u8>, ExtractError> {
        let fail = |code: &str, message: String| ExtractError {
            code: code.to_string(),
            message,
        };
        let parts: Vec<_> = Path::new(item_id).components().collect();
        let mut probe = self.root.clone();
        for (i, c) in parts.iter().enumerate() {
            let Component::Normal(part) = c else {
                return Err(fail(
                    "excluded",
                    format!("{item_id}: not a plain relative path"),
                ));
            };
            probe.push(part);
            if i + 1 == parts.len() {
                break;
            }
            let meta = std::fs::symlink_metadata(&probe)
                .map_err(|e| fail("fetch_error", format!("{item_id}: {e}")))?;
            if meta.file_type().is_symlink() {
                return Err(fail("symlink", format!("{item_id}: crosses a link")));
            }
        }
        // #10746 S9 (S8 race fix): open once without following a final link
        // and without blocking on a FIFO, then check and read that one
        // handle. A file swapped in after the open is never read.
        let file = open_no_follow(&probe).map_err(|e| {
            let code = open_error_code(&e);
            fail(code, format!("{item_id}: {e}"))
        })?;
        crate::bundle::ops::read_seam(&probe);
        read_checked(file).map_err(|(code, msg)| fail(code, format!("{item_id}: {msg}")))
    }
}

/// Open `path` read-only, refusing a final symbolic link and never blocking
/// on a FIFO (`O_NOFOLLOW | O_NONBLOCK` on Unix).
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::other("a symbolic link"));
    }
    opts.open(path)
}

/// The report code of a failed open: a final link is `symlink`; a socket
/// or a FIFO with no writer that the platform refuses to open is
/// `unsupported_format`; anything else is `fetch_error`.
fn open_error_code(e: &std::io::Error) -> &'static str {
    #[cfg(unix)]
    match e.raw_os_error() {
        Some(libc::ELOOP) => return "symlink",
        Some(libc::ENXIO | libc::EOPNOTSUPP) => return "unsupported_format",
        _ => {}
    }
    #[cfg(not(unix))]
    if e.kind() == std::io::ErrorKind::Other {
        return "symlink";
    }
    "fetch_error"
}

/// Check the type and size of the open file (`fstat`) and read it, both on
/// the one handle; the read is bounded so a file that grows after the check
/// is still refused as `too_large`.
fn read_checked(file: File) -> Result<Vec<u8>, (&'static str, String)> {
    let meta = file
        .metadata()
        .map_err(|e| ("fetch_error", e.to_string()))?;
    // #10746 S8 fix, kept: a FIFO, socket or device is never read.
    if !meta.file_type().is_file() {
        return Err(("unsupported_format", "not a regular file".into()));
    }
    if meta.len() > MAX_ITEM_BYTES {
        return Err(("too_large", format!("{} bytes", meta.len())));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ITEM_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ("fetch_error", e.to_string()))?;
    if bytes.len() as u64 > MAX_ITEM_BYTES {
        return Err(("too_large", format!("over {MAX_ITEM_BYTES} bytes")));
    }
    Ok(bytes)
}

/// `path` relative to `root`, `/`-separated.
fn rel_of(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// A file's mtime as an ISO-8601 UTC timestamp, when the platform reports one.
fn modified_iso(meta: &std::fs::Metadata) -> Option<String> {
    let secs = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let dt = chrono::DateTime::from_timestamp(i64::try_from(secs).ok()?, 0)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(all(test, unix))]
#[path = "source_tests.rs"]
mod tests;
