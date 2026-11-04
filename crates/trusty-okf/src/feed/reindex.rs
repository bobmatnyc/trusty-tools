//! The scope proof before a full reindex (SPEC-OKF-01 §5.3.1 rule 4).
//!
//! Why: a full `search.index.reindex` walks the whole index root, and the
//! walker does not skip `_sources/` or `_state/`. A reindex is safe only
//! when the binding provably excludes both trees; until D2, an allowlist
//! binding cannot prove it, so it gets per-path pushes and removes only.
//! What: [`reindex_after_rebuild`] reads `search.index.config.get` and sends
//! `search.index.reindex` only on an exclusion proof: for each tree, the
//! literal glob `<tree>/**` in `exclude_globs` or the literal `<tree>` in
//! `extra_skip_dirs`, and an `invalid_exclude_globs` present and empty. No
//! other glob is interpreted. Slice S8 of #10746.
//! Test: `c129_scope_leak_refuses_and_reindex_needs_an_exclusion_proof`.

use serde_json::{Value, json};

use super::IndexFeed;
use crate::OkfError;

/// What [`reindex_after_rebuild`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReindexDecision {
    /// The proof held and the daemon queued the reindex.
    Sent,
    /// No proof; no reindex was sent. The reason names the missing part.
    NotProven(String),
}

fn strings(v: &Value, key: &str) -> Option<Vec<String>> {
    v.get(key)?
        .as_array()?
        .iter()
        .map(|s| s.as_str().map(str::to_string))
        .collect()
}

/// Send a full reindex of `index_id` after a `rebuild`, only on a scope
/// proof (§5.3.1 rule 4, §5.6 D2).
///
/// Errors: `Search` when a call fails; nothing is sent after a failed
/// `config.get`.
pub async fn reindex_after_rebuild(
    sink: &dyn IndexFeed,
    index_id: &str,
) -> Result<ReindexDecision, OkfError> {
    let cfg = sink
        .call("search.index.config.get", json!({ "index_id": index_id }))
        .await
        .map_err(OkfError::Search)?;
    let globs = strings(&cfg, "exclude_globs").unwrap_or_default();
    let dirs = strings(&cfg, "extra_skip_dirs").unwrap_or_default();
    match strings(&cfg, "invalid_exclude_globs") {
        Some(bad) if bad.is_empty() => {}
        Some(bad) => {
            return Ok(ReindexDecision::NotProven(format!(
                "invalid_exclude_globs: {bad:?}"
            )));
        }
        None => {
            return Ok(ReindexDecision::NotProven(
                "no invalid_exclude_globs field".into(),
            ));
        }
    }
    for tree in ["_sources", "_state"] {
        let excluded =
            globs.iter().any(|g| *g == format!("{tree}/**")) || dirs.iter().any(|d| d == tree);
        if !excluded {
            return Ok(ReindexDecision::NotProven(format!(
                "{tree}/ is not excluded"
            )));
        }
    }
    sink.call("search.index.reindex", json!({ "index_id": index_id }))
        .await
        .map_err(OkfError::Search)?;
    Ok(ReindexDecision::Sent)
}
