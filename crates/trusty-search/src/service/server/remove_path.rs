//! Map a `remove-file` or `index-file` request path onto the index's stored
//! file keys (#9236, #9510).
//!
//! Why: chunk keys are index-relative, but search answers carry absolute
//! paths. `remove-file` matched the path exactly, so an absolute path removed
//! nothing and answered `200 removed_chunks: 0`; `index-file` stored it
//! verbatim, beside the file's index-relative chunks.
//! What: [`remove_keys`] and [`index_key`] relativize an absolute path against
//! the index's roots by text (#7434: every root, keyed `@root<n>/…` under an
//! additional one) and refuse any path that names no file under a root with a
//! 400 naming the accepted forms.
//! Test: `remove_file_takes_an_absolute_in_root_path_9236` and its siblings,
//! and `index_file_by_an_absolute_in_root_path_replaces_its_chunks_9510` and
//! its siblings, in `crate::service::rpc::writes`'s tests.

use std::path::{Component, Path};

use axum::http::StatusCode;

use super::helpers::file_is_within_root;
use crate::core::index_roots::{stored_path_for_slot, IndexRoots};

/// The stored keys one `remove-file` path names, normalized key first.
///
/// Why: see the module doc. An absolute path inside the root must remove the
/// same chunks as its index-relative form, and a path naming no file under
/// the root must be refused rather than answered as a successful no-op.
/// What: a relative path is returned unchanged, as one key. An absolute path
/// is relativized by [`stored_key`]; the literal path follows as a second
/// key, because `index-file` stored a pushed absolute path verbatim before
/// #9510 and such keys may still be indexed. A key
/// that is empty, only `.`, or holds a `..` segment is refused with
/// `400 remove_file_path_outside_root`.
/// Test: `remove_file_takes_an_absolute_in_root_path_9236`,
/// `remove_file_refuses_a_path_outside_the_root_9236`,
/// `remove_file_refuses_a_relative_path_outside_the_root_9236`,
/// `remove_file_by_a_symlinked_path_removes_the_link_key_9236`,
/// `remove_file_still_removes_a_pushed_absolute_key_9236`,
/// `remove_file_maps_an_additional_root_path_to_its_stored_key`.
pub(super) fn remove_keys(
    index_id: &str,
    roots: &IndexRoots,
    path: &str,
) -> Result<Vec<String>, (StatusCode, serde_json::Value)> {
    let root = roots.primary();
    // #9236: a relative path is checked too, with no syscall — `""`, `.` and a
    // `..` climb used to reach the store and answer `200 removed_chunks: 0`.
    if !Path::new(path).is_absolute() {
        if !names_an_in_root_file(path, root) {
            return Err(outside_root(index_id, roots, path));
        }
        return Ok(vec![path.to_string()]);
    }
    match stored_key(roots, Path::new(path)) {
        Some(key) => Ok(vec![key, path.to_string()]),
        None => Err(outside_root(index_id, roots, path)),
    }
}

/// The stored key one `index-file` path is written under (#9510).
///
/// Why: an absolute path was stored verbatim, so re-adding a file by its
/// absolute path duplicated its index-relative chunks, and no `path_prefix`
/// search or reindex could reach the copy.
/// What: a relative path is returned unchanged, as before #9510. An absolute
/// path maps to the key [`remove_keys`] removes first, so the watcher, a
/// reindex and `index-file` agree on it; one naming no file under a root is
/// refused with `400 index_file_path_outside_root`.
/// Test: `index_file_by_an_absolute_in_root_path_replaces_its_chunks_9510`,
/// `index_file_refuses_an_absolute_path_outside_the_root_9510`,
/// `index_file_keeps_a_relative_path_unchanged_9510`.
pub(super) fn index_key(
    index_id: &str,
    roots: &IndexRoots,
    path: &str,
) -> Result<String, (StatusCode, serde_json::Value)> {
    if !Path::new(path).is_absolute() {
        return Ok(path.to_string());
    }
    stored_key(roots, Path::new(path)).ok_or_else(|| index_outside_root(index_id, roots, path))
}

/// The stored key of the absolute `path`, or `None` when it names no file
/// under any root.
fn stored_key(roots: &IndexRoots, path: &Path) -> Option<String> {
    let root = roots.primary();
    // #9236: relativized by text first, so an in-root symlink keeps its key.
    // #7434: against every root, deepest first; a file under additional slot
    // `n` maps to its stored `@root<n+1>/…` key, the one the hash forget uses.
    let mut table: Vec<(Option<usize>, &Path)> = std::iter::once((None, root))
        .chain(
            roots
                .additional()
                .iter()
                .enumerate()
                .map(|(n, r)| (Some(n), r.as_path())),
        )
        .collect();
    table.sort_by_key(|(_, r)| std::cmp::Reverse(r.components().count()));
    for (slot, r) in table {
        if let Some(rel) = absolute_key(r, path) {
            if !names_an_in_root_file(&rel, r) {
                return None;
            }
            return Some(stored_path_for_slot(slot, &rel));
        }
    }
    None
}

/// True when the relative `key` names a file under the root.
///
/// What: [`file_is_within_root`]'s relative branch — non-empty, no `..`
/// segment, no syscall — plus at least one named component, so `.` and `./`
/// (the root itself) are refused too.
fn names_an_in_root_file(key: &str, root: &Path) -> bool {
    let key_path = Path::new(key);
    !key_path.is_absolute()
        && file_is_within_root(key, root)
        && key_path
            .components()
            .any(|c| matches!(c, Component::Normal(_)))
}

/// The index-relative form of the absolute `path`, or `None` outside the root.
///
/// Why: #9236 review — canonicalizing the whole path resolves its last
/// component, so an in-root symlink `a.rs -> b.rs` named `b.rs`'s key.
/// What: strips the raw root, then the canonical root, by text. Only when both
/// fail is the PARENT directory canonicalized — never the last component — and
/// the canonical root stripped from it, so a `/var` vs `/private/var` or a
/// directory-alias path still maps. A path ending in `..` is `None`.
/// Test: `remove_file_by_a_symlinked_path_removes_the_link_key_9236`.
fn absolute_key(root: &Path, path: &Path) -> Option<String> {
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(rel.to_string_lossy().into_owned());
    }
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if let Ok(rel) = path.strip_prefix(&canonical_root) {
        return Some(rel.to_string_lossy().into_owned());
    }
    // #9236: resolve the parent only; the last component may be a symlink
    // whose own key is the one asked for.
    let name = path.file_name()?;
    let parent = std::fs::canonicalize(path.parent()?).ok()?;
    let resolved = parent.join(name);
    let rel = resolved.strip_prefix(&canonical_root).ok()?;
    Some(rel.to_string_lossy().into_owned())
}

/// Every root the path could have been under, for a refusal message (#7434).
fn root_list(roots: &IndexRoots) -> String {
    roots
        .all()
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join("`, `")
}

/// The 400 an `index-file` path naming no file under the index root answers.
fn index_outside_root(
    index_id: &str,
    roots: &IndexRoots,
    path: &str,
) -> (StatusCode, serde_json::Value) {
    let root = root_list(roots);
    (
        StatusCode::BAD_REQUEST,
        serde_json::json!({
            "error": "index_file_path_outside_root",
            "index_id": index_id,
            "path": path,
            "indexed": false,
            "chunks": 0,
            "message": format!(
                "index-file takes an index-relative path (such as `src/lib.rs`) or an \
                 absolute path to a file under `{root}`; `{path}` is neither, so nothing \
                 was indexed"
            ),
        }),
    )
}

/// The 400 a `remove-file` path naming no file under the index root answers.
fn outside_root(index_id: &str, roots: &IndexRoots, path: &str) -> (StatusCode, serde_json::Value) {
    let root = root_list(roots);
    (
        StatusCode::BAD_REQUEST,
        serde_json::json!({
            "error": "remove_file_path_outside_root",
            "index_id": index_id,
            "path": path,
            "removed_chunks": 0,
            "message": format!(
                "remove-file takes an index-relative path (such as `src/lib.rs`) or an \
                 absolute path to a file under `{root}`; `{path}` is neither, so nothing \
                 was removed"
            ),
        }),
    )
}
