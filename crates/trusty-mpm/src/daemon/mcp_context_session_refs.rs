//! The session-ref halves of `session_context_catchup` and
//! `session_context_pause`: hydrate the cache from this caller's ref, and
//! append a pause to it (#7830, ADR-0062).
//!
//! Why: both halves shell out to git and touch the network, and #9160 makes
//! each resolve the project's pinned gh identity first. Keeping them here
//! keeps `mcp_context.rs` under the production SLOC cap.
//! What: [`hydrate_session_refs`] and [`publish_session_ref`], each a blocking
//! git pass under the env
//! [`crate::core::session_ref_identity::session_ref_git_env`] resolves.
//! Test: `mcp_context_session_refs_tests.rs`, and the catch-up/pause tests in
//! `mcp_context.rs`.

use std::path::Path;

use crate::core::session_ref_identity::{
    SessionRefGitEnv, session_ref_git_env, session_ref_publish_git_env,
};
use crate::core::session_ref_privacy::RepoVisibility;
use crate::daemon::catchup_payload::HydrationReceipt;

/// Rebuild `<project>/.trusty-mpm/sessions/` from its session refs (#7830).
///
/// Why: ADR-0062 decision 4 demotes the working-tree store to a cache and makes
/// `refs/tm/sessions/**` the durable copy. Every reader below — the digest, the
/// snapshot resolver, the attribution index — reads the cache, so hydrating it
/// first is what lets the entire read path stay unchanged on a fresh clone.
/// What: [`trusty_common::catchup::session_refs::hydrate_session_cache_with_env`]
/// on a blocking thread (it shells out to git and fetches), for the ONE ref
/// [`session_ref_target`] names — this caller's own. `refs/tm/sessions/**` has
/// no server-side access control, so reading anything wider would materialize a
/// ref an attacker with push access authored (#7830 review round 2). #9160:
/// the fetch runs as the project's pinned gh identity; a pin that cannot be
/// proven makes no fetch at all. Fail-open on the catch-up: a directory that
/// is not a checkout, has no `origin`, is offline, or whose pin is refused
/// leaves the cache exactly as it found it, because a catch-up must never fail
/// on an unreachable remote. It is never silent either: the failure is a
/// `tracing::warn!` AND the returned receipt, which reaches the response's
/// `session_refs` object.
///
/// Only the PRIMARY project is hydrated, not every project an `all_projects`
/// peek walks: `resolved_snapshot` — the value a resume acts on — is resolved
/// against the primary alone, and fetching every registered project's remote
/// would put N network round-trips on a read.
/// Test: `catchup_hydrates_a_deleted_cache_from_the_session_ref`,
/// `catchup_reports_a_hydration_failure_without_failing_the_catchup`,
/// `a_pinned_env_reaches_the_catchup_fetch`,
/// `an_unproven_pin_never_falls_back_to_the_ambient_credential`.
pub(crate) async fn hydrate_session_refs(
    project_dir: &Path,
    session_id: Option<&str>,
) -> HydrationReceipt {
    hydrate_session_refs_with(project_dir, session_id, session_ref_git_env).await
}

/// [`hydrate_session_refs`] with the identity resolver given (#9160).
pub(crate) async fn hydrate_session_refs_with(
    project_dir: &Path,
    session_id: Option<&str>,
    resolve: impl FnOnce(&Path) -> SessionRefGitEnv + Send + 'static,
) -> HydrationReceipt {
    let Some(target) = session_ref_target(project_dir, session_id) else {
        let error = "no session ref can be attributed to this caller: it named no \
                     session id, or no user id resolved for this checkout"
            .to_string();
        tracing::warn!(project = %project_dir.display(), "session-ref hydration skipped: {error}");
        return HydrationReceipt {
            error: Some(error),
            ..HydrationReceipt::default()
        };
    };
    let dir = project_dir.to_path_buf();
    let hydrated = tokio::task::spawn_blocking(move || {
        // #9160: resolved on the blocking thread — proving a pin may run `gh`.
        let git_env = resolve(&dir);
        git_env.log(&dir, "fetch");
        if let Some(reason) = git_env.refusal() {
            return Err(reason.to_string());
        }
        trusty_common::catchup::session_refs::hydrate_session_cache_with_env(
            &dir,
            &target,
            git_env.vars(),
        )
        .map_err(|e| git_env.annotate(&e.to_string()))
    })
    .await;
    match hydrated {
        Ok(Ok(outcome)) => {
            tracing::debug!(
                refs_seen = outcome.refs_seen,
                own_ref_found = outcome.own_ref_found,
                snapshots = outcome.snapshots_written.len(),
                log_entries = outcome.log_entries_added,
                "session-ref hydration finished"
            );
            HydrationReceipt {
                hydrated: true,
                refs_seen: outcome.refs_seen,
                own_ref_found: outcome.own_ref_found,
                restored: outcome.snapshots_written.len(),
                error: None,
            }
        }
        Ok(Err(error)) => {
            tracing::warn!(
                project = %project_dir.display(),
                "session-ref hydration failed; the local cache was not refreshed: {error}"
            );
            HydrationReceipt {
                error: Some(error),
                ..HydrationReceipt::default()
            }
        }
        Err(e) => {
            let error = format!("session-ref hydration task failed: {e}");
            tracing::warn!("{error}");
            HydrationReceipt {
                error: Some(error),
                ..HydrationReceipt::default()
            }
        }
    }
}

/// The ONE session ref this caller may hydrate from (#7830 review round 2).
///
/// Why: `refs/tm/sessions/**` has no server-side access control — anyone with
/// push access to `origin` can create a ref under any login. The gate can
/// therefore only limit WHICH ref is read, never authenticate it, so the scope
/// is the single ref this caller's own pauses write. Building it from the write
/// side's own [`crate::core::session_ref_publish::resolve_user_id`] and
/// `session_key` is what keeps the reader and the writer on one naming rule.
/// What: `None` when the caller named no session id, or when no user id
/// resolves, or when the id cannot be a ref component — each the same condition
/// that skips the publish, so there is nothing to hydrate either.
/// Test: `catchup_hydrates_a_deleted_cache_from_the_session_ref`,
/// `catchup_reports_a_hydration_failure_without_failing_the_catchup`.
fn session_ref_target(
    project_dir: &Path,
    session_id: Option<&str>,
) -> Option<trusty_common::catchup::session_refs::SessionRefTarget> {
    use crate::core::session_ref_publish::{resolve_user_id, session_key};
    let session_id = session_id?;
    Some(trusty_common::catchup::session_refs::SessionRefTarget {
        user_id: resolve_user_id(project_dir)?,
        session_key: session_key(session_id)?,
        session_id: session_id.to_string(),
    })
}

/// Append this pause to its ADR-0062 session ref, fail-open (#7830).
///
/// Why: the ref is the DURABLE copy of session history, but the local snapshot
/// is the PRIMARY write — it is already on disk by the time this runs. A ref
/// write or a push that fails must therefore not fail the pause, and must not
/// be silent either: the receipt reaches the response and a `tracing::warn!`
/// names the project, the ref and the error.
/// What: runs [`crate::core::session_ref_publish::publish_receipt`] on a
/// blocking thread — it shells out to git and touches the network — under the
/// `enabled` its caller resolved from `[session_refs]`. #9160: the ls-remote,
/// fetch and push run as the project's pinned gh identity; a pin that cannot
/// be proven publishes nothing and makes no network call, and the receipt
/// says why. See #TBD-pause-ref: an origin that GitHub does not confirm as
/// private — public, non-GitHub, or a failed lookup — publishes nothing
/// either, and the receipt says which. A join failure is itself a receipt,
/// never a pause failure.
/// Test: `a_failed_ref_publish_is_reported_and_never_fails_the_pause`,
/// `core::session_ref_publish::tests::a_disabled_section_publishes_nothing`,
/// `a_pinned_env_reaches_the_pause_publish`,
/// `an_unproven_pin_never_publishes_with_the_ambient_credential`,
/// `an_unknown_visibility_skips_the_publish`,
/// `a_public_origin_skips_the_publish`,
/// `a_private_origin_publishes_a_redacted_ref`.
pub(crate) async fn publish_session_ref(
    project_path: &Path,
    session_id: &str,
    outcome: &trusty_common::catchup::pause::PauseSnapshotOutcome,
    enabled: bool,
) -> crate::core::session_ref_publish::SessionRefReceipt {
    publish_session_ref_with(
        project_path,
        session_id,
        outcome,
        enabled,
        session_ref_publish_git_env,
        crate::core::session_ref_privacy::origin_visibility,
    )
    .await
}

/// [`publish_session_ref`] with the identity and visibility resolvers given
/// (#9160, #TBD-pause-ref).
pub(crate) async fn publish_session_ref_with(
    project_path: &Path,
    session_id: &str,
    outcome: &trusty_common::catchup::pause::PauseSnapshotOutcome,
    enabled: bool,
    resolve: impl FnOnce(&Path) -> SessionRefGitEnv + Send + 'static,
    visibility: impl FnOnce(&Path, &[(String, String)]) -> RepoVisibility + Send + 'static,
) -> crate::core::session_ref_publish::SessionRefReceipt {
    use crate::core::session_ref_publish::{
        SessionRefReceipt, SessionRefRequest, publish_receipt, session_ref_for,
    };

    let repo = project_path.to_path_buf();
    let session = session_id.to_string();
    let snapshot = outcome.snapshot_path.clone();
    let timestamp = outcome.timestamp;
    tokio::task::spawn_blocking(move || {
        if !enabled {
            return SessionRefReceipt::default();
        }
        // #9160: resolved on the blocking thread — proving a pin may run `gh`.
        let git_env = resolve(&repo);
        git_env.log(&repo, "publish");
        if let Some(reason) = git_env.refusal() {
            return SessionRefReceipt {
                ref_name: session_ref_for(&repo, &session),
                published: false,
                error: Some(reason.to_string()),
            };
        }
        // See #TBD-pause-ref: fail closed — only a confirmed-private origin
        // receives the ref.
        if let Some(reason) = visibility(&repo, git_env.vars()).refusal() {
            tracing::info!(project = %repo.display(), "{reason}");
            return SessionRefReceipt {
                ref_name: session_ref_for(&repo, &session),
                published: false,
                error: Some(reason),
            };
        }
        let mut receipt = publish_receipt(
            &SessionRefRequest {
                repo: &repo,
                session_id: &session,
                snapshot_path: &snapshot,
                timestamp,
                git_env: git_env.vars(),
            },
            true,
        );
        receipt.error = receipt.error.map(|e| git_env.annotate(&e));
        receipt
    })
    .await
    .unwrap_or_else(|e| SessionRefReceipt {
        ref_name: None,
        published: false,
        error: Some(format!("session ref publish task failed: {e}")),
    })
}

#[cfg(test)]
#[path = "mcp_context_session_refs_tests.rs"]
mod tests;
