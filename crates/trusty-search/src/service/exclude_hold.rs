//! Hold an index whose exclude globs do not parse (#9059).
//!
//! Why: entry points reject an unparsable exclude glob (#8922), but one
//! restored from `indexes.toml` — a legacy entry or a hand edit — reached the
//! matcher, which skips it. The index then admitted the paths the glob was
//! written to exclude, often secrets, and served their plaintext.
//! What: [`hold`] names the invalid patterns of a live handle. Every ingest
//! path asks it before writing: `index_admission::admits` answers
//! `Undetermined`, the rescan, the boot delta and the reindex claim refuse,
//! and a pushed write answers 409. Reads are untouched. The hold is derived
//! from the handle the registry holds, so the PATCH that replaces it with
//! valid globs releases it without a restart and starts a catch-up reindex.
//! Test: `crate::service::exclude_hold_9059_tests`.

use axum::http::StatusCode;

use crate::core::registry::IndexHandle;
use crate::service::reindex::{try_claim_reindex, ReindexClaim, ReindexClaimError};

/// Why an index is held: its id and the exclude globs that do not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExcludeHold {
    /// The held index.
    pub(crate) index_id: String,
    /// Every `exclude_globs` entry that does not parse, in order.
    pub(crate) patterns: Vec<String>,
}

/// The hold on `handle`, or `None` when every exclude glob parses.
///
/// Why: an invalid glob cannot exclude anything, so admitting any path would
/// admit what it names; nothing can be admitted until it is fixed.
/// What: `Some` when [`crate::core::repo_config::invalid_exclude_globs`] finds
/// at least one pattern. Each call compiles every exclude glob once, held or
/// not, so callers on a per-file path pay one glob compile per pattern.
/// Test: `every_ingest_path_refuses_a_held_index`.
pub(crate) fn hold(handle: &IndexHandle) -> Option<ExcludeHold> {
    let patterns = crate::core::repo_config::invalid_exclude_globs(&handle.exclude_globs);
    (!patterns.is_empty()).then(|| ExcludeHold {
        index_id: handle.id.0.clone(),
        patterns,
    })
}

impl ExcludeHold {
    /// The operator-facing reason, naming every invalid glob and the fix.
    pub(crate) fn reason(&self) -> String {
        format!(
            "index '{id}' is held: exclude glob(s) {patterns:?} do not parse, so the paths they \
             name cannot be excluded. Nothing is indexed until PATCH /indexes/{id}/config sets \
             valid exclude_globs; that PATCH then starts a catch-up reindex for the changes \
             refused while held. Search keeps serving what is already indexed (#9059)",
            id = self.index_id,
            patterns = self.patterns,
        )
    }

    /// The 409 a refused write answers on every transport.
    ///
    /// What: `error: "index_held"`, `reason: "invalid_exclude_glob"`, the
    /// patterns under `invalid_exclude_globs`, and `retryable: false` — only
    /// a config fix clears it. Callers add their own fields.
    pub(crate) fn refusal(&self) -> (StatusCode, serde_json::Value) {
        tracing::warn!(index_id = %self.index_id, patterns = ?self.patterns, "write refused: index is held (#9059)");
        (
            StatusCode::CONFLICT,
            serde_json::json!({
                "error": "index_held",
                "index_id": self.index_id,
                "reason": "invalid_exclude_glob",
                "invalid_exclude_globs": self.patterns,
                "message": self.reason(),
                "retryable": false,
            }),
        )
    }
}

/// Claim `handle` for a reindex, refusing a serve-only or held index.
///
/// Why: every reindex entry point claims first (#8889), so refusing here keeps
/// a serve-only (#8883) or held index from ever reaching the walk, the prune
/// or a stage reset.
/// What: [`ReindexClaimError::ServeOnly`] for a serve-only index, then
/// [`ReindexClaimError::Held`] naming the glob, else [`try_claim_reindex`].
/// Test: `every_ingest_path_refuses_a_held_index` (`reindex-http`, `reindex-spawn`),
/// `an_internal_reindex_spawn_refuses_a_serve_only_index`.
pub(crate) fn claim_reindex(
    handle: &IndexHandle,
    origin: &'static str,
    force: bool,
) -> Result<ReindexClaim, ReindexClaimError> {
    // #8883: refused before the claim slot is touched, so nothing is queued.
    if handle.serve_only {
        tracing::warn!(index_id = %handle.id, origin, "reindex refused: index is serve-only (#8883)");
        return Err(ReindexClaimError::ServeOnly {
            index_id: handle.id.0.clone(),
            message: crate::service::serve_only::reason(&handle.id.0),
        });
    }
    if let Some(hold) = hold(handle) {
        return Err(ReindexClaimError::Held {
            message: hold.reason(),
            index_id: hold.index_id,
            invalid_exclude_globs: hold.patterns,
        });
    }
    try_claim_reindex(&handle.id, origin, force)
}
