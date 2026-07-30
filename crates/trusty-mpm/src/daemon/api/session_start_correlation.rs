//! `SessionStart`/`SessionEnd` hook correlation: linking a Claude Code session
//! UUID to the right managed session record (#1744, #4337).
//!
//! Why: `api.rs` was at its frozen 1272-SLOC allowlist budget
//! (`.line-cap-allowlist.tsv`); adding the #4337 re-sync logic there would
//! have breached it. This cluster — `correlate_session_start`,
//! `handle_session_end`, and the pure `session_end_pane_still_live` gate it
//! uses — is cohesive and self-contained (both hook handlers, called only
//! from `ingest_hook`), so it moves out wholesale, mirroring how
//! `control_routes.rs` / `claude_config_routes.rs` already keep `api.rs`
//! focused on routing.
//! What: two `pub(super)` async fns wired into `api::ingest_hook`'s
//! `SessionStart`/`SessionEnd` branches, plus the private pure liveness gate.
//! Test: the `#[cfg(test)]` suite in `api_tests.rs` (brought into scope there
//! via `api.rs`'s `use session_start_correlation::*;` and `use super::*;`).

use std::sync::Arc;

use crate::daemon::state::DaemonState;

/// Link a `SessionStart` Claude session UUID to the right managed session record.
///
/// Why (#1744): when `resume` calls `spawn_resume` with `--resume <id>`, it needs
/// the Claude Code internal session UUID persisted on the `SessionRecord`. The only
/// reliable source is the `SessionStart` hook, which fires as Claude Code starts
/// and delivers `CLAUDE_SESSION_ID` via the environment. The hook handler
/// (`tm hook`) now also embeds the caller's `cwd` in the payload (issue #1744);
/// this function uses that `cwd` to find the Active managed session running in that
/// directory and persists the UUID.
/// What: extracts `cwd` from `payload["cwd"]`; canonicalizes it and each record's
/// `workspace_path`/`cwd` (falling back to raw path on error so macOS
/// `/private/tmp` ↔ `/tmp` symlinks resolve); finds Active managed sessions whose
/// path matches. If more than one Active session matches the cwd (ambiguous) the
/// correlation is skipped with a warning to avoid mis-attribution. Single match →
/// `SessionManager::set_claude_session_id`. Best-effort — failures are logged and
/// silently swallowed so a missing correlation never blocks the hook response.
///
/// # Subagent overwrite guard + re-sync (#4337)
///
/// A subagent dispatched from a managed session's own pane (native `Task`/
/// `Agent` tool, or any other `claude` invocation that happens to share the
/// PM's cwd) is its OWN top-level Claude Code process from Claude Code's point
/// of view: it gets its own internal session UUID and fires its own
/// `SessionStart`, which lands here with the SAME `cwd` as the PM's managed
/// session — because trusty-mpm never registers a subagent as its own managed
/// session, cwd-matching alone finds exactly one (the PM's) record, the
/// pre-#4337 ambiguity guard above never triggers, and the subagent's id
/// could silently clobber the PM's own. The next in-place `--resume` would
/// then resume the SUBAGENT's transcript instead of the PM's conversation.
///
/// The fix: once a record already carries a `claude_session_id`, a
/// `SessionStart` reporting a DIFFERENT id for the same cwd is accepted ONLY
/// when the STORED id no longer resolves to a live session transcript
/// ([`stored_id_still_live`], reusing the exact staleness check
/// `runtime::claude_code::session_id_exists` the resume path already trusts
/// for the same question) — otherwise it is refused and logged. A subagent's
/// own transcript DOES exist and is live for as long as it runs, so its
/// report is refused; a genuinely stale id (the prior conversation's
/// transcript was pruned, or it never made it to disk) is accepted, so the
/// record can recover. The common case — the prior conversation cleanly
/// ending — is handled even earlier and more precisely by
/// [`handle_session_end`] clearing the field outright (and by
/// `SessionManager::mark_reactivated`, #4337, doing the same on an in-place
/// reactivate), so this staleness check is the residual, narrower safety net
/// for when that clear did not happen (a crash, a missed hook, or a request
/// that raced ahead of it).
/// Test: `session_start_hook_correlates_claude_id`,
/// `session_start_hook_still_refuses_a_live_subagent_id`,
/// `session_start_hook_reasserting_same_id_is_a_noop`,
/// `session_start_hook_re_correlates_a_stale_id` in `api_tests.rs`.
pub(super) async fn correlate_session_start(
    state: &Arc<DaemonState>,
    claude_session_id: &str,
    payload: &serde_json::Value,
) {
    let cwd_str = match payload["cwd"].as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return,
    };
    // Canonicalize hook cwd (resolves /tmp ↔ /private/tmp on macOS).
    let hook_cwd =
        std::fs::canonicalize(cwd_str).unwrap_or_else(|_| std::path::PathBuf::from(cwd_str));

    let mgr = state.session_manager().await;
    let records = mgr.list().await;

    // Collect ALL matching Active sessions to detect ambiguous cwd.
    let matched: Vec<_> = records
        .iter()
        .filter(|r| {
            if !matches!(r.state, crate::session_manager::ManagedSessionState::Active) {
                return false;
            }
            let ws_canon = r
                .workspace_path
                .as_ref()
                .map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
            let cwd_canon = std::fs::canonicalize(&r.cwd).unwrap_or_else(|_| r.cwd.clone());
            ws_canon.as_deref() == Some(hook_cwd.as_path()) || cwd_canon == hook_cwd
        })
        .collect();

    match matched.len() {
        0 => {} // no Active managed session at this cwd — silent, not an error
        1 => {
            let record = matched[0];
            let id = record.id;

            if let Some(existing) = record.claude_session_id.as_deref() {
                if existing == claude_session_id {
                    return; // no-op: already correlated to this id
                }
                if stored_id_still_live(&hook_cwd, existing) {
                    tracing::warn!(
                        managed_id = %id,
                        existing_claude_session_id = %existing,
                        reported_claude_session_id = %claude_session_id,
                        cwd = %cwd_str,
                        "SessionStart: refusing to overwrite a still-live claude_session_id \
                         with a different one — likely a subagent sharing this cwd, not the \
                         pane's top-level process (#4337)"
                    );
                    return;
                }
                tracing::info!(
                    managed_id = %id,
                    stale_claude_session_id = %existing,
                    claude_session_id = %claude_session_id,
                    cwd = %cwd_str,
                    "SessionStart: stored claude_session_id no longer resolves to a live \
                     session — re-correlating to the new id (#4337)"
                );
            }

            match mgr.set_claude_session_id(&id, claude_session_id).await {
                Ok(()) => tracing::info!(
                    managed_id = %id,
                    claude_session_id = %claude_session_id,
                    cwd = %cwd_str,
                    "SessionStart: linked Claude session to managed session (#1744)"
                ),
                Err(e) => tracing::warn!(
                    managed_id = %id,
                    "SessionStart: failed to persist claude_session_id: {e}"
                ),
            }
        }
        n => {
            tracing::warn!(
                cwd = %cwd_str,
                n,
                "SessionStart: {n} Active managed sessions share the same cwd — \
                 skipping claude_session_id attribution to avoid mis-assignment (#1744)"
            );
        }
    }
}

/// Whether a previously-stored `claude_session_id` still resolves to a live
/// Claude Code session transcript for `cwd` (#4337 re-sync gate).
///
/// Why: reuses `runtime::claude_code::session_id_exists` — the SAME
/// staleness check `spawn_resume` already trusts before honoring a stored id
/// with `--resume` — so the two call sites can never disagree about what
/// "stale" means. A subagent's transcript exists and is live for as long as
/// it runs, so this returns `true` for it; a genuinely stale id (pruned,
/// moved, or never flushed to disk) returns `false`.
/// What: resolves the managed `CLAUDE_CONFIG_DIR` via
/// [`crate::core::trusty_tools_config::managed_claude_config_dir`] and
/// delegates to [`crate::runtime::session_id_exists`].
/// Test: `session_start_hook_re_correlates_a_stale_id`,
/// `session_start_hook_still_refuses_a_live_subagent_id` in `api_tests.rs`.
fn stored_id_still_live(cwd: &std::path::Path, id: &str) -> bool {
    let config_dir = crate::core::trusty_tools_config::managed_claude_config_dir();
    crate::runtime::session_id_exists(cwd, config_dir.as_deref(), id)
}

/// Pure decision: should `handle_session_end` DEFER its `Stopped` transition
/// because the record's own tmux pane still shows a live runtime? (#2454)
///
/// Why: `SessionEnd` firing is Claude Code self-reporting that its session is
/// "ending / being torn down" ([`crate::core::hook::HookEvent::SessionEnd`])
/// — NOT proof the OS process (and therefore the tmux pane) has already fully
/// exited; the hook POST can race the pane's actual fall-back to a bare idle
/// shell. `SessionManager::resume` decides pane reuse purely on
/// `tmux.session_exists()`, so flipping the record to `Stopped` while the
/// pane is still occupied could let a `resume` (manual or the auto-resume
/// supervisor) type into a pane the outgoing runtime hasn't relinquished yet.
/// This reuses the EXACT SAME classification the 60-second runtime-exit
/// reaper already trusts for this question —
/// [`crate::daemon::runtime_reap::session_has_live_pane`] (any-pane-live
/// aggregation over the idle-shell allowlist and the [`ChildLivenessProbe`]
/// fail-closed gate) — instead of inventing a second one, so the two call
/// sites can never disagree about what "exited" means.
/// What: delegates directly to
/// [`session_has_live_pane`](crate::daemon::runtime_reap::session_has_live_pane),
/// which returns `true` (defer) when ANY pane whose `session_name` matches
/// `tmux_name` is NOT yet exited. Fixed by #2463: this previously used
/// `panes.iter().find(...)` — the FIRST matching pane only — which
/// misclassified a manually-split, multi-pane managed session as idle
/// whenever `tmux list-panes -a` happened to return an idle pane ahead of a
/// live one. When no matching pane is found (already gone, or tmux/pane
/// enumeration was unavailable and `panes` is empty) this returns `false`
/// (proceed) — there is nothing to protect from a destructive teardown here,
/// since `mark_runtime_exited_stopped` never touches the pane either way; a
/// genuinely vanished session is left to the #1744 reaper as before. Pure —
/// no I/O — so it is unit-testable without a live tmux.
/// Test: `session_end_pane_still_live_true_for_running_agent`,
/// `session_end_pane_still_live_false_for_idle_shell`,
/// `session_end_pane_still_live_false_when_pane_missing`,
/// `session_end_pane_still_live_true_when_any_of_multiple_panes_live`,
/// `session_end_pane_still_live_false_when_all_of_multiple_panes_idle` in
/// `api_tests.rs`.
pub(super) fn session_end_pane_still_live(
    tmux_name: &str,
    panes: &[crate::daemon::orphan_gc::PaneInfo],
    probe: &dyn crate::daemon::orphan_gc::ChildLivenessProbe,
) -> bool {
    crate::daemon::runtime_reap::session_has_live_pane(tmux_name, panes, probe)
}

/// Immediately mark a managed session Stopped on `SessionEnd`, WITHOUT killing
/// its tmux pane (#2454).
///
/// Why (#1744, revised #2454): without this, a managed session that exits
/// ungracefully (tmux pane killed, terminal closed) stays `Active` in the store
/// until the 60-second reap loop fires. On `SessionEnd`, Claude Code's internal
/// session has already ended; marking the managed session `Stopped` immediately
/// keeps the daemon's view consistent and lets the operator see the correct
/// state right away. This originally called [`SessionManager::stop`], which is
/// the EXPLICIT-stop contract (`tm session stop`) and therefore also
/// `graceful_terminate_runtime`s / `kill_session`s the tmux pane — but a
/// `SessionEnd` correlation is a self-healing state reconcile, not a
/// human/client teardown request, exactly like the 60-second runtime-exit
/// reaper (see [`crate::daemon::runtime_reap`] #2023 A). Routing it through
/// `stop` could destroy a pane the operator was still attached to, purely
/// because the daemon correlated the exit a few hundred milliseconds before
/// they noticed. This now mirrors the reaper and calls
/// [`SessionManager::mark_runtime_exited_stopped`] instead, leaving the pane
/// alive. It also gates that transition behind
/// [`session_end_pane_still_live`] (#2454 follow-up): the hook can race the
/// pane's actual teardown, so if the pane is found and still shows a live
/// runtime the transition is DEFERRED for this hook receipt — the 60-second
/// reaper will pick the session up once its pane is genuinely idle. This gate
/// is best-effort: it needs a real `tmux` binary to enumerate panes, so on a
/// host with no `tmux` (or a `list_managed_panes` failure) it fails OPEN and
/// proceeds with the transition unchecked, same as pre-gate behavior.
///
/// #4337: BEFORE that pane-liveness gate, this ALSO clears the record's
/// `claude_session_id` via `SessionManager::clear_claude_session_id_if` — an
/// exact-match, race-safe clear (a concurrent fresher correlation is never
/// clobbered). `SessionEnd` is Claude Code's own unambiguous signal that
/// THIS conversation ended, independent of whether the pane still shows a
/// live runtime (that gate is about the TMUX PANE, not the Claude session),
/// so the id is stale from this point on regardless of the deferral below.
/// Clearing it here means the record's NEXT `SessionStart` — the real
/// relaunch — takes the plain "no existing id" branch in
/// [`correlate_session_start`] rather than depending on a later subagent's
/// report ever satisfying the narrower stale-transcript re-sync gate.
/// What: searches Active managed sessions for one whose `claude_session_id`
/// matches the hook's `session_id`. If found, clears that id (best-effort),
/// then best-effort discovers the live tmux panes
/// ([`crate::daemon::tmux::TmuxDriver::discover`] + `list_managed_panes`) and
/// calls [`session_end_pane_still_live`]; when that reports the pane is
/// still live, logs and returns WITHOUT transitioning. Otherwise calls
/// `SessionManager::mark_runtime_exited_stopped`, which marks the record
/// `Stopped` and persists WITHOUT calling `graceful_terminate_runtime` /
/// `kill_session`. Best-effort — failures are logged and swallowed so the
/// hook response is unaffected.
/// Test: `session_end_hook_marks_managed_stopped` and
/// `session_end_hook_does_not_kill_pane` in `api_tests.rs` cover the
/// transition itself (tmux is unavailable/pane-not-found in that harness, so
/// the gate fails open); the gate's own decision logic is covered by the
/// `session_end_pane_still_live_*` unit tests above;
/// `session_end_hook_clears_claude_session_id` covers the #4337 clear.
pub(super) async fn handle_session_end(state: &Arc<DaemonState>, claude_session_id: &str) {
    let mgr = state.session_manager().await;
    let records = mgr.list().await;
    let matched = records.iter().find(|r| {
        matches!(r.state, crate::session_manager::ManagedSessionState::Active)
            && r.claude_session_id.as_deref() == Some(claude_session_id)
    });
    if let Some(r) = matched {
        let id = r.id;
        let tmux_name = r.tmux_name.clone();

        // #4337: this Claude session has genuinely ended — un-correlate it so
        // a later, unrelated SessionStart is never compared against a dead
        // id. Exact-match guarded, so a concurrent fresher correlation (a
        // real relaunch that already landed) is never clobbered.
        if let Err(e) = mgr.clear_claude_session_id_if(&id, claude_session_id).await {
            tracing::warn!(
                managed_id = %id,
                "SessionEnd: failed to clear claude_session_id: {e}"
            );
        }

        // #2454: best-effort gate — defer if the pane still shows a live
        // runtime. Fails open (proceeds) when tmux is unavailable or pane
        // enumeration fails, matching the transition's own non-destructive
        // nature (nothing here ever touches the pane regardless).
        if let Ok(driver) = crate::daemon::tmux::TmuxDriver::discover()
            && let Ok(panes) = driver.list_managed_panes()
            && session_end_pane_still_live(
                &tmux_name,
                &panes,
                &crate::daemon::orphan_gc::ProcessTreeProbe,
            )
        {
            tracing::info!(
                managed_id = %id,
                claude_session_id = %claude_session_id,
                name = %tmux_name,
                "SessionEnd: deferring Stopped transition — pane still shows a live runtime (#2454); the 60s reaper will retry once idle"
            );
            return;
        }

        match mgr.mark_runtime_exited_stopped(&id).await {
            Ok(_) => tracing::info!(
                managed_id = %id,
                claude_session_id = %claude_session_id,
                "SessionEnd: marked managed session Stopped immediately (#1744, pane left alive #2454)"
            ),
            Err(e) => tracing::warn!(
                managed_id = %id,
                "SessionEnd: failed to mark managed session Stopped: {e}"
            ),
        }
    }
}
