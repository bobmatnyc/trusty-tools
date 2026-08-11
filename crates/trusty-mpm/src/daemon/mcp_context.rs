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

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::core::catchup::resolve::{
    CallerIdentity, ResolvedSnapshot, redact_sessions_not_owned_by, resolve_snapshot_for_caller,
};
use crate::core::catchup::{CatchupOptions, generate_catchup_json};
use crate::daemon::state::DaemonState;

/// Shape the merged digest into the `session_context_catchup` response body.
///
/// Why: `undatable_sessions_dropped` is a receipt — an empty `sessions` array
/// means "nothing paused" only when it is 0 — and nothing else pins that it
/// reaches the wire. No end-to-end test can drive it non-zero, because an
/// undatable session is unreachable through the filesystem once both
/// `PausedSession` arms fall back to mtime, so substituting a literal `0` here
/// would leave the suite green while the receipt stopped working (#5072). A
/// pure function is the seam that makes the field assertable.
/// What: the seven response keys. `resolved_via` names which lookup produced
/// `resolved_snapshot` (`session_id`, `tmux_window`, or `null` alongside a null
/// snapshot), so a caller can tell an exact match from the window fallback
/// instead of reading both as ownership. `watermark_advanced` is always `false`
/// by construction — no path in this module calls `save_catchup_state`.
/// Test: `catchup_payload_carries_the_undatable_drop_count`,
/// `session_context_catchup_returns_expected_shape`.
fn catchup_payload(
    merged: &trusty_common::catchup::CatchupJson,
    resolved: Option<ResolvedSnapshot>,
) -> Value {
    let (snapshot, via) = match resolved {
        Some(r) => (Some(r.path.display().to_string()), Some(r.via.as_str())),
        None => (None, None),
    };
    json!({
        "sessions": merged.sessions,
        "recent_commits": merged.recent_commits,
        "recent_memory": merged.recent_memory,
        "resolved_snapshot": snapshot,
        "resolved_via": via,
        "undatable_sessions_dropped": merged.undatable_sessions_dropped,
        "watermark_advanced": false,
    })
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
/// Test: `session_context_catchup_missing_project_dir_errors`,
/// `session_context_catchup_returns_expected_shape`,
/// `session_context_catchup_never_resolves_another_sessions_snapshot`,
/// `session_context_catchup_resolves_by_tmux_window_after_a_relaunch`,
/// `session_context_catchup_withholds_a_non_owners_handles`,
/// `session_context_catchup_digest_agrees_with_the_window_fallback`.
pub async fn session_context_catchup(
    project_dir: &str,
    session_id: Option<&str>,
    tmux_window: Option<&str>,
    all_projects: bool,
    full: bool,
) -> Result<Value, String> {
    let primary = PathBuf::from(project_dir);
    if !primary.is_dir() {
        return Err(format!(
            "project_dir does not exist or is not a directory: {project_dir}"
        ));
    }

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
    let memory_url = trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();

    // #5072: `absorb` sums `undatable_sessions_dropped` across projects rather
    // than concatenating it — an empty `sessions` array is only "nothing
    // paused" when that total is 0.
    let mut merged = trusty_common::catchup::CatchupJson::default();
    let caller = CallerIdentity::new(session_id, tmux_window);

    for dir in &project_dirs {
        let opts = CatchupOptions {
            project_dir: dir.clone(),
            memory_url: memory_url.clone(),
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

    Ok(catchup_payload(&merged, resolved))
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
/// Test: `session_context_pause_missing_project_dir_errors`,
/// `session_context_pause_requires_summary`,
/// `session_context_pause_writes_snapshot_without_pruning`.
#[allow(clippy::too_many_arguments)]
pub async fn session_context_pause(
    state: &Arc<DaemonState>,
    project_dir: &str,
    session_id: &str,
    summary: &str,
    completed: Vec<String>,
    in_progress: Vec<String>,
    next_steps: Vec<String>,
    tmux_window: Option<&str>,
    prune_worktrees: bool,
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

    Ok(json!({
        "snapshot_path": outcome.snapshot_path.display().to_string(),
        "timestamp": outcome.timestamp.to_rfc3339(),
        "pruned_worktrees": pruned_worktrees,
        "skipped_dirty_worktrees": skipped_dirty,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_context_catchup_missing_project_dir_errors() {
        let err = session_context_catchup("/nonexistent/does/not/exist", None, None, false, true)
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

        let result = session_context_catchup(tmp.path().to_str().unwrap(), None, None, false, true)
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
        let body = catchup_payload(&merged, Some(resolved));
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
            session_a,
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

        let for_b = session_context_catchup(dir, Some(session_b), None, false, true)
            .await
            .unwrap();
        assert!(
            for_b["resolved_snapshot"].is_null(),
            "B must not be handed A's snapshot: {}",
            for_b["resolved_snapshot"]
        );

        let for_a = session_context_catchup(dir, Some(session_a), None, false, true)
            .await
            .unwrap();
        assert_eq!(for_a["resolved_snapshot"], a_snapshot);
        assert_eq!(for_a["resolved_via"], "session_id");

        let anonymous = session_context_catchup(dir, None, None, false, true)
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
            "e262f4c5-d309-4203-ad3b-e0c29084d87e",
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
                session_context_catchup(dir, Some("never-paused"), Some(bad), false, true)
                    .await
                    .unwrap();
            assert!(
                malformed["resolved_snapshot"].is_null(),
                "{bad:?} must resolve nothing"
            );
            assert!(malformed["resolved_via"].is_null());
        }
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
            session_a,
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

        let for_b = session_context_catchup(dir, Some(session_b), None, false, true)
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
        let for_a = session_context_catchup(dir, Some(session_a), None, false, true)
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
            "e262f4c5-d309-4203-ad3b-e0c29084d87e",
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
            "s1",
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
            "s1",
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
            "s1",
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
}
