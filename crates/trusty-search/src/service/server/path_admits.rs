//! `search.index.path.admits`: does this index admit one path? (#10739)
//!
//! Why: a caller that must prove a binding never indexes a tree (SPEC-OKF-01
//! §5.3.1 rule 4) would otherwise copy the daemon's glob, allowlist and
//! skip-dir rules, which are crate-private and change. Asking the daemon uses
//! the policy the reindex walk, the watcher and pushed writes already share.
//! What: [`path_admits_report`] answers `{admitted, reason}` for an
//! index-relative (or absolute in-root) path through
//! [`crate::service::write_admission::admits_pushed`]; `admitted` is
//! `true`, `false` or `null` (undecided), and the `reason` names the first
//! rule that refuses it. [`under_any_root`] is the subtree test the
//! `include_paths` purge shares.
//! Test: `path_admits_agrees_with_the_reindex_walk`,
//! `path_admits_names_the_refusing_rule`, `path_admits_refuses_an_unknown_index`,
//! `path_admits_never_reports_an_undecided_answer_as_excluded`.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use super::state::SearchAppState;
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::index_admission::{self, Admission};

#[cfg(test)]
#[path = "path_admits_10739_tests.rs"]
mod tests;

/// The query half of `search.index.path.admits`.
#[derive(Debug, Deserialize)]
pub struct PathAdmitsParams {
    /// Index-relative path (`@root<n>/…` for an additional root), or an
    /// absolute path inside one of the index's roots.
    pub path: String,
}

/// The reply: whether the index admits `path`, and why.
///
/// What: `admitted` is tri-state on the wire. `true`: the live policy indexes
/// the path. `false`: the policy decided it does not, a proof of exclusion.
/// `null`: the daemon did not decide (`reason` is `index_held` or
/// `undetermined`), which is neither admission nor exclusion; retry later.
/// `reason` is `admitted`; `index_held` or `undetermined`; or the refusing rule:
/// `path_escapes_root`, `outside_include_paths`, `exclude_globs`, `filters`
/// (extensions, `path_filter`, tombstone) or `walker` (built-in and extra skip
/// dirs, ignore files, docs, size caps, unsupported file types).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathAdmission {
    /// The index the answer is about.
    pub index_id: String,
    /// The path as the caller sent it.
    pub path: String,
    /// `Some(true)` admitted, `Some(false)` excluded, `None` (`null`)
    /// undecided.
    // #10739: `null`, not `false`, so an undecided answer is never read as an
    // exclusion proof (SPEC-OKF-01 §5.3.1 rule 4).
    pub admitted: Option<bool>,
    /// See the type doc.
    pub reason: String,
}

/// Is `abs` inside any of `roots`, raw or canonical?
///
/// Why: stored keys resolve against the raw root, but a pre-#9510 absolute key
/// may carry the canonical form (`/private/var` on macOS).
/// Test: `setting_include_paths_purges_newly_refused_files`.
pub(crate) fn under_any_root(roots: &[PathBuf], abs: &Path) -> bool {
    roots.iter().any(|root| {
        abs.starts_with(root)
            || root
                .canonicalize()
                .is_ok_and(|canonical| abs.starts_with(canonical))
    })
}

/// Answer `search.index.path.admits` for one path.
///
/// Why: see the module doc.
/// What: `404` for an unknown index, `400` for an empty path; otherwise the
/// verdict of `admits_pushed` (content length 0, so no size cap applies to a
/// file that is not on disk), with the reason from [`refusal_reason`].
/// `Undetermined` answers `admitted: None`.
/// Test: `path_admits_agrees_with_the_reindex_walk`,
/// `path_admits_refuses_an_unknown_index`,
/// `path_admits_never_reports_an_undecided_answer_as_excluded`.
pub(crate) fn path_admits_report(
    state: &Arc<SearchAppState>,
    id: &str,
    path: &str,
) -> Result<PathAdmission, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    let Some(handle) = state.registry.get(&index_id) else {
        return Err((
            StatusCode::NOT_FOUND,
            serde_json::json!({ "error": format!("unknown index '{}'", index_id.0) }),
        ));
    };
    if path.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "path must not be empty" }),
        ));
    }
    let verdict = crate::service::write_admission::admits_pushed(&handle, path, 0);
    let reason = match verdict {
        Admission::Included => "admitted",
        Admission::Undetermined if crate::service::exclude_hold::hold(&handle).is_some() => {
            "index_held"
        }
        Admission::Undetermined => "undetermined",
        Admission::Excluded => refusal_reason(&handle, path),
    };
    Ok(PathAdmission {
        index_id: index_id.0,
        path: path.to_string(),
        // #10739: an undecided verdict is `None`, never `Some(false)`.
        admitted: match verdict {
            Admission::Included => Some(true),
            Admission::Excluded => Some(false),
            Admission::Undetermined => None,
        },
        reason: reason.to_string(),
    })
}

/// The first rule that refuses `path`, for a path the policy excluded.
///
/// What: diagnostic only; the verdict is `admits_pushed`'s. Rules are tested
/// in the order a caller is most likely to have configured them.
/// Test: `path_admits_names_the_refusing_rule`.
fn refusal_reason(handle: &IndexHandle, path: &str) -> &'static str {
    let given = Path::new(path);
    if given
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return "path_escapes_root";
    }
    let abs = crate::core::index_roots::resolve_absolute(
        &handle.root_path,
        &handle.additional_roots,
        path,
    );
    // #10739: an unresolvable include root names no refusing rule of its own.
    let outside =
        index_admission::configured_roots(handle).is_ok_and(|roots| !under_any_root(&roots, &abs));
    if outside {
        "outside_include_paths"
    } else if index_admission::globs_exclude(
        &handle.root_path,
        &handle.additional_roots,
        &handle.exclude_globs,
        &abs,
    ) {
        "exclude_globs"
    } else if !index_admission::configured_file(handle, &abs) {
        "filters"
    } else {
        "walker"
    }
}
