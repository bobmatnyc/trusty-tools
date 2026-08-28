//! Replace-in-place for a drifted drawer, and the findability gate around it.
//!
//! Why (issue #5044): the #4834 migration took its import as a one-shot
//! snapshot and deleted the source files later. A file edited between the two
//! steps left the palace serving superseded text — three such drawers were
//! found by direct comparison, two of them carrying claims a later edit had
//! reversed. Detection already existed ([`super::Existing::Drifted`]); the
//! action did not, so an operator's only route was to delete the drawer by hand
//! and re-import.
//!
//! What: [`refresh_drawer`] is that action — `memory_forget` then
//! `memory_remember`, because trusty-memory has no update-in-place tool by
//! design. [`verify_findable`] is the gate: it asks `palace_verify_embedded`
//! (PR #6328) whether the drawer a file now maps to is actually retrievable,
//! and downgrades the row to [`ImportStatus::Failed`] when it is not. A
//! deletion workflow reads the run's exit code, so a fresh-but-unfindable
//! drawer must not report success.
//!
//! Both halves fail closed and say which of the two happened, because the
//! sequence has a window with no copy of the file in the palace: if the forget
//! lands and the write does not, the stale drawer is gone and nothing replaced
//! it. That arm is [`RefreshError::ReplacementLost`], and it is the one case
//! where re-running the import is mandatory rather than merely idempotent.
//!
//! Test: `refresh_replaces_a_drifted_drawer`,
//! `refresh_reports_the_lost_replacement_loudly`,
//! `refresh_aborts_with_the_stale_drawer_intact`,
//! `verify_gate_fails_a_drawer_no_vector_search_returns`.

use std::path::Path;

use serde_json::{Value, json};

use super::{FileResult, ImportOptions, ImportStatus, ParsedMemory, write_drawer};

/// How a refresh failed, and — the part that matters — what it left behind.
///
/// Why: the two arms call for opposite operator responses. An abort changed
/// nothing, so the run can simply be repeated. A lost replacement means the
/// palace no longer holds this file at all, so its source must not be deleted
/// and the import must be re-run before anything else reads the palace.
/// What: each variant names the drawer id it concerns and carries the
/// underlying RPC failure.
/// Test: `refresh_aborts_with_the_stale_drawer_intact`,
/// `refresh_reports_the_lost_replacement_loudly`.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The `memory_forget` failed; the stale drawer is still there.
    #[error(
        "refresh aborted with nothing changed — the stale drawer {drawer_id} is still in \
         place: {source:#}"
    )]
    Aborted {
        /// The drawer the refresh would have replaced.
        drawer_id: String,
        /// The `memory_forget` failure.
        source: anyhow::Error,
    },
    /// The forget landed and the write did not: the file is now unrepresented.
    #[error(
        "DATA LOSS — the stale drawer {drawer_id} was removed and its replacement could not \
         be written ({source:#}); this file is no longer in the palace, so do not delete its \
         source, and re-run the import to restore it"
    )]
    ReplacementLost {
        /// The drawer that was removed and not replaced.
        drawer_id: String,
        /// The `memory_remember` failure.
        source: anyhow::Error,
    },
}

/// Replace `drawer_id`'s contents with this file's current text.
///
/// Why: trusty-memory exposes no update-in-place tool, so "refresh" composes
/// out of the two tools that do exist. Doing it here rather than at the call
/// site keeps the ordering — forget first, so the slug tag can never name two
/// drawers — in one place.
/// What: `memory_forget` the stale drawer, then `memory_remember` the derived
/// text through the same [`write_drawer`] path a first import uses. Returns the
/// new drawer id.
///
/// # Errors
///
/// [`RefreshError::Aborted`] when the forget failed and nothing changed;
/// [`RefreshError::ReplacementLost`] when the forget landed and the write did
/// not. A forget that reports `not_found` is not an error: the drawer was
/// already gone, and writing the file back is exactly the right next step.
///
/// Test: `refresh_replaces_a_drifted_drawer`,
/// `refresh_aborts_with_the_stale_drawer_intact`,
/// `refresh_reports_the_lost_replacement_loudly`.
pub async fn refresh_drawer(
    socket: &Path,
    opts: &ImportOptions,
    parsed: &ParsedMemory,
    drawer_id: &str,
) -> Result<String, RefreshError> {
    // #5044: forget before remember, so the file's slug tag never names two
    // drawers at once — the ambiguity `existing_drawer` refuses to guess at.
    trusty_common::memory_rpc::call_memory_tool_at(
        socket,
        "memory_forget",
        json!({ "palace": opts.palace, "drawer_id": drawer_id }),
    )
    .await
    .map_err(|source| RefreshError::Aborted {
        drawer_id: drawer_id.to_string(),
        source,
    })?;

    write_drawer(socket, opts, parsed).await.map_err(|source| {
        // The one arm that leaves the palace worse than it found it.
        tracing::error!(
            drawer_id = %drawer_id,
            palace = %opts.palace,
            "refresh removed a drawer and could not write its replacement: {source:#}"
        );
        RefreshError::ReplacementLost {
            drawer_id: drawer_id.to_string(),
            source,
        }
    })
}

/// Fail `result` unless the drawer it names is retrievable right now.
///
/// Why (#5044, gate from PR #6328): the reason to refresh a drawer is that
/// something is about to delete the source file. "Written" is not the property
/// that authorises the delete — "findable" is, and the two differ: a drawer can
/// be stored, durable, and permanently absent from vector search.
/// `memory_recall` cannot answer it either, because it can hit lexically on a
/// drawer no vector search returns.
/// What: asks `palace_verify_embedded` about this row's drawer id and reads its
/// single `verified` boolean. Anything but `true` — a missing vector, an id the
/// palace does not know, an alias-audited collision, or a gate that could not
/// run at all — downgrades the row to [`ImportStatus::Failed`], which is what
/// makes the run exit non-zero. Rows with no drawer id (a dry-run create, an
/// index file) and rows already failed are left alone.
/// Test: `verify_gate_fails_a_drawer_no_vector_search_returns`,
/// `verify_gate_fails_closed_when_it_cannot_run`,
/// `refresh_replaces_a_drifted_drawer`.
pub async fn verify_findable(socket: &Path, palace: &str, result: &mut FileResult) {
    let Some(drawer_id) = result.drawer_id.clone() else {
        return;
    };
    if result.status == ImportStatus::Failed {
        return;
    }
    let stored = stored_phrase(result.status);
    match verdict(socket, palace, &drawer_id).await {
        Ok(None) => {}
        Ok(Some(reason)) => fail(result, format!("{stored}, but is not findable: {reason}")),
        // #5044: a gate that could not run is never a pass — nothing is known
        // about this drawer, which is a block for a deletion workflow.
        Err(e) => fail(
            result,
            format!("{stored}, but the findability gate could not run: {e:#}"),
        ),
    }
}

/// What the row had achieved before the gate downgraded it to `Failed`.
///
/// Why: the row's own status is about to be overwritten, and "failed" alone
/// would read as "nothing was written" — a drawer this run created and could
/// not verify is still in the palace, and the operator has to know that.
fn stored_phrase(status: ImportStatus) -> &'static str {
    match status {
        ImportStatus::Created => "the drawer was written",
        ImportStatus::Refreshed => "the drifted drawer was replaced",
        ImportStatus::Skipped => "the drawer was already in the palace",
        _ => "the drawer is in the palace",
    }
}

/// `Ok(None)` when the drawer is findable, `Ok(Some(reason))` when it is not.
async fn verdict(socket: &Path, palace: &str, drawer_id: &str) -> anyhow::Result<Option<String>> {
    let answer = trusty_common::memory_rpc::call_memory_tool_at(
        socket,
        "palace_verify_embedded",
        json!({ "palace": palace, "drawer_ids": [drawer_id] }),
    )
    .await?;
    if answer.get("verified").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    Ok(Some(unverified_reason(&answer)))
}

/// Turn a `verified: false` answer into the phrase an operator can act on.
fn unverified_reason(answer: &Value) -> String {
    let has = |key: &str| {
        answer
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|ids| !ids.is_empty())
    };
    if has("unknown") {
        return "the palace holds no drawer with this id".to_string();
    }
    if has("missing") {
        return "the drawer has no vector, so no vector search will return it — \
                run palace_reembed"
            .to_string();
    }
    match answer.get("alias_audit").and_then(Value::as_str) {
        Some("clean") | None => {
            "palace_verify_embedded declined to verify it and named no cause".to_string()
        }
        Some(state) => format!(
            "the palace's vector-key audit is {state}{}",
            answer
                .get("alias_audit_error")
                .and_then(Value::as_str)
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        ),
    }
}

/// Downgrade a row to `Failed`, keeping any detail it already carried.
fn fail(result: &mut FileResult, reason: String) {
    result.status = ImportStatus::Failed;
    result.detail = Some(match result.detail.take() {
        Some(prior) => format!("{prior}; {reason}"),
        None => reason,
    });
}
