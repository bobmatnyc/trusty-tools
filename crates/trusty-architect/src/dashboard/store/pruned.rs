//! The prune log: every deleted document's full record, outside the served
//! root.
//!
//! Why: DOC-82 LDG-2.8.4 says a delete appends the full record before the
//! document goes, and fails when that append fails (#10063 AC 5). The page
//! serves the store root, so the log lives outside it (Architect ruling Q5;
//! the prototype's `<root>/../owner-queue-archive/pruned.jsonl`). Writing the
//! trusty-memory note first is the `tm` verb's step (PR 3): this crate has no
//! memory client.
//! What: [`PruneLog`], a [`DeleteArchive`] at a path that must not lie under
//! the store root once symlinks are resolved; it keeps and writes the
//! resolved path, never through a symlink at its last component (a dangling
//! one is refused up front, a late one fails the open).
//! [`DeleteArchive::record`] appends one canonical line per removed document,
//! `{by, collection, data, deleted_at, id, version}` (the AC's shape, owner
//! ruling D34), then fsyncs; [`PruneLog::lines`] returns the
//! raw lines matching an id and collection, oldest first, in either this
//! shape or the prototype's (`pruned_at`, `actor`).
//! Test: `delete_refused_when_pruned_write_fails`, `pruned_by_id_returns_record`,
//! `prune_log_inside_root_refused`, `prune_log_dangling_link_refused`,
//! `prune_log_late_link_refuses_delete`, `pruned_reads_prototype_lines`.

use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use super::super::canonical::compact;
use super::super::error::DashError;
use super::changes::append_durable_nofollow;
use super::{DeleteArchive, Removed};

/// An append-only `pruned.jsonl` outside a store's served root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneLog {
    path: PathBuf,
}

impl PruneLog {
    /// A prune log at `path` for the store served from `served_root`.
    ///
    /// # Errors
    /// [`DashError::Invalid`] when `path` is the root or lies under it
    /// (symlinks resolved where the path exists), either path has a `..`
    /// below a directory that does not exist, or a part of either path is a
    /// dangling symlink; [`DashError::Io`] when the working directory is
    /// needed and unreadable, or an existing part of either path cannot be
    /// resolved.
    pub fn outside(served_root: &Path, path: impl Into<PathBuf>) -> Result<Self, DashError> {
        let path = path.into();
        let (root, at) = (resolve(served_root)?, resolve(&path)?);
        if at.starts_with(&root) {
            return Err(DashError::Invalid(format!(
                "prune log {} (resolved {}) is inside the served root {}: put it outside",
                path.display(),
                at.display(),
                served_root.display()
            )));
        }
        // #10063 df-G01: the checked path is the one written.
        Ok(Self { path: at })
    }

    /// The prototype's default, `<root>/../owner-queue-archive/pruned.jsonl`.
    ///
    /// # Errors
    /// [`DashError::Invalid`] when the root has no parent; [`PruneLog::outside`]'s.
    pub fn default_for(served_root: &Path) -> Result<Self, DashError> {
        let root = resolve(served_root)?;
        let parent = root.parent().ok_or_else(|| {
            DashError::Invalid(format!(
                "store root {} has no parent for the prune log",
                served_root.display()
            ))
        })?;
        Self::outside(
            served_root,
            parent.join("owner-queue-archive").join("pruned.jsonl"),
        )
    }

    /// The log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The raw lines whose `id` and `collection` match (each filter when
    /// given), oldest first; empty when the log does not exist.
    ///
    /// # Errors
    /// [`DashError::Io`] when it cannot be read; [`DashError::Corrupt`] for a
    /// line that is not a JSON object (never skipped).
    pub fn lines(
        &self,
        id: Option<&str>,
        collection: Option<&str>,
    ) -> Result<Vec<String>, DashError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(DashError::io(&self.path)(e)),
        };
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let rec: Value = serde_json::from_str(line)
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| {
                    DashError::corrupt(&self.path, format!("line {}: not a record", n + 1))
                })?;
            let is = |key: &str, want: Option<&str>| {
                want.is_none_or(|w| rec.get(key).and_then(Value::as_str) == Some(w))
            };
            if is("id", id) && is("collection", collection) {
                out.push(line.to_owned());
            }
        }
        Ok(out)
    }
}

impl DeleteArchive for PruneLog {
    fn record(&self, removed: &[Removed<'_>]) -> Result<(), DashError> {
        if removed.is_empty() {
            return Ok(());
        }
        let text: String = removed
            .iter()
            .map(|r| {
                let rec = json!({
                    "by": r.by,
                    "collection": r.collection,
                    "data": r.data,
                    "deleted_at": r.deleted_at,
                    "id": r.id,
                    "version": r.version,
                });
                compact(&rec) + "\n"
            })
            .collect();
        append_durable_nofollow(&self.path, &text)
    }
}

/// `path` as the OS opens it: made absolute, its deepest existing ancestor
/// canonicalized (symlinks resolved before `..`, so `/tmp` and
/// `/private/tmp` compare equal on macOS), the rest appended.
///
/// # Errors
/// [`DashError::Invalid`] for a `..` in the part that does not exist (it
/// cannot be resolved before the directory is made) or a dangling symlink
/// (the OS would create its target); [`DashError::Io`] when the working
/// directory or an existing ancestor cannot be resolved, never a fallback to
/// the unresolved path (#10063 df-G01).
fn resolve(path: &Path) -> Result<PathBuf, DashError> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(DashError::io(Path::new(".")))?
            .join(path)
    };
    let parts: Vec<Component<'_>> = joined.components().collect();
    for n in (1..=parts.len()).rev() {
        let base: PathBuf = parts[..n].iter().collect();
        let mut out = match fs::canonicalize(&base) {
            Ok(out) => out,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                // #10063 df-G01: a dangling link exists; a write follows it.
                if fs::symlink_metadata(&base).is_ok() {
                    return Err(DashError::Invalid(format!(
                        "{} is a dangling symlink: the write would follow it",
                        base.display()
                    )));
                }
                continue;
            }
            Err(e) => return Err(DashError::io(&base)(e)),
        };
        for part in &parts[n..] {
            match part {
                Component::Normal(name) => out.push(name),
                Component::CurDir => {}
                _ => {
                    return Err(DashError::Invalid(format!(
                        "{} has `..` below {}, which does not exist",
                        path.display(),
                        base.display()
                    )));
                }
            }
        }
        return Ok(out);
    }
    Err(DashError::Invalid(format!(
        "{} has no existing ancestor",
        path.display()
    )))
}
