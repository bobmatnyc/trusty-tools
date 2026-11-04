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
//! re-checks every segment for a link before it reads, so a link swapped in
//! after the listing is refused too. Slice S8 of #10746.
//! Test: `c71_import_leaves_the_source_tree_unchanged`,
//! `c81_symlinks_are_reported_and_never_read`.

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
        let mut probe = self.root.clone();
        for c in Path::new(item_id).components() {
            let Component::Normal(part) = c else {
                return Err(fail(
                    "excluded",
                    format!("{item_id}: not a plain relative path"),
                ));
            };
            probe.push(part);
            let meta = std::fs::symlink_metadata(&probe)
                .map_err(|e| fail("fetch_error", format!("{item_id}: {e}")))?;
            if meta.file_type().is_symlink() {
                return Err(fail("symlink", format!("{item_id}: crosses a link")));
            }
        }
        let meta = std::fs::symlink_metadata(&probe)
            .map_err(|e| fail("fetch_error", format!("{item_id}: {e}")))?;
        // #10746 S8 fix: a FIFO, socket or device swapped in after the
        // listing would block the read under the bundle lock; only a regular
        // file is read.
        if !meta.file_type().is_file() {
            return Err(fail(
                "unsupported_format",
                format!("{item_id}: not a regular file"),
            ));
        }
        if meta.len() > MAX_ITEM_BYTES {
            return Err(fail(
                "too_large",
                format!("{item_id}: {} bytes", meta.len()),
            ));
        }
        std::fs::read(&probe).map_err(|e| fail("fetch_error", format!("{item_id}: {e}")))
    }
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
