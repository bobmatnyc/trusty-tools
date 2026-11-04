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
//! opens each directory relative to the previous directory handle with
//! `O_NOFOLLOW | O_DIRECTORY`, then the leaf relative to the last one with
//! `O_NOFOLLOW | O_NONBLOCK`, and takes the leaf's type and size from that
//! handle (`fstat`) before it reads the same handle. So a link, FIFO or
//! larger file swapped in after the listing, or a parent directory swapped
//! for a link, is refused or never read. Slice S8 of #10746; race fix in S9.
//! Test: `c71_import_leaves_the_source_tree_unchanged`,
//! `c81_symlinks_are_reported_and_never_read`,
//! `fetch_never_reads_a_symlink_swapped_in_before_the_read`,
//! `fetch_never_blocks_on_a_fifo_swapped_in_before_the_read`,
//! `fetch_size_check_and_read_act_on_one_inode`,
//! `fetch_never_reads_through_a_parent_swapped_for_a_symlink`.

use std::fs::File;
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListEntry {
    /// The source's immutable item ID; for a tree, the source-relative path.
    pub item_id: String,
    /// The item's own timestamp, ISO-8601, when known.
    pub timestamp: Option<String>,
    /// Why the item is not read, when it is not (§2.14 rule 8).
    pub skip: Option<String>,
    /// #10746 S9: the item's group, for mail its thread ID (§2.15 item 6).
    pub group: Option<String>,
    /// #10746 S9: the source revision, when the listing reports one (mail:
    /// the message ID; docs: `version`, else `modifiedTime`). An item with
    /// a revision equal to its ledger record is not fetched (DOC-55 C2).
    pub revision: Option<String>,
    /// #10746 S9: the source marks the item deleted (docs `trashed`).
    pub deleted: bool,
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
        ..ListEntry::default()
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
        let mut parts = Vec::new();
        for c in Path::new(item_id).components() {
            let Component::Normal(part) = c else {
                return Err(fail(
                    "excluded",
                    format!("{item_id}: not a plain relative path"),
                ));
            };
            parts.push(part);
        }
        // #10746 S9 fix round: every component is opened relative to the
        // previous directory handle, never followed through a link, and the
        // leaf is checked and read on the one handle it opened.
        let file = open_beneath(&self.root, &parts)
            .map_err(|(code, e)| fail(code, format!("{item_id}: {e}")))?;
        crate::bundle::ops::read_seam(&self.root.join(item_id));
        read_checked(file).map_err(|(code, msg)| fail(code, format!("{item_id}: {msg}")))
    }
}

type OpenFail = (&'static str, std::io::Error);

/// Open `root/<parts>` for reading without following any symbolic link:
/// each directory is opened relative to the previous one with
/// `O_NOFOLLOW | O_DIRECTORY`, then the leaf with `O_NOFOLLOW | O_NONBLOCK`
/// (so a FIFO never blocks). A directory swapped for a link after an earlier
/// check is refused, and one swapped after it was opened is never left.
/// Errors carry the report code: a link is `symlink`; a socket or FIFO the
/// platform refuses to open is `unsupported_format`; else `fetch_error`.
#[cfg(unix)]
fn open_beneath(root: &Path, parts: &[&std::ffi::OsStr]) -> Result<File, OpenFail> {
    use rustix::fs::{Mode, OFlags, open, openat};
    use rustix::io::Errno;
    let io = |code: &'static str, e: Errno| (code, std::io::Error::from(e));
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let Some((leaf, dirs)) = parts.split_last() else {
        return Err(("unsupported_format", std::io::Error::other("not a file")));
    };
    let mut dir = open(root, dir_flags, Mode::empty()).map_err(|e| io("fetch_error", e))?;
    for d in dirs {
        dir = openat(&dir, *d, dir_flags | OFlags::NOFOLLOW, Mode::empty()).map_err(|e| {
            let code = match e {
                Errno::LOOP | Errno::NOTDIR => "symlink",
                _ => "fetch_error",
            };
            io(code, e)
        })?;
    }
    crate::bundle::ops::walk_seam(&parts.iter().fold(root.to_path_buf(), |p, s| p.join(s)));
    let leaf_flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = openat(&dir, *leaf, leaf_flags, Mode::empty()).map_err(|e| {
        let code = match e {
            Errno::LOOP => "symlink",
            Errno::NXIO | Errno::OPNOTSUPP => "unsupported_format",
            _ => "fetch_error",
        };
        io(code, e)
    })?;
    Ok(File::from(fd))
}

/// The portable fallback: refuse a link on any component, then open.
#[cfg(not(unix))]
fn open_beneath(root: &Path, parts: &[&std::ffi::OsStr]) -> Result<File, OpenFail> {
    let mut probe = root.to_path_buf();
    for part in parts {
        probe.push(part);
        let meta = std::fs::symlink_metadata(&probe).map_err(|e| ("fetch_error", e))?;
        if meta.file_type().is_symlink() {
            return Err(("symlink", std::io::Error::other("crosses a link")));
        }
    }
    File::open(&probe).map_err(|e| ("fetch_error", e))
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
