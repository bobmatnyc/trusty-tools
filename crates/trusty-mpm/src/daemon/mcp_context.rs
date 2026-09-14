//! Daemon-side implementation of the two PM pause/resume context MCP tools:
//! `session_context_catchup` and `session_context_pause`.
//!
//! Why: `/tm-session-resume` and `/tm-session-pause` used to shell out to
//! `tm session catchup` + `git log`/`git status` + hand-written snapshot files
//! so the PM could rebuild or save context across a pause. Those tools live in
//! their own file — distinct from `mcp_session.rs` (managed sub-session
//! lifecycle: `session_new`/`session_stop`/…) — because this is a DIFFERENT
//! concept: operations on the CALLING PM session's own project-local snapshot
//! state, not a spawned sub-session. Kept separate to stay under the 500-SLOC
//! production cap given `mcp_session.rs` is already large.
//! What: [`session_context_catchup`] wraps
//! [`trusty_common::catchup::generate_catchup_json`] (never advancing the
//! watermark — a manual peek, same contract as `tm session catchup`) plus
//! [`crate::core::catchup::resolve::resolve_snapshot_for_caller`] for
//! `resolved_snapshot` + `resolved_via`. [`session_context_pause`] wraps
//! [`trusty_common::catchup::pause::write_pause_snapshot`] and, unless
//! `prune_worktrees` is `false`, the SAME [`crate::session_manager::SessionManager::prune_orphaned_worktrees`]
//! engine `tm session prune-worktrees` / the HTTP `prune-worktrees` route use —
//! called in-process rather than looped back over HTTP.
//! Test: `cargo test -p trusty-mpm daemon::mcp_context` plus the dispatch-level
//! mock tests in `crate::mcp::tests`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::core::catchup::resolve::{
    CallerIdentity, ResolvedSnapshot, redact_sessions_not_owned_by, resolve_snapshot_for_caller,
};
use crate::core::catchup::{CatchupOptions, generate_catchup_json};
use crate::daemon::catchup_bounds::{CATCHUP_BUDGET_BYTES, bound_catchup};
use crate::daemon::state::DaemonState;

/// The session id to use for a caller that named none.
///
/// Why: #6888 — a PM-invented `session_id` never matched itself twice, so the
/// pause and the next catch-up keyed different strings and the owner was told
/// no snapshot resolved. Both tools call this, so writer and reader agree by
/// construction. The DERIVATION itself lives in
/// [`trusty_common::catchup::session_id`]; this wrapper exists only to pin what
/// the daemon may feed it.
/// What: passes `None` for the managed session id and derives from the caller's
/// tmux window alone. The daemon is a long-lived process behind the
/// `trusty-mpm serve --stdio` bridge, so its OWN `TM_MANAGED_SESSION_ID` — if a
/// managed pane happened to auto-start it — names some unrelated session and
/// would misattribute every caller's pause to it. The bridge, which really does
/// run inside the calling pane, stamps that id into the arguments instead
/// (`commands::serve_stdio`), so a managed caller arrives here with an explicit
/// `session_id` already set.
/// Test: `catchup_derives_a_missing_session_id_from_the_callers_window`,
/// `pause_derives_a_missing_session_id_from_the_callers_window`,
/// `pause_without_an_identifiable_caller_errors`.
fn derive_caller_session_id(tmux_window: Option<&str>) -> Option<String> {
    trusty_common::catchup::session_id::derive_session_id(None, tmux_window)
}

/// Shape the merged digest into the `session_context_catchup` response body.
///
/// Why: `undatable_sessions_dropped` is a receipt — an empty `sessions` array
/// means "nothing paused" only when it is 0 — and nothing else pins that it
/// reaches the wire. No end-to-end test can drive it non-zero, because an
/// undatable session is unreachable through the filesystem once both
/// `PausedSession` arms fall back to mtime, so substituting a literal `0` here
/// would leave the suite green while the receipt stopped working (#5072). A
/// pure function is the seam that makes the field assertable.
///
/// #5557: the same argument now covers SIZE. The digest arrays used to go on
/// the wire whole, so the body grew with the project's snapshot history until
/// the harness could no longer deliver it. They are paged through
/// [`bound_catchup`] here, and the page's own receipt —
/// `truncated` / `truncation_notice` / `sessions_total` / `sessions_next_offset`
/// — travels with it, because a capped response that reads exactly like a
/// complete one recreates the silent-loss defect the withheld count exists to
/// prevent.
/// What: the seven original response keys, unchanged in meaning, plus six
/// additive paging keys. `resolved_via` names which lookup produced
/// `resolved_snapshot` (`session_id`, `tmux_window`, or `null` alongside a null
/// snapshot), so a caller can tell an exact match from the window fallback
/// instead of reading both as ownership. `watermark_advanced` is always `false`
/// by construction — no path in this module calls `save_catchup_state`.
/// Test: `catchup_payload_carries_the_undatable_drop_count`,
/// `session_context_catchup_returns_expected_shape`,
/// `catchup_payload_bounds_an_oversized_store`,
/// `catchup_payload_announces_what_it_withheld`.
fn catchup_payload(
    merged: trusty_common::catchup::CatchupJson,
    sessions_offset: usize,
    resolved: Option<ResolvedSnapshot>,
    session_refs: HydrationReceipt,
) -> Value {
    let (snapshot, via) = match resolved {
        Some(r) => (Some(r.path.display().to_string()), Some(r.via.as_str())),
        None => (None, None),
    };
    let undatable_sessions_dropped = merged.undatable_sessions_dropped;
    // #5557: page the digest so the body cannot outgrow what a caller can read.
    let page = bound_catchup(merged, sessions_offset, CATCHUP_BUDGET_BYTES);
    json!({
        "sessions": page.sessions,
        "sessions_total": page.sessions_total,
        "sessions_offset": page.sessions_offset,
        "sessions_next_offset": page.next_offset(),
        "recent_commits": page.recent_commits,
        "recent_commits_total": page.recent_commits_total,
        "recent_memory": page.recent_memory,
        "recent_memory_total": page.recent_memory_total,
        "truncated": page.truncated(),
        "over_budget": page.over_budget(),
        "page_bytes": page.page_bytes,
        "truncation_notice": page.truncation_notice(),
        "resolved_snapshot": snapshot,
        "resolved_via": via,
        "undatable_sessions_dropped": undatable_sessions_dropped,
        "watermark_advanced": false,
        // #7830 review: hydration used to fail into `tracing::debug!`, below
        // the default filter, so a resume that silently read an empty cache
        // looked identical to one with nothing to restore.
        "session_refs": {
            "hydrated": session_refs.hydrated,
            "refs_seen": session_refs.refs_seen,
            "own_ref_found": session_refs.own_ref_found,
            "restored": session_refs.restored,
            "error": session_refs.error,
        },
    })
}

/// What one session-ref hydration pass produced, as the catch-up reports it.
///
/// Why (#7830 review): the resume digest is only as complete as the cache it
/// was built from. When hydration cannot run — no `origin`, no user id, an
/// unreachable remote — the caller must be able to see that the cache was NOT
/// refreshed, rather than infer "nothing paused" from an empty digest.
/// What: whether the pass completed, how many refs the aggregator saw, whether
/// the caller's OWN ref was among them, how many snapshots were restored, and
/// the failure text otherwise. `Default` is the "disabled" state.
///
/// `refs_seen` alone cannot be read as success (#7830 review round 2): after a
/// hostname change or a `gh` account switch every old ref is unreachable
/// forever, so `hydrated: true, refs_seen: 5, own_ref_found: false,
/// restored: 0` is a real and permanent state that used to look identical to
/// "nothing to do". The field is `own_ref_found` rather than `owned` because
/// `sessions[].owned` in the same response is a different, per-session concept
/// (#7830 review round 3).
/// Test: `catchup_reports_a_hydration_failure_without_failing_the_catchup`,
/// `catchup_hydrates_a_deleted_cache_from_the_session_ref`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HydrationReceipt {
    /// Whether the hydration pass ran to completion.
    hydrated: bool,
    /// How many `refs/tm/sessions/**` refs the aggregator enumerated.
    refs_seen: usize,
    /// Whether the caller's own ref was among them.
    own_ref_found: bool,
    /// How many snapshot files this pass wrote back.
    restored: usize,
    /// Why the pass did not complete, when it did not.
    error: Option<String>,
}

/// Back the `session_context_catchup` MCP tool.
///
/// Why: gives the PM a typed, JSON-native resume digest instead of scraping
/// `tm session catchup`'s rendered markdown + separately shelling out to `git
/// log`/`git status` and reading snapshot files by hand.
/// What: validates `project_dir` exists; when `all_projects` is set, also
/// scans every project in the legacy claude-mpm registry (same discovery `tm
/// session catchup --all-projects` uses), merging every project's
/// sessions/commits/memory into flat arrays. Builds [`CatchupOptions`] from
/// the persisted `MpmConfig` catchup section (git/palace limits + toggles) and
/// calls [`generate_catchup_json`] per project — which NEVER persists a
/// watermark, so `watermark_advanced` in the result is unconditionally
/// `false`. `resolved_snapshot` is resolved against the primary `project_dir`
/// only (not every scanned project) and strictly FOR `session_id`; it answers
/// "what should I resume from", while `sessions` answers "what paused since
/// your last catch-up", so the two legitimately disagree under a recent
/// watermark. `undatable_sessions_dropped` sums each project's withheld count
/// so an empty `sessions` array can be told apart from sessions that exist but
/// could not be dated (#5072).
///
/// #5272: with no `session_id` — or with one that owns no snapshot —
/// `resolved_snapshot` is `null`. It is never another session's file, which is
/// what the shared-store PM model turned the old "newest pause overall"
/// fallback into. `tmux_window` adds a second, narrower route on top of that
/// rule rather than reopening it: see
/// [`resolve_snapshot_for_caller`](crate::core::catchup::resolve::resolve_snapshot_for_caller).
///
/// #5386: `sessions[]` is gated on the same ownership test. It used to return
/// every paused session's `source_file` and `tmux_window` to any caller, which
/// let a caller read another session's window out of this response, pass it
/// back as its own, and resolve that session's snapshot by hand — #5272's
/// outcome reconstructed from the data #5272 left in place. A non-owning
/// caller now sees `format`, `paused_at`, `summary` and `owned: false`; the
/// handles and the state a resume would restore are withheld. See
/// [`redact_sessions_not_owned_by`](crate::core::catchup::resolve::redact_sessions_not_owned_by).
/// The CLI `tm session catchup` digest is unchanged — it renders one operator's
/// own terminal, not a response to a remote caller.
///
/// #5557: `sessions` is a PAGE, not the whole list. A live `full: true` call on
/// this repo returned 112k characters, past what the harness could hand back to
/// the calling model — so it spilled the body to a file and the session
/// resuming from it had to read that instead. `sessions_offset` selects the
/// page; `sessions_next_offset` names the one that follows, so `full` still
/// delivers every snapshot in history, one readable page at a time, rather than
/// one unreadable response. The CLI digest is again unchanged.
/// Test: `session_context_catchup_missing_project_dir_errors`,
/// `session_context_catchup_returns_expected_shape`,
/// `session_context_catchup_never_resolves_another_sessions_snapshot`,
/// `session_context_catchup_resolves_by_tmux_window_after_a_relaunch`,
/// `session_context_catchup_withholds_a_non_owners_handles`,
/// `session_context_catchup_digest_agrees_with_the_window_fallback`,
/// `catchup_derives_a_missing_session_id_from_the_callers_window`.
pub async fn session_context_catchup(
    project_dir: &str,
    session_id: Option<&str>,
    tmux_window: Option<&str>,
    all_projects: bool,
    full: bool,
    sessions_offset: usize,
) -> Result<Value, String> {
    let primary = PathBuf::from(project_dir);
    if !primary.is_dir() {
        return Err(format!(
            "project_dir does not exist or is not a directory: {project_dir}"
        ));
    }

    // #6888: the reader derives the same id the pause writer derived, so the
    // exact-id route survives a restart that mints a new harness session id.
    let derived = derive_caller_session_id(tmux_window);
    let session_id = session_id.or(derived.as_deref());

    let mut project_dirs = vec![primary.clone()];
    if all_projects {
        let registry = crate::core::claude_mpm_registry::default_registry_path();
        match crate::core::claude_mpm_registry::discover_claude_mpm_projects(&registry) {
            Ok(extra) => {
                for p in extra {
                    if !project_dirs.contains(&p) {
                        project_dirs.push(p);
                    }
                }
            }
            Err(e) => {
                // Fail-open: log and continue with just the primary project.
                eprintln!(
                    "session_context_catchup: warning: could not read claude-mpm registry: {e}"
                );
            }
        }
    }

    let config = crate::core::config::MpmConfig::load_default();
    // #7830 / ADR-0062: rebuild the local cache from `refs/tm/sessions/**`
    // BEFORE anything reads it, so a fresh clone resolves its snapshot through
    // the unchanged read path.
    let session_refs = if config.session_refs.enabled {
        hydrate_session_refs(&primary, session_id).await
    } else {
        HydrationReceipt::default()
    };
    let memory_socket = trusty_common::memory_rpc::resolve_memory_socket_or_unreachable();

    // #5072: `absorb` sums `undatable_sessions_dropped` across projects rather
    // than concatenating it — an empty `sessions` array is only "nothing
    // paused" when that total is 0.
    let mut merged = trusty_common::catchup::CatchupJson::default();
    let caller = CallerIdentity::new(session_id, tmux_window);

    for dir in &project_dirs {
        let opts = CatchupOptions {
            project_dir: dir.clone(),
            memory_socket: memory_socket.clone(),
            include_git: config.catchup.include_git,
            include_palace: config.catchup.include_palace,
            git_limit: config.catchup.git_limit,
            drawer_limit: config.catchup.drawer_limit,
            full,
        };
        // Manual catch-up NEVER advances the watermark — only automatic
        // session-start injection does (core/session_launch/mod.rs).
        let mut digest = generate_catchup_json(&opts).await;
        // #5386: redact per project, before merging — session-id attribution is
        // read from the store of the project that owns the snapshot, so a merged
        // list would check every session against the primary project's log only.
        redact_sessions_not_owned_by(dir, &caller, &mut digest.sessions);
        merged.absorb(digest);
    }

    // PR #5386: the exact-id lookup misses across a Claude Code relaunch, which
    // mints a new harness session id inside the same tmux window.
    let resolved = resolve_snapshot_for_caller(&primary, session_id, tmux_window);

    Ok(catchup_payload(
        merged,
        sessions_offset,
        resolved,
        session_refs,
    ))
}

/// Rebuild `<project>/.trusty-mpm/sessions/` from its session refs (#7830).
///
/// Why: ADR-0062 decision 4 demotes the working-tree store to a cache and makes
/// `refs/tm/sessions/**` the durable copy. Every reader below — the digest, the
/// snapshot resolver, the attribution index — reads the cache, so hydrating it
/// first is what lets the entire read path stay unchanged on a fresh clone.
/// What: [`trusty_common::catchup::session_refs::hydrate_session_cache`] on a
/// blocking thread (it shells out to git and fetches), for the ONE ref
/// [`session_ref_target`] names — this caller's own. `refs/tm/sessions/**` has
/// no server-side access control, so reading anything wider would materialize a
/// ref an attacker with push access authored (#7830 review round 2). Fail-open:
/// a directory that is not a checkout, has no `origin`, or is offline leaves the
/// cache exactly as it found it, because a catch-up must never fail on an
/// unreachable remote. It is never silent either: the failure is a
/// `tracing::warn!` AND the returned receipt, which reaches the response's
/// `session_refs` object.
///
/// Only the PRIMARY project is hydrated, not every project an `all_projects`
/// peek walks: `resolved_snapshot` — the value a resume acts on — is resolved
/// against the primary alone, and fetching every registered project's remote
/// would put N network round-trips on a read.
/// Test: `catchup_hydrates_a_deleted_cache_from_the_session_ref`,
/// `catchup_reports_a_hydration_failure_without_failing_the_catchup`.
async fn hydrate_session_refs(project_dir: &Path, session_id: Option<&str>) -> HydrationReceipt {
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
        trusty_common::catchup::session_refs::hydrate_session_cache(&dir, &target)
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
        Ok(Err(e)) => {
            let error = e.to_string();
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

/// Back the `session_context_pause` MCP tool.
///
/// Why: replaces the bash snapshot-write in `/tm-session-pause` with an
/// in-process writer that emits the exact section shape the catch-up reader
/// already parses, plus the same in-process worktree-prune engine the HTTP
/// `prune-worktrees` route uses (never a self-loopback HTTP call).
/// What: writes the snapshot + appends the pause log line via
/// [`trusty_common::catchup::pause::write_pause_snapshot`]; when
/// `prune_worktrees` is true (the default), lists active managed-session
/// workspace paths and calls
/// [`crate::session_manager::SessionManager::prune_orphaned_worktrees`] with
/// `dry_run: false` — mirroring
/// [`crate::daemon::managed_routes::prune::prune_worktrees_route`] exactly,
/// just called directly instead of over HTTP. A worktree-prune failure is
/// logged and reported as an empty list rather than failing the whole pause
/// (the snapshot write is the operation that must not silently fail).
///
/// #4091: the prune leg always passes
/// [`crate::session_manager::DirtyWorktreePolicy::Skip`] — a worktree holding
/// uncommitted or unpushed work is never removed by a pause, and every such
/// skip is returned in the `skipped_dirty_worktrees` field (path + reason +
/// file/commit counts) so the `/tm-session-pause` skill can surface it to the
/// operator instead of it being a log line nobody reads. There is
/// deliberately no argument through which the MCP tool could request the
/// force-discard policy.
/// #6888: `session_id` is OPTIONAL. It used to be a required free-text string
/// the PM invented per pause, and because the resume lookup is exact string
/// equality, the next guess never matched the last one. Omitting it now derives
/// the id from the caller's own identity via [`derive_caller_session_id`], which
/// is the same derivation `session_context_catchup` runs — so the two agree
/// without the PM having to retype anything. An explicitly passed id is honored
/// unchanged. A caller that is neither managed nor inside tmux derives nothing
/// and gets an error naming what to pass, rather than a snapshot filed under an
/// id nothing will ever look up again.
/// Test: `session_context_pause_missing_project_dir_errors`,
/// `session_context_pause_requires_summary`,
/// `session_context_pause_writes_snapshot_without_pruning`,
/// `pause_derives_a_missing_session_id_from_the_callers_window`,
/// `pause_without_an_identifiable_caller_errors`.
#[allow(clippy::too_many_arguments)]
pub async fn session_context_pause(
    state: &Arc<DaemonState>,
    project_dir: &str,
    session_id: Option<&str>,
    summary: &str,
    completed: Vec<String>,
    in_progress: Vec<String>,
    next_steps: Vec<String>,
    tmux_window: Option<&str>,
    prune_worktrees: bool,
) -> Result<Value, String> {
    // #7830 review: the flag is READ here and PASSED down, so a test can pin it
    // instead of inheriting the operator's `~/.trusty-mpm/config.toml`.
    let session_refs_enabled = crate::core::config::MpmConfig::load_default()
        .session_refs
        .enabled;
    session_context_pause_with(
        state,
        project_dir,
        session_id,
        summary,
        completed,
        in_progress,
        next_steps,
        tmux_window,
        prune_worktrees,
        session_refs_enabled,
    )
    .await
}

/// [`session_context_pause`] with `[session_refs].enabled` supplied explicitly.
///
/// Why (#7830 review): the public entry point reads the flag from the
/// operator's real `~/.trusty-mpm/config.toml`, so a test asserting on the ref
/// receipt passed or failed with the host's configuration. This seam is the
/// same shape [`crate::core::session_ref_publish::publish_session_ref_as`] uses
/// for the user id, and for the same reason.
/// What: identical to [`session_context_pause`]; `session_refs_enabled` is
/// forwarded to [`publish_session_ref`] verbatim.
/// Test: `a_failed_ref_publish_is_reported_and_never_fails_the_pause`,
/// `session_context_pause_writes_snapshot_without_pruning`.
#[allow(clippy::too_many_arguments)]
async fn session_context_pause_with(
    state: &Arc<DaemonState>,
    project_dir: &str,
    session_id: Option<&str>,
    summary: &str,
    completed: Vec<String>,
    in_progress: Vec<String>,
    next_steps: Vec<String>,
    tmux_window: Option<&str>,
    prune_worktrees: bool,
    session_refs_enabled: bool,
) -> Result<Value, String> {
    let project_path = PathBuf::from(project_dir);
    if !project_path.is_dir() {
        return Err(format!(
            "project_dir does not exist or is not a directory: {project_dir}"
        ));
    }
    if summary.trim().is_empty() {
        return Err("`summary` must not be empty".to_string());
    }

    // #6888: derive rather than let the caller invent an id the reader can't match.
    let derived = derive_caller_session_id(tmux_window);
    let Some(session_id) = session_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(derived.as_deref())
    else {
        return Err(
            "`session_id` was omitted and could not be derived: this caller is neither a \
             tm-managed session nor running inside tmux. Pass `session_id` explicitly."
                .to_string(),
        );
    };

    let input = trusty_common::catchup::pause::PauseSnapshotInput {
        session_id,
        summary,
        completed: &completed,
        in_progress: &in_progress,
        next_steps: &next_steps,
        tmux_window,
    };
    let outcome = trusty_common::catchup::pause::write_pause_snapshot(&project_path, &input)
        .map_err(|e| format!("failed to write pause snapshot: {e}"))?;

    // #7830 / ADR-0062: the snapshot's durable copy is one commit appended to
    // this session's own ref. Fail-open by construction — see the helper.
    let receipt =
        publish_session_ref(&project_path, session_id, &outcome, session_refs_enabled).await;

    let mut skipped_dirty = Vec::new();
    let pruned_worktrees: Vec<String> = if prune_worktrees {
        let mgr = state.session_manager().await;
        let records = mgr.list().await;
        let active_workspace_paths: Vec<PathBuf> = records
            .iter()
            .filter_map(|r| r.workspace_path.clone())
            .collect();
        let tt_config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
        let repos_root = crate::core::trusty_tools_config::workspace_root(&tt_config);
        // #4091: the pause path ALWAYS uses the default skip-dirty policy —
        // there is deliberately no argument threaded from the MCP tool that
        // could turn this into a force-discard, so an ordinary
        // `/tm-session-pause` can never destroy uncommitted work.
        match mgr
            .prune_orphaned_worktrees(
                &repos_root,
                &active_workspace_paths,
                false,
                crate::session_manager::DirtyWorktreePolicy::Skip,
                // #7357: resolved here, at the MCP entry point.
                &crate::project::adopted_anchors_under(state.framework_root()),
            )
            .await
        {
            Ok(sweep) => {
                skipped_dirty = sweep.skipped_dirty;
                sweep
                    .removed
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            }
            Err(e) => {
                tracing::warn!("session_context_pause: worktree prune failed: {e}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // #7282: the snapshot reaches `origin/main` through its own branch and PR.
    // A failure here is an ERROR, never a warning — the previous behaviour
    // (commit onto whatever branch the checkout was on) failed silently for
    // weeks, and the snapshot file is already on disk either way.
    // #7464: the `ws/<session>` label carries the session's NAME, so resolve it
    // from the managed record before the publish spends it on `gh pr create`.
    let session_name = managed_session_name(state, session_id).await;
    let publish =
        publish_snapshot(&project_path, session_id, session_name.as_deref(), &outcome).await;

    let publish_json = match publish {
        Ok(SnapshotPublish::Opened(out)) => json!({
            "status": "opened",
            "branch": out.branch,
            "commit": out.commit,
            "pr_url": out.pr_url,
            "auto_merge_armed": out.auto_merge_armed,
            // #7282 review round 3: an unarmed PR is not a failed pause, but the
            // reason must reach the caller — otherwise the PR simply never
            // merges and nothing anywhere says why.
            "auto_merge_error": out.auto_merge_error,
        }),
        // #7282 review: a skip carries WHY. `not_tracked` (this project keeps
        // its sessions git-ignored) and `not_a_git_repo` are benign, but a bare
        // `skipped` made them indistinguishable from `unchanged` — and from
        // each other — for anyone reading the pause result.
        Ok(SnapshotPublish::Skipped(reason)) => json!({
            "status": "skipped",
            "reason": reason,
        }),
        Err(e) => {
            return Err(format!(
                "pause snapshot written to {}, but publishing it failed: {e} \
                 (worktree prune: {} removed, {} skipped as dirty)",
                outcome.snapshot_path.display(),
                pruned_worktrees.len(),
                skipped_dirty.len()
            ));
        }
    };

    Ok(json!({
        // #6888: report the id the snapshot was filed under, derived or not.
        "session_id": session_id,
        "snapshot_path": outcome.snapshot_path.display().to_string(),
        "timestamp": outcome.timestamp.to_rfc3339(),
        "pruned_worktrees": pruned_worktrees,
        "skipped_dirty_worktrees": skipped_dirty,
        "snapshot_publish": publish_json,
        // #7830 / ADR-0062: the durable copy of this pause, as a receipt.
        "ref_name": receipt.ref_name,
        "ref_published": receipt.published,
        "ref_error": receipt.error,
    }))
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
/// `enabled` its caller resolved from `[session_refs]`. A join failure is
/// itself a receipt, never a pause failure.
/// Test: `a_failed_ref_publish_is_reported_and_never_fails_the_pause`,
/// `core::session_ref_publish::tests::a_disabled_section_publishes_nothing`.
async fn publish_session_ref(
    project_path: &Path,
    session_id: &str,
    outcome: &trusty_common::catchup::pause::PauseSnapshotOutcome,
    enabled: bool,
) -> crate::core::session_ref_publish::SessionRefReceipt {
    use crate::core::session_ref_publish::{SessionRefReceipt, SessionRefRequest, publish_receipt};

    let repo = project_path.to_path_buf();
    let session = session_id.to_string();
    let snapshot = outcome.snapshot_path.clone();
    let timestamp = outcome.timestamp;
    tokio::task::spawn_blocking(move || {
        publish_receipt(
            &SessionRefRequest {
                repo: &repo,
                session_id: &session,
                snapshot_path: &snapshot,
                timestamp,
            },
            enabled,
        )
    })
    .await
    .unwrap_or_else(|e| SessionRefReceipt {
        ref_name: None,
        published: false,
        error: Some(format!("session ref publish task failed: {e}")),
    })
}

/// Publish a freshly written pause snapshot as a branch + PR (#7282).
///
/// Why: `session_context_pause` is the one path every pause goes through, so
/// wiring the publish here is what makes "no snapshot commits onto the main
/// checkout's current branch" true by construction rather than by convention.
/// What: resolves the two repo-relative paths a pause touches (the snapshot and
/// the append-only log), then runs
/// [`crate::core::session_pause_pr::publish_pause_snapshot`] on a blocking
/// thread, with the project's configured default branch from
/// [`configured_default_branch`]. A skip carries its reason so the pause result
/// distinguishes "this project git-ignores its sessions" from "this directory
/// is not a checkout" from "nothing changed"; every other failure is an error
/// the caller must surface.
/// Test: `crate::core::session_pause_pr` covers the sequence against a fake
/// driver; `publish_snapshot_reports_not_a_git_repo_as_the_skip_reason` covers
/// this wiring.
async fn publish_snapshot(
    project_path: &Path,
    session_id: &str,
    session_name: Option<&str>,
    outcome: &trusty_common::catchup::pause::PauseSnapshotOutcome,
) -> Result<SnapshotPublish, String> {
    use crate::core::session_pause_pr as pause_pr;

    let Ok(rel) = outcome.snapshot_path.strip_prefix(project_path) else {
        return Err(format!(
            "snapshot {} is not inside {}",
            outcome.snapshot_path.display(),
            project_path.display()
        ));
    };
    let paths = vec![
        rel.to_string_lossy().replace('\\', "/"),
        format!("{}sessions-log.jsonl", pause_pr::SESSIONS_PREFIX),
    ];

    let repo = project_path.to_path_buf();
    let session = session_id.to_string();
    // #7464: carried into the blocking task so the publish can name the `ws/`
    // label after the session rather than after its UUID.
    let name = session_name.map(str::to_string);
    let timestamp = outcome.timestamp;
    let default_branch = configured_default_branch(project_path);
    tokio::task::spawn_blocking(move || {
        let req = pause_pr::PublishRequest {
            repo: &repo,
            session_id: &session,
            session_name: name.as_deref(),
            timestamp,
            paths,
            default_branch: default_branch.as_deref(),
        };
        match pause_pr::publish_pause_snapshot(&pause_pr::RealPauseVcs, &req) {
            Ok(Some(out)) => Ok(SnapshotPublish::Opened(out)),
            Ok(None) => Ok(SnapshotPublish::Skipped("unchanged")),
            Err(pause_pr::PublishError::NotTracked(_)) => {
                Ok(SnapshotPublish::Skipped("not_tracked"))
            }
            Err(pause_pr::PublishError::NotAGitRepo(_)) => {
                Ok(SnapshotPublish::Skipped("not_a_git_repo"))
            }
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| format!("snapshot publish task failed: {e}"))?
}

/// The NAME of the managed session `session_id` identifies, when one is on
/// record.
///
/// Why (#7464): the `ws/<session>` label the snapshot PR is opened with is
/// derived from the session's name — that is the label session launch and
/// `tm issue seed-labels` create. The pause tool is handed the session ID, so
/// without this lookup the publish spent a UUID on a label that has never
/// existed and `gh pr create` refused the PR.
/// What: the managed record's tmux session name, or `None` when nothing on
/// record matches — an unmanaged or already-pruned caller, which the publish
/// then falls back to the session id for.
/// Test: `session_pause_pr::the_workstream_label_comes_from_the_session_name`
/// covers the derivation this feeds; this lookup is a single store read.
async fn managed_session_name(state: &Arc<DaemonState>, session_id: &str) -> Option<String> {
    let manager = state.session_manager().await;
    manager
        .list()
        .await
        .into_iter()
        .find(|record| record.id.to_string() == session_id)
        .map(|record| record.tmux_name)
}

/// What a pause snapshot publish produced.
///
/// Why: three different reasons produce no PR, and the pause result has to say
/// which — collapsing them into one `skipped` string hid a git-ignored sessions
/// tree behind the same word as an unchanged one (#7282 review).
/// What: the opened PR, or a stable snake_case reason for the skip.
/// Test: `publish_snapshot_reports_not_a_git_repo_as_the_skip_reason`.
#[derive(Debug)]
enum SnapshotPublish {
    /// A branch was pushed and a PR opened.
    Opened(crate::core::session_pause_pr::PublishOutcome),
    /// No PR: `unchanged`, `not_tracked`, or `not_a_git_repo`.
    Skipped(&'static str),
}

/// The default branch this project declares in `config.yaml`, if any.
///
/// Why: the pause publish must not assume `main` — a `master` or `develop`
/// project would then be told every pause is illegal (#7282 review). The
/// operator's declaration is the authoritative answer; the resolver in
/// `session_pause_pr` falls back to `origin/HEAD` when this returns `None`.
/// What: matches the project directory's own name against the `projects:`
/// entries and returns that entry's `default_branch`.
/// Test: `configured_default_branch_reads_the_projects_section`.
fn configured_default_branch(project_path: &Path) -> Option<String> {
    let name = project_path.file_name()?.to_str()?.to_string();
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    default_branch_for(&config.projects, &name)
}

/// The `default_branch` of the `projects:` entry named `name`.
fn default_branch_for(
    projects: &[crate::core::trusty_tools_config::ProjectConfig],
    name: &str,
) -> Option<String> {
    projects
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| p.default_branch.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `NotTracked` and `NotAGitRepo` both used to render as a bare
    /// `{"status":"skipped"}`, so a pause result could not say whether the
    /// project git-ignores its sessions or is not a checkout at all (#7282
    /// review). The wiring, not the module, is where that collapse happened.
    #[tokio::test]
    async fn publish_snapshot_reports_not_a_git_repo_as_the_skip_reason() {
        let tmp = tempfile::TempDir::new().unwrap();
        let snapshot = tmp
            .path()
            .join(".trusty-mpm/sessions/s/session-20260909-183015.md");
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        std::fs::write(&snapshot, b"# Session Pause\n").unwrap();
        let outcome = trusty_common::catchup::pause::PauseSnapshotOutcome {
            snapshot_path: snapshot,
            timestamp: chrono::Utc::now(),
        };

        // #7464: `None` for the session name — this skip is decided before the
        // `ws/` label matters.
        let publish = publish_snapshot(tmp.path(), "s", None, &outcome)
            .await
            .unwrap();
        let SnapshotPublish::Skipped(reason) = publish else {
            panic!("{publish:?}");
        };
        assert_eq!(reason, "not_a_git_repo");
    }

    /// Why: a `develop` project must publish its pause from `develop`, and the
    /// operator declares that in `config.yaml`'s `projects:` section.
    #[test]
    fn configured_default_branch_reads_the_projects_section() {
        use crate::core::trusty_tools_config::ProjectConfig;
        let projects = vec![
            ProjectConfig {
                name: "on-develop".into(),
                default_branch: Some("develop".into()),
                ..ProjectConfig::default()
            },
            ProjectConfig {
                name: "undeclared".into(),
                ..ProjectConfig::default()
            },
        ];

        assert_eq!(
            default_branch_for(&projects, "on-develop").as_deref(),
            Some("develop")
        );
        assert_eq!(default_branch_for(&projects, "undeclared"), None);
        assert_eq!(default_branch_for(&projects, "unregistered"), None);
    }

    #[tokio::test]
    async fn session_context_catchup_missing_project_dir_errors() {
        let err =
            session_context_catchup("/nonexistent/does/not/exist", None, None, false, true, 0)
                .await
                .unwrap_err();
        assert!(err.contains("project_dir"), "{err}");
    }

    #[tokio::test]
    async fn session_context_catchup_returns_expected_shape() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .output()
                .unwrap();
        };
        run(&["init"]);
        run(&["config", "user.email", "t@t.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(tmp.path().join("a.txt"), b"a").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "init"]);

        let result =
            session_context_catchup(tmp.path().to_str().unwrap(), None, None, false, true, 0)
                .await
                .unwrap();
        assert_eq!(result["watermark_advanced"], false);
        assert!(result["sessions"].is_array());
        assert!(result["recent_commits"].is_array());
        assert!(result["recent_memory"].is_array());
        assert!(result["resolved_snapshot"].is_null());
        assert!(result["resolved_via"].is_null());
        assert_eq!(result["undatable_sessions_dropped"], 0);
    }

    /// Why: #5072 — `undatable_sessions_dropped` is a receipt: an empty
    /// `sessions` array means "nothing paused" only when it is 0. No
    /// end-to-end test can drive it non-zero, because an undatable session is
    /// unreachable through the filesystem once both `PausedSession` arms fall
    /// back to mtime — so without this, substituting a literal `0` in
    /// `catchup_payload` leaves the suite green while the receipt stops
    /// reaching the MCP client.
    /// What: a merged digest's non-zero count appears on the response body.
    /// Test: itself.
    #[test]
    fn catchup_payload_carries_the_undatable_drop_count() {
        let merged = trusty_common::catchup::CatchupJson {
            undatable_sessions_dropped: 4,
            ..Default::default()
        };
        let resolved = ResolvedSnapshot::new(
            PathBuf::from("/tmp/snap.md"),
            crate::core::catchup::resolve::ResolutionPath::TmuxWindow,
        );
        let body = catchup_payload(merged, 0, Some(resolved), HydrationReceipt::default());
        assert_eq!(
            body["undatable_sessions_dropped"], 4,
            "the withheld count must reach the wire: {body}"
        );
        assert_eq!(body["resolved_snapshot"], "/tmp/snap.md");
        assert_eq!(
            body["resolved_via"], "tmux_window",
            "a fallback must never be presented as an exact match: {body}"
        );
        assert_eq!(body["watermark_advanced"], false);
    }

    /// Why: #5272, end to end through the MCP tool the report came from.
    /// Session `7bd5c27a…` called `session_context_catchup` and the response's
    /// `resolved_snapshot` was `session-20260809-010155.md`, which
    /// `sessions-log.jsonl` attributes to `2eb72dca…`. The resolver-level tests
    /// in `trusty-common` pin the behavior; this pins that the tool actually
    /// returns it, since `resolved_snapshot` is assembled here.
    /// What: pause as session A, then catch up as session B — B's
    /// `resolved_snapshot` is null while A's names A's own file.
    /// Test: itself.
    #[tokio::test]
    async fn session_context_catchup_never_resolves_another_sessions_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let state = DaemonState::shared();
        let session_a = "2eb72dca-de08-481b-8dfa-22ab7f81b1f9";
        let session_b = "7bd5c27a-475b-41df-9e9f-a6f630801717";

        let paused = session_context_pause(
            &state,
            dir,
            Some(session_a),
            "Session A's work.",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap();
        let a_snapshot = paused["snapshot_path"].as_str().unwrap().to_string();

        let for_b = session_context_catchup(dir, Some(session_b), None, false, true, 0)
            .await
            .unwrap();
        assert!(
            for_b["resolved_snapshot"].is_null(),
            "B must not be handed A's snapshot: {}",
            for_b["resolved_snapshot"]
        );

        let for_a = session_context_catchup(dir, Some(session_a), None, false, true, 0)
            .await
            .unwrap();
        assert_eq!(for_a["resolved_snapshot"], a_snapshot);
        assert_eq!(for_a["resolved_via"], "session_id");

        let anonymous = session_context_catchup(dir, None, None, false, true, 0)
            .await
            .unwrap();
        assert!(anonymous["resolved_snapshot"].is_null());
    }

    /// Why: the reported defect, end to end through the tool. Relaunching
    /// Claude Code in tmux window `@230` minted harness session
    /// `69895d04-…`, which had never paused, so `resolved_snapshot` came back
    /// null while the snapshot written from that same window sat in the store
    /// — resume degraded to a human reading prose summaries to guess which of
    /// two snapshots 19 seconds apart was theirs.
    /// What: an id that never paused plus the window that did resolves that
    /// window's snapshot and reports `resolved_via: "tmux_window"`; a
    /// different project answers null even for the same window id.
    /// Test: itself.
    #[tokio::test]
    async fn session_context_catchup_resolves_by_tmux_window_after_a_relaunch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let state = DaemonState::shared();
        let window = "tm-dogfood:0:@230";

        let paused = session_context_pause(
            &state,
            dir,
            Some("e262f4c5-d309-4203-ad3b-e0c29084d87e"),
            "Work from the previous incarnation.",
            vec![],
            vec![],
            vec![],
            Some(window),
            false,
        )
        .await
        .unwrap();
        let snapshot = paused["snapshot_path"].as_str().unwrap().to_string();

        let relaunched = session_context_catchup(
            dir,
            Some("69895d04-149d-4c31-a640-29048831f9a5"),
            Some(window),
            false,
            true,
            0,
        )
        .await
        .unwrap();
        assert_eq!(relaunched["resolved_snapshot"], snapshot);
        assert_eq!(relaunched["resolved_via"], "tmux_window");

        // Same window id, different project: the store scanned is the one
        // named by `project_dir`, so nothing resolves.
        let elsewhere = session_context_catchup(
            other.path().to_str().unwrap(),
            Some("69895d04-149d-4c31-a640-29048831f9a5"),
            Some(window),
            false,
            true,
            0,
        )
        .await
        .unwrap();
        assert!(
            elsewhere["resolved_snapshot"].is_null(),
            "another project's snapshot must not resolve: {}",
            elsewhere["resolved_snapshot"]
        );

        // A window field that does not parse resolves nothing and does not panic.
        for bad in ["", "tm-dogfood", "tm-dogfood:0"] {
            let malformed =
                session_context_catchup(dir, Some("never-paused"), Some(bad), false, true, 0)
                    .await
                    .unwrap();
            assert!(
                malformed["resolved_snapshot"].is_null(),
                "{bad:?} must resolve nothing"
            );
            assert!(malformed["resolved_via"].is_null());
        }
    }

    /// The `summary` of every session on a response, in page order.
    fn summaries(body: &Value) -> Vec<String> {
        body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["summary"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// Seed `n` paused snapshots of roughly `body_bytes` of prose each.
    ///
    /// Each summary carries its own `orig<NNNN>-` prefix so a test can tell the
    /// records apart — identical bodies would collapse under any set-based
    /// assertion and quietly pass a walk that dropped 24 of 25.
    fn seed_snapshots(project: &std::path::Path, n: usize, body_bytes: usize) {
        let dir = project.join(".trusty-mpm").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let body = "x".repeat(body_bytes);
        for i in 0..n {
            std::fs::write(
                dir.join(format!("session-20260801-12{:02}{:02}.md", i / 60, i % 60)),
                format!("## Summary\norig{i:04}-{body}\n\n## Next Steps\n{body}\n"),
            )
            .unwrap();
        }
    }

    /// Why: the reported defect, end to end through the tool. `full: true` on
    /// this repo's store returned 112,096 characters — the harness could not
    /// hand that back to the calling model, spilled it to a file, and the
    /// session trying to resume had to go read the file instead. Against this
    /// 25-snapshot fixture the unbounded code returned 154,656 characters.
    /// What: the encoded response body stays within the budget.
    /// Test: itself.
    #[tokio::test]
    async fn catchup_payload_bounds_an_oversized_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_snapshots(tmp.path(), 25, 6_000);

        let result =
            session_context_catchup(tmp.path().to_str().unwrap(), None, None, false, true, 0)
                .await
                .unwrap();

        // The arrays are bounded to the budget plus at most ONE record's
        // overshoot, so the ceiling is the budget plus this fixture's record
        // size — not a round slack number that a 59k regression would slip past.
        let fixture_record = 6_000 + 512;
        let len = serde_json::to_string(&result).unwrap().len();
        assert!(
            len <= CATCHUP_BUDGET_BYTES + fixture_record,
            "response is {len} chars against a {CATCHUP_BUDGET_BYTES}-byte budget"
        );
        assert_eq!(result["over_budget"], false);
        assert!(
            !result["sessions"].as_array().unwrap().is_empty(),
            "a bound that returns nothing is not a bound: {result}"
        );
    }

    /// Why: this repo's recurring defect is an operation that returns an
    /// incomplete result and reports success, so the loss is invisible. A
    /// capped response that reads exactly like a complete one recreates it — so
    /// the receipt has to be ON the response, naming what was withheld and how
    /// to get it, not in a log the caller never sees.
    /// What: the short page carries `truncated: true`, the full count, and a
    /// notice naming the literal `sessions_offset` that retrieves the rest;
    /// walking that offset reaches every snapshot with none repeated.
    /// Test: itself.
    #[tokio::test]
    async fn catchup_payload_announces_what_it_withheld() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        seed_snapshots(tmp.path(), 25, 6_000);

        let first = session_context_catchup(dir, None, None, false, true, 0)
            .await
            .unwrap();
        assert_eq!(first["truncated"], true);
        assert_eq!(first["sessions_total"], 25);
        assert_eq!(first["sessions_offset"], 0);
        let notice = first["truncation_notice"].as_str().unwrap().to_string();
        assert!(notice.contains("25"), "names the total: {notice}");

        let mut seen = first["sessions"].as_array().unwrap().len();
        let mut next = first["sessions_next_offset"].as_u64();
        assert!(
            notice.contains(&format!("sessions_offset: {}", next.unwrap())),
            "names the recovery: {notice}"
        );

        let mut pages = 1;
        while let Some(offset) = next {
            let body = session_context_catchup(dir, None, None, false, true, offset as usize)
                .await
                .unwrap();
            assert_eq!(body["sessions_offset"], offset);
            let n = body["sessions"].as_array().unwrap().len();
            assert!(n > 0, "page at {offset} made no progress: {body}");
            seen += n;
            next = body["sessions_next_offset"].as_u64();
            pages += 1;
            assert!(pages < 50, "paging did not terminate");
        }
        assert!(pages > 1, "the fixture must actually need paging");
        assert_eq!(seen, 25, "`full` must still deliver the whole history");
    }

    /// Why: the offset is positional into a list rebuilt from disk on every
    /// call, so a snapshot paused mid-walk sorts to the front and shifts every
    /// index. The module doc and the schema both disclose that a later page can
    /// then REPEAT a record; nothing pinned that the behaviour is repetition
    /// rather than a skip, which is the difference between a disclosed
    /// inefficiency and the silent loss this PR exists to remove.
    /// What: writes a new snapshot between page 0 and page 1 and asserts the
    /// walk still delivers every pre-existing record — the shift duplicates,
    /// never drops.
    /// Test: itself.
    #[tokio::test]
    async fn a_snapshot_written_mid_walk_repeats_a_record_but_never_drops_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        seed_snapshots(tmp.path(), 25, 6_000);

        let first = session_context_catchup(dir, None, None, false, true, 0)
            .await
            .unwrap();
        let page0: Vec<String> = summaries(&first);
        let next = first["sessions_next_offset"].as_u64().unwrap() as usize;

        // A 26th snapshot lands at the front of the newest-first list.
        std::fs::write(
            tmp.path()
                .join(".trusty-mpm")
                .join("sessions")
                .join("session-20260801-235959.md"),
            format!("## Summary\ninjected-{}\n", "n".repeat(6_000)),
        )
        .unwrap();

        let mut seen = page0.clone();
        let mut offset = Some(next);
        while let Some(o) = offset {
            let body = session_context_catchup(dir, None, None, false, true, o)
                .await
                .unwrap();
            seen.extend(summaries(&body));
            offset = body["sessions_next_offset"].as_u64().map(|v| v as usize);
        }

        // Every original record still arrives — the shift costs a repeat, and
        // the repeat is what the schema and the notice disclose.
        let originals: std::collections::HashSet<&String> =
            seen.iter().filter(|s| s.starts_with("orig")).collect();
        assert_eq!(
            originals.len(),
            25,
            "an index shift must not drop a record; got {} distinct of 25",
            originals.len()
        );
        assert!(
            seen.len() > originals.len(),
            "the shift is expected to repeat at least one record, so the \
             disclosure describes something real"
        );
    }

    /// Why: the bound must not change what a normal project gets back — a
    /// regression here breaks every resume in every project, and almost every
    /// store is far under budget.
    /// What: a three-snapshot store returns all three, reports no truncation,
    /// no notice, and no next page.
    /// Test: itself.
    #[tokio::test]
    async fn catchup_payload_leaves_a_normal_store_whole() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_snapshots(tmp.path(), 3, 200);

        let result =
            session_context_catchup(tmp.path().to_str().unwrap(), None, None, false, true, 0)
                .await
                .unwrap();

        assert_eq!(result["sessions"].as_array().unwrap().len(), 3);
        assert_eq!(result["sessions_total"], 3);
        assert_eq!(result["truncated"], false);
        assert!(result["truncation_notice"].is_null());
        assert!(result["sessions_next_offset"].is_null());
    }

    /// Why: #5386 — `resolved_snapshot` already refused to cross sessions
    /// (#5272), but `sessions[]` still returned every paused session's
    /// `source_file` AND `tmux_window` to any caller. A caller could read
    /// another session's window out of this exact response, pass it back as its
    /// own `tmux_window`, and resolve that session's snapshot deterministically
    /// — then `/tm-session-resume` adopts it as the caller's own continuation.
    /// The fix has to make the response honor #5272's invariant, not just omit
    /// the one code path that violated it.
    /// What: session B, which owns nothing, gets no `source_file`, no
    /// `tmux_window` and `owned: false` for A's session — while A still sees
    /// all three. B's whole response body contains neither A's window id nor
    /// A's snapshot path, so there is nothing to hand back.
    /// Test: itself.
    #[tokio::test]
    async fn session_context_catchup_withholds_a_non_owners_handles() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let state = DaemonState::shared();
        let session_a = "2eb72dca-de08-481b-8dfa-22ab7f81b1f9";
        let session_b = "7bd5c27a-475b-41df-9e9f-a6f630801717";
        let window = "tm-dogfood:0:@230";

        let paused = session_context_pause(
            &state,
            dir,
            Some(session_a),
            "Session A's work.",
            vec![],
            vec!["halfway through X".to_string()],
            vec!["finish X".to_string()],
            Some(window),
            false,
        )
        .await
        .unwrap();
        let a_snapshot = paused["snapshot_path"].as_str().unwrap().to_string();

        let for_b = session_context_catchup(dir, Some(session_b), None, false, true, 0)
            .await
            .unwrap();
        let listed = &for_b["sessions"][0];
        assert!(
            listed["source_file"].is_null(),
            "B must not receive A's snapshot path: {listed}"
        );
        assert!(
            listed["tmux_window"].is_null(),
            "B must not receive A's window — that is the value it would hand back: {listed}"
        );
        assert_eq!(listed["owned"], false);
        assert_eq!(listed["in_progress"], Value::Null);
        assert_eq!(listed["next_steps"], Value::Null);
        // The digest still answers "something else paused here".
        assert_eq!(listed["summary"], "Session A's work.");

        let body = for_b.to_string();
        assert!(
            !body.contains("@230"),
            "nothing in B's response may spell A's window id: {body}"
        );
        assert!(
            !body.contains(&a_snapshot),
            "nothing in B's response may spell A's snapshot path: {body}"
        );

        // A's own digest entry is untouched — resume still renders it.
        let for_a = session_context_catchup(dir, Some(session_a), None, false, true, 0)
            .await
            .unwrap();
        let own = &for_a["sessions"][0];
        assert_eq!(own["owned"], true);
        assert_eq!(own["source_file"], a_snapshot);
        assert_eq!(own["tmux_window"], window);
        assert_eq!(own["next_steps"], "- finish X");
    }

    /// Why: the digest and the resolver must agree on ownership. A caller that
    /// resolves a snapshot by window would otherwise be told, in the same
    /// response, that it does not own the row `resolved_snapshot` points at.
    /// What: the relaunched session — new id, same window — sees the full entry
    /// and `resolved_via: "tmux_window"`.
    /// Test: itself.
    #[tokio::test]
    async fn session_context_catchup_digest_agrees_with_the_window_fallback() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_str().unwrap();
        let state = DaemonState::shared();
        let window = "tm-dogfood:0:@230";

        let paused = session_context_pause(
            &state,
            dir,
            Some("e262f4c5-d309-4203-ad3b-e0c29084d87e"),
            "Work from the previous incarnation.",
            vec![],
            vec![],
            vec![],
            Some(window),
            false,
        )
        .await
        .unwrap();
        let snapshot = paused["snapshot_path"].as_str().unwrap().to_string();

        let relaunched = session_context_catchup(
            dir,
            Some("69895d04-149d-4c31-a640-29048831f9a5"),
            Some(window),
            false,
            true,
            0,
        )
        .await
        .unwrap();
        assert_eq!(relaunched["resolved_snapshot"], snapshot);
        assert_eq!(relaunched["resolved_via"], "tmux_window");
        assert_eq!(
            relaunched["sessions"][0]["owned"], true,
            "the row resolved_snapshot points at must not be redacted: {}",
            relaunched["sessions"][0]
        );
        assert_eq!(relaunched["sessions"][0]["source_file"], snapshot);
    }

    #[tokio::test]
    async fn session_context_pause_missing_project_dir_errors() {
        let state = DaemonState::shared();
        let err = session_context_pause(
            &state,
            "/nonexistent/does/not/exist",
            Some("s1"),
            "summary",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.contains("project_dir"), "{err}");
    }

    #[tokio::test]
    async fn session_context_pause_requires_summary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = DaemonState::shared();
        let err = session_context_pause(
            &state,
            tmp.path().to_str().unwrap(),
            Some("s1"),
            "   ",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.contains("summary"), "{err}");
    }

    #[tokio::test]
    async fn session_context_pause_writes_snapshot_without_pruning() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = DaemonState::shared();
        let result = session_context_pause(
            &state,
            tmp.path().to_str().unwrap(),
            Some("s1"),
            "Did the thing.",
            vec![],
            vec![],
            vec!["ship it".to_string()],
            None,
            false,
        )
        .await
        .unwrap();
        assert!(result["snapshot_path"].as_str().unwrap().ends_with(".md"));
        assert_eq!(result["pruned_worktrees"], json!([]));

        let sessions =
            crate::core::catchup::session_finder::find_paused_sessions(tmp.path()).unwrap();
        assert_eq!(sessions.len(), 1, "the written snapshot should round-trip");
    }

    /// Why: #7830 / ADR-0062 fail-open check. The local snapshot is the primary
    /// write, so a session-ref publish that cannot reach `origin` must never
    /// fail the pause — and must never be silent either. Before this change
    /// there was no ref leg at all, so a caller had nothing to read.
    /// What: a real checkout whose `origin` points at a path that does not
    /// exist, so the ref leg fails on its first remote read. The pause still
    /// returns `Ok`, the snapshot still round-trips through the unchanged
    /// reader, and the response carries `ref_published: false` plus a non-empty
    /// `ref_error`.
    ///
    /// `[session_refs].enabled` is PINNED through
    /// [`session_context_pause_with`] rather than read from the operator's real
    /// `~/.trusty-mpm/config.toml`, which would make this test pass or fail with
    /// the host's configuration (#7830 review).
    /// Test: itself.
    #[tokio::test]
    async fn a_failed_ref_publish_is_reported_and_never_fails_the_pause() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "pause@example.invalid"],
            vec!["config", "user.name", "Pause Tester"],
            vec![
                "remote",
                "add",
                "origin",
                "/nonexistent/7830/no/such/remote.git",
            ],
        ] {
            let out = trusty_common::git::command_in(repo)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture `git {args:?}` failed");
        }

        let state = DaemonState::shared();
        let result = session_context_pause_with(
            &state,
            repo.to_str().unwrap(),
            Some("s-refs-fail"),
            "Did the thing.",
            vec![],
            vec![],
            vec![],
            None,
            false,
            true,
        )
        .await
        .expect("a ref publish failure must not fail the pause");

        assert!(result["snapshot_path"].as_str().unwrap().ends_with(".md"));
        assert_eq!(
            result["ref_published"], false,
            "an unreachable origin cannot have published a ref: {result}"
        );
        let err = result["ref_error"]
            .as_str()
            .expect("the failure must be reported, not swallowed");
        assert!(!err.trim().is_empty(), "{result}");

        let sessions = crate::core::catchup::session_finder::find_paused_sessions(repo).unwrap();
        assert_eq!(
            sessions.len(),
            1,
            "the local snapshot is still the primary write"
        );
    }

    /// Why: #7830 review — a pinned `enabled = false` must leave the pause
    /// byte-identical to its pre-ADR-0062 behaviour, including writing no git
    /// config into the project. The receipt fields are present but empty.
    /// What: the same unreachable-origin checkout, paused with the flag off.
    /// Test: itself.
    #[tokio::test]
    async fn a_disabled_section_leaves_the_pause_exactly_as_it_was() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "pause@example.invalid"],
            vec!["config", "user.name", "Pause Tester"],
            vec![
                "remote",
                "add",
                "origin",
                "/nonexistent/7830/no/such/remote.git",
            ],
        ] {
            let out = trusty_common::git::command_in(repo)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture `git {args:?}` failed");
        }

        let state = DaemonState::shared();
        let result = session_context_pause_with(
            &state,
            repo.to_str().unwrap(),
            Some("s-refs-off"),
            "Did the thing.",
            vec![],
            vec![],
            vec![],
            None,
            false,
            false,
        )
        .await
        .unwrap();

        assert!(result["ref_name"].is_null(), "{result}");
        assert_eq!(result["ref_published"], false, "{result}");
        assert!(result["ref_error"].is_null(), "{result}");

        let fetch = trusty_common::git::command_in(repo)
            .args(["config", "--get-all", "remote.origin.fetch"])
            .output()
            .unwrap();
        let configured = String::from_utf8_lossy(&fetch.stdout);
        assert!(
            !configured.contains(trusty_common::catchup::session_refs::SESSION_REF_FETCH_REFSPEC),
            "a disabled section must not register the session fetch refspec: {configured}"
        );
    }

    /// Why: #7830 review — hydration used to fail into `tracing::debug!`, below
    /// the default `info` filter, and the catch-up response had no field for
    /// it. A resume that silently read a stale or empty cache then looked
    /// identical to one with nothing to restore.
    /// What: a real checkout whose `origin` does not exist. The hydration pass
    /// reports `hydrated: false` with a non-empty error rather than panicking
    /// or returning a success, the receipt reaches the response body through
    /// [`catchup_payload`], and a real `session_context_catchup` against the
    /// same checkout still succeeds and carries the `session_refs` object.
    /// Test: itself.
    #[tokio::test]
    async fn catchup_reports_a_hydration_failure_without_failing_the_catchup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "catchup@example.invalid"],
            vec!["config", "user.name", "Catchup Tester"],
            vec![
                "remote",
                "add",
                "origin",
                "/nonexistent/7830/no/such/remote.git",
            ],
        ] {
            let out = trusty_common::git::command_in(repo)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture `git {args:?}` failed");
        }

        let receipt = hydrate_session_refs(repo, Some("s-hydrate-fail")).await;
        assert!(!receipt.hydrated, "{receipt:?}");
        assert_eq!(receipt.refs_seen, 0, "{receipt:?}");
        let error = receipt
            .error
            .clone()
            .expect("an unreachable origin must be reported, not swallowed");
        assert!(!error.trim().is_empty(), "{receipt:?}");

        let body = catchup_payload(Default::default(), 0, None, receipt);
        assert_eq!(body["session_refs"]["hydrated"], false, "{body}");
        assert_eq!(body["session_refs"]["refs_seen"], 0, "{body}");
        assert_eq!(body["session_refs"]["own_ref_found"], false, "{body}");
        assert_eq!(body["session_refs"]["restored"], 0, "{body}");
        assert_eq!(body["session_refs"]["error"], error, "{body}");

        let live = session_context_catchup(repo.to_str().unwrap(), None, None, false, true, 0)
            .await
            .expect("an unreachable remote must never fail a catch-up");
        assert!(
            live["session_refs"]["hydrated"].is_boolean(),
            "the receipt must reach the wire: {live}"
        );
    }

    /// Why: #7830 closure condition 4 / ADR-0062 decision 4 — the working-tree
    /// store is a CACHE. Deleting it must be recoverable from the session ref
    /// through the unchanged read path, which is exactly what a resume from a
    /// fresh clone does.
    /// What: pause into a real checkout with a bare `origin`, publish the ref,
    /// delete `.trusty-mpm/sessions/` outright, hydrate, and catch up. The
    /// resolved snapshot is the same path the pause reported.
    ///
    /// The ref publish and the hydration are both driven explicitly rather than
    /// left to `[session_refs].enabled`, because that section is read from the
    /// operator's real `~/.trusty-mpm/config.toml` — a test that depended on it
    /// would pass or fail with the host's configuration. The one-line gate
    /// itself is covered by `config_session_refs_*`.
    /// Test: itself.
    #[tokio::test]
    async fn catchup_hydrates_a_deleted_cache_from_the_session_ref() {
        use crate::core::session_ref_publish::{SessionRefRequest, publish_receipt};

        let tmp = tempfile::TempDir::new().unwrap();
        let remote = tmp.path().join("remote.git");
        let repo = tmp.path().join("work");
        std::fs::create_dir_all(&repo).unwrap();
        fn git_fixture(dir: &Path, args: &[&str]) {
            let out = trusty_common::git::command_in(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "fixture `git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        git_fixture(
            tmp.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git_fixture(&repo, &["init", "--initial-branch=main"]);
        git_fixture(&repo, &["config", "user.email", "refs@example.invalid"]);
        git_fixture(&repo, &["config", "user.name", "Ref Tester"]);
        git_fixture(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );

        let state = DaemonState::shared();
        let paused = session_context_pause(
            &state,
            repo.to_str().unwrap(),
            Some("s-hydrate"),
            "Parked mid-review.",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap();
        let snapshot_path = PathBuf::from(paused["snapshot_path"].as_str().unwrap());

        let receipt = publish_receipt(
            &SessionRefRequest {
                repo: &repo,
                session_id: "s-hydrate",
                snapshot_path: &snapshot_path,
                timestamp: chrono::Utc::now(),
            },
            true,
        );
        assert!(receipt.published, "{receipt:?}");

        std::fs::remove_dir_all(repo.join(".trusty-mpm/sessions")).unwrap();
        assert!(!snapshot_path.exists());

        let receipt = hydrate_session_refs(&repo, Some("s-hydrate")).await;
        assert!(receipt.hydrated, "{receipt:?}");
        assert_eq!(receipt.refs_seen, 1, "{receipt:?}");
        assert!(receipt.own_ref_found, "{receipt:?}");
        assert_eq!(receipt.restored, 1, "{receipt:?}");

        let resumed = session_context_catchup(
            repo.to_str().unwrap(),
            Some("s-hydrate"),
            None,
            false,
            true,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            resumed["resolved_snapshot"].as_str(),
            Some(snapshot_path.to_string_lossy().as_ref()),
            "the hydrated cache must resolve through the unchanged read path: {resumed}"
        );
        assert_eq!(resumed["resolved_via"], "session_id");
    }

    /// Why: #6888 — this is the whole fix, end to end at the daemon boundary. A
    /// caller pauses without naming an id and, after a restart that renames the
    /// tmux session and renumbers the window, resumes and gets its own snapshot
    /// back. Before this change the PM invented an id at each end and the second
    /// one never matched the first.
    /// What: pause with `session_id: None`, then catch up with `session_id: None`
    /// from the same window; `resolved_via` is `session_id`, so the derived id —
    /// not the pre-existing window fallback — is what answered.
    /// Test: itself.
    #[tokio::test]
    async fn pause_derives_a_missing_session_id_from_the_callers_window() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = DaemonState::shared();
        let paused = session_context_pause(
            &state,
            tmp.path().to_str().unwrap(),
            None,
            "Did the thing.",
            vec![],
            vec![],
            vec![],
            Some("tm-dogfood:0:@230"),
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            paused["session_id"], "tmux-window-230",
            "the response must name the id the snapshot was filed under: {paused}"
        );

        let resumed = session_context_catchup(
            tmp.path().to_str().unwrap(),
            None,
            Some("renamed:7:@230"),
            false,
            true,
            0,
        )
        .await
        .unwrap();
        assert_eq!(
            resumed["resolved_snapshot"], paused["snapshot_path"],
            "the resume must find the pause this caller wrote: {resumed}"
        );
        assert_eq!(
            resumed["resolved_via"], "session_id",
            "the DERIVED id must be what matched, not the window fallback: {resumed}"
        );
    }

    /// Why: #6888 must not invent an id for a caller it cannot identify — a
    /// snapshot filed under a made-up string is exactly the defect. #5272's rule
    /// stands: unidentified means nothing is attributed.
    /// What: no `session_id` and no tmux window is an error naming what to pass,
    /// and nothing is written.
    /// Test: itself.
    #[tokio::test]
    async fn pause_without_an_identifiable_caller_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = DaemonState::shared();
        let err = session_context_pause(
            &state,
            tmp.path().to_str().unwrap(),
            None,
            "Did the thing.",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(err.contains("session_id"), "{err}");
        assert!(
            crate::core::catchup::session_finder::find_paused_sessions(tmp.path())
                .unwrap()
                .is_empty(),
            "a refused pause must write nothing"
        );
    }

    /// Why: #6888 — the reader has to derive the same id the writer derived, or
    /// the two agree only by the PM retyping a string. This pins the reader half.
    /// What: an explicitly-attributed pause under the derived id resolves for a
    /// catch-up caller that passes only its window, through the `session_id`
    /// route.
    /// Test: itself.
    #[tokio::test]
    async fn catchup_derives_a_missing_session_id_from_the_callers_window() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = DaemonState::shared();
        session_context_pause(
            &state,
            tmp.path().to_str().unwrap(),
            Some("tmux-window-77"),
            "Did the thing.",
            vec![],
            vec![],
            vec![],
            None,
            false,
        )
        .await
        .unwrap();

        let resumed = session_context_catchup(
            tmp.path().to_str().unwrap(),
            None,
            Some("proj:3:@77"),
            false,
            true,
            0,
        )
        .await
        .unwrap();
        assert_eq!(resumed["resolved_via"], "session_id", "{resumed}");
    }
}
