//! Map a `remove-file` request path onto the index's stored file keys (#9236).
//!
//! Why: chunk keys are index-relative, but search answers carry absolute
//! paths. `remove-file` matched the path exactly, so an absolute path removed
//! nothing and answered `200 removed_chunks: 0`.
//! What: [`remove_keys`] relativizes an absolute path against the root by
//! text and refuses any path that names no file under the root with a 400
//! naming the accepted forms.
//! Test: `remove_file_takes_an_absolute_in_root_path_9236` and its siblings
//! in `crate::service::rpc::writes`'s tests.

use std::path::{Component, Path};

use axum::http::StatusCode;

use super::helpers::file_is_within_root;

/// The stored keys one `remove-file` path names, normalized key first.
///
/// Why: see the module doc. An absolute path inside the root must remove the
/// same chunks as its index-relative form, and a path naming no file under
/// the root must be refused rather than answered as a successful no-op.
/// What: a relative path is returned unchanged, as one key. An absolute path
/// is relativized by [`absolute_key`]; the literal path follows as a second
/// key, because `index-file` stores a pushed absolute path verbatim. A key
/// that is empty, only `.`, or holds a `..` segment is refused with
/// `400 remove_file_path_outside_root`.
/// Test: `remove_file_takes_an_absolute_in_root_path_9236`,
/// `remove_file_refuses_a_path_outside_the_root_9236`,
/// `remove_file_refuses_a_relative_path_outside_the_root_9236`,
/// `remove_file_by_a_symlinked_path_removes_the_link_key_9236`,
/// `remove_file_still_removes_a_pushed_absolute_key_9236`.
pub(super) fn remove_keys(
    index_id: &str,
    root: &Path,
    path: &str,
) -> Result<Vec<String>, (StatusCode, serde_json::Value)> {
    // #9236: a relative path is checked too, with no syscall — `""`, `.` and a
    // `..` climb used to reach the store and answer `200 removed_chunks: 0`.
    if !Path::new(path).is_absolute() {
        if !names_an_in_root_file(path, root) {
            return Err(outside_root(index_id, root, path));
        }
        return Ok(vec![path.to_string()]);
    }
    // #9236: relativized by text first, so an in-root symlink keeps its key.
    match absolute_key(root, Path::new(path)) {
        Some(key) if names_an_in_root_file(&key, root) => Ok(vec![key, path.to_string()]),
        _ => Err(outside_root(index_id, root, path)),
    }
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

/// The 400 a path naming no file under the index root answers.
fn outside_root(index_id: &str, root: &Path, path: &str) -> (StatusCode, serde_json::Value) {
    let root = root.display();
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
