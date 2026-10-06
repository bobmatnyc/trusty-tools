//! Map a `remove-file` request path onto the index's stored file keys (#9236).
//!
//! Why: chunk keys are index-relative, but search answers carry absolute
//! paths. `remove-file` matched the path exactly, so an absolute path removed
//! nothing and answered `200 removed_chunks: 0`.
//! What: [`remove_keys`] relativizes an absolute path with the watcher's own
//! rule and refuses one outside the root with a 400 naming the accepted forms.
//! Test: `remove_file_takes_an_absolute_in_root_path_9236` and its two
//! siblings in `crate::service::rpc::writes`'s tests.

use std::path::Path;

use axum::http::StatusCode;

use super::helpers::file_is_within_root;
use crate::service::watch_loop::watcher_relative_path;

/// The stored keys one `remove-file` path names, normalized key first.
///
/// Why: see the module doc. An absolute path inside the root must remove the
/// same chunks as its index-relative form, and one outside the root must be
/// refused rather than answered as a successful no-op.
/// What: a relative path is returned unchanged, as one key. An absolute path
/// is relativized by [`watcher_relative_path`] against the canonical and raw
/// root — the rule the watcher keys its writes with, symlinked and
/// `/var`↔`/private/var` roots included. The literal path follows as a second
/// key, because `index-file` stores a pushed absolute path verbatim. A result
/// that stays absolute, is empty, or climbs out with `..` is refused with
/// `400 remove_file_path_outside_root`.
/// Test: `remove_file_takes_an_absolute_in_root_path_9236`,
/// `remove_file_refuses_a_path_outside_the_root_9236`,
/// `remove_file_still_removes_a_pushed_absolute_key_9236`.
pub(super) fn remove_keys(
    index_id: &str,
    root: &Path,
    path: &str,
) -> Result<Vec<String>, (StatusCode, serde_json::Value)> {
    if !Path::new(path).is_absolute() {
        return Ok(vec![path.to_string()]);
    }
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let key = watcher_relative_path(&canonical_root, root, Path::new(path));
    // The absolute check comes first, so `file_is_within_root` only ever sees
    // a relative key and never takes its blocking canonicalize branch.
    if Path::new(&key).is_absolute() || !file_is_within_root(&key, root) {
        return Err(outside_root(index_id, root, path));
    }
    Ok(vec![key, path.to_string()])
}

/// The 400 an absolute path outside the index root answers.
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
