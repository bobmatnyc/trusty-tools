//! Auto-prune definitively-dead (`unresumable`) managed-session records at
//! listing time (owner request 2026-07-29; hardened per code-critic WARN,
//! PR #4384 review).
//!
//! Why: extracted out of `session_picker.rs` (which sits at the 500-SLOC
//! production cap, mirroring why `session_picker_render.rs`/
//! `session_picker_rename.rs` were split out before it). Before this module
//! existed, a session flagged `unresumable` (its workspace GC-pruned out from
//! under it) sat in the picker forever printing `DEAD: workspace removed; use
//! [d<N>] to remove the record` — the operator had to notice the row and type
//! the number themselves, every time.
//!
//! The critic's WARN (then BLOCK on re-review) identified this feature's
//! first cut as converting an advisory flag into an unconditional,
//! unconfirmed, uncapped, unverified destructive action. Four independent
//! hardenings close that gap:
//!   1. **Record-only removal** — [`decommission_dead_record`] calls the
//!      `/decommission` route with `record_only=true`
//!      (`SessionManager::decommission_record_only`), which touches neither the
//!      filesystem nor the runtime. A remount between the listing-time probe and
//!      this call can never lose data, because nothing is ever deleted here.
//!      (#4728: the "nor the runtime" half was NOT true until that fix — the
//!      teardown call sat above the `record_only` guard, so this sweep could
//!      SIGTERM and kill a live pane whose name matched the record's. Treat
//!      "record-only" as a claim that must stay pinned by tests, never as a
//!      property the name guarantees.)
//!   2. **Two-sightings confirmation** — [`auto_prune_dead_records_at`] only
//!      acts on a session id first seen dead at least
//!      [`SIGHTING_MIN_AGE_SECS`] ago, tracked via a small persisted marker
//!      file ([`load_seen`]/[`save_seen`]). A single-shot blip (one bad
//!      listing) never triggers a prune, and neither does a tight loop of
//!      them — the window is elapsed TIME, not a call count.
//!   3. **Per-call cap** — at most [`AUTO_PRUNE_CAP`] confirmed records are
//!      decommissioned per call; the rest are reported as "pending
//!      confirmation" rather than acted on. A bad-mount day cannot
//!      mass-tombstone an entire fleet in one `tm ls`.
//!   4. **Response-body verification (a daemon/client version-skew guard)**
//!      — `record_only=true` alone is not proof of safety: a daemon process
//!      still running a build older than this fix has no
//!      `Query<DecommissionQuery>` extractor on the route at all, silently
//!      ignores the param (axum drops query params a handler doesn't
//!      declare), and falls through to the OLD unconditional full-teardown
//!      path — which ALSO returns 200. [`decommission_dead_record`] parses
//!      the response body and requires `workspace_removed == false` before
//!      counting a call as safe; anything else trips
//!      [`STALE_DAEMON_MARKER_KEY`], which disables further auto-prune
//!      attempts until the sentinel expires ([`STALE_DAEMON_TTL_SECS`], 1
//!      hour) — self-healing: one probe per hour against a genuinely stale
//!      daemon, not a permanent lockout once it's restarted. Records held
//!      back by this gate fold into [`AutoPruneOutcome::pending`], never
//!      silently disappearing from the reported count.
//!
//! What: [`prune_and_report`] is the production entry point (resolves the
//! marker file under `~/.trusty-mpm` and prints the operator summary);
//! [`auto_prune_dead_records_at`] is the testable core (explicit marker-file
//! path); [`AutoPruneOutcome`] is the three-part result (`kept`, `pruned`,
//! `pending`) it reports.
//!
//! Coverage (#4702) — this module used to fire from the interactive TTY picker
//! ONLY, and to act on the daemon-computed `unresumable` flag ONLY. Both
//! narrowings let dead records accumulate without bound:
//!   * every piped / scripted / `--json` `tm ls` took `managed::session_ls`,
//!     which never pruned — fixed by routing BOTH listing paths through
//!     [`prune_and_report`];
//!   * `unresumable` is computed daemon-side for records whose PERSISTED state
//!     is `Stopped`/`Errored`. A zombie (persisted `Active`, tmux pane gone) is
//!     DISPLAY-reconciled to `stopped` but never probed, so it reads
//!     `unresumable == false` forever — fixed by
//!     [`workspace_verified_gone`], a client-side probe of the same workdir
//!     candidates the wire summary carries.
//!
//! Test: `auto_prune_dead_records_removes_confirmed_unresumable_records`,
//! `auto_prune_dead_records_first_sighting_is_not_pruned`,
//! `auto_prune_dead_records_keeps_workspace_present_records`,
//! `auto_prune_dead_records_is_noop_when_nothing_is_dead`,
//! `auto_prune_dead_records_honors_the_cap`,
//! `auto_prune_dead_records_stops_sweep_when_daemon_reports_workspace_removed`,
//! `auto_prune_dead_records_stale_daemon_sentinel_expires_after_ttl`,
//! `auto_prune_clears_stopped_record_whose_workspace_is_gone`,
//! `auto_prune_keeps_stopped_record_whose_workspace_still_exists`,
//! `auto_prune_never_touches_a_running_record`,
//! `auto_prune_never_touches_a_decommissioned_record`,
//! `auto_prune_always_requests_record_only_never_full_teardown`
//! in `tests_behavior_d_tests.rs`; the record-only server-side guarantee is
//! pinned by `decommission_record_only_never_removes_existing_workspace`
//! (`session_manager::tests`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use trusty_mpm::client::ManagedSessionSummary;

/// Maximum number of CONFIRMED dead records one call may decommission (critic
/// HIGH finding #1, owner request 2026-07-29).
///
/// Why: "a bad-mount day must not mass-tombstone every session in one `tm
/// ls`" — even a session flagged dead on two consecutive listings might all
/// share one now-unmounted volume. Capping bounds the blast radius of any
/// single call regardless of how many records are confirmed-eligible.
const AUTO_PRUNE_CAP: usize = 5;

/// Basename of the two-sightings confirmation marker file, under the
/// framework root (`~/.trusty-mpm/` by default).
const SEEN_MARKER_FILENAME: &str = "auto-prune-seen.json";

/// Minimum elapsed time between a record's FIRST sighting as dead and the call
/// that may act on it: 10 minutes (#4702).
///
/// Why: "two sightings" is only a safeguard if the two are genuinely separated
/// in time. Before #4702 the check was `seen.contains_key(id)` — presence, never
/// age — so `tm ls; tm ls` confirmed instantly and [`AUTO_PRUNE_CAP`] degraded
/// from a rate limit to a per-call batch size a loop could defeat. Ten minutes
/// is long enough that no scripted or accidental double-listing clears anything,
/// and short enough that an operator's ordinary session-to-session usage still
/// reaps dead rows without ever running a manual command.
const SIGHTING_MIN_AGE_SECS: i64 = 10 * 60;

/// Reserved key inside the marker file recording "a stale daemon already
/// proved it ignores `record_only`" (critic CRITICAL finding, 2026-07-30
/// re-review of PR #4384).
///
/// Why: `?record_only=true` is read by a `Query<DecommissionQuery>`
/// extractor this feature ADDS to the `/decommission` handler. Axum silently
/// drops a query param a handler doesn't declare — a daemon process still
/// running a build older than this fix has no such extractor and falls
/// through to the OLD unconditional full-teardown `decommission()` for EVERY
/// auto-prune call, regardless of the query string sent. The HTTP status
/// alone cannot distinguish that from a genuine record-only success (both
/// return 200) — only the response body's `workspace_removed` field can
/// (see [`decommission_dead_record`]). Once that's observed, auto-prune must
/// stay off for the rest of THIS invocation — the daemon does not un-stale
/// itself mid-process, and the picker's own re-fetch loop calls
/// [`auto_prune_dead_records_at`] repeatedly within one process. Storing this
/// in the SAME marker file (rather than process-global state) keeps the
/// behavior scoped to `marker_path` — exactly like every other piece of
/// state this module tracks — so tests using distinct tempdir paths can
/// never bleed into each other despite `cargo test` running many tests in
/// one process.
/// What: an unlikely-to-collide sentinel key (real session ids are UUIDs);
/// its value is the RFC 3339 timestamp of first detection — checked against
/// [`STALE_DAEMON_TTL_SECS`] by [`stale_daemon_sentinel_active`], not merely
/// its presence.
/// Test: `auto_prune_dead_records_stops_sweep_when_daemon_reports_workspace_removed`,
/// `auto_prune_dead_records_stale_daemon_sentinel_expires_after_ttl`.
const STALE_DAEMON_MARKER_KEY: &str = "__stale_daemon_detected__";

/// TTL for the `STALE_DAEMON_MARKER_KEY` sentinel: 1 hour (owner request
/// 2026-07-30 follow-up).
///
/// Why: a daemon that was stale an hour ago may have been restarted since —
/// wedging auto-prune off permanently would defeat the point of a
/// self-healing safeguard once the operator (or the daemon's own restart
/// machinery) catches up. Expiring the sentinel lets the NEXT invocation
/// retry; if the daemon is STILL stale, [`decommission_dead_record`]'s
/// body-check trips again immediately and rewrites a fresh sentinel — one
/// probe per hour against a genuinely stale daemon, never a permanent
/// lockout.
const STALE_DAEMON_TTL_SECS: i64 = 60 * 60;

/// Result of one [`auto_prune_dead_records`] / [`auto_prune_dead_records_at`]
/// call.
///
/// Why: the caller (`fetch_live_sessions`) needs three independent numbers to
/// report honestly — what it removed, what it left alone because a
/// confirmation window or the cap wasn't met, and the surviving list to
/// render — rather than collapsing them into a single count.
pub(crate) struct AutoPruneOutcome {
    /// Every session NOT successfully decommissioned this call — healthy
    /// sessions, first-sightings awaiting confirmation, confirmed sessions
    /// over the cap, and any record whose decommission attempt failed.
    pub(crate) kept: Vec<ManagedSessionSummary>,
    /// Count of records actually decommissioned this call (never more than
    /// [`AUTO_PRUNE_CAP`]).
    pub(crate) pruned: usize,
    /// Count of `unresumable` records NOT acted on this call — a first
    /// sighting (not yet confirmed), confirmed-but-over-the-cap, or held
    /// back because the stale-daemon gate is active (owner request
    /// 2026-07-30 follow-up — this must fold in here or the reported count
    /// under-represents exactly the batch that gate is protecting).
    pub(crate) pending: usize,
}

/// Everything the prune needs from its environment, resolved once per
/// invocation (#4702).
///
/// Why: both inputs — the confirmation marker file and the live-tmux name set —
/// must be injectable, or a test silently depends on the developer's machine.
/// CI caught exactly that: the `session_ls_*` tests reached
/// [`live_tmux_session_names`] directly, so they passed locally (tmux present,
/// enumeration succeeds) and failed on a runner with no tmux, where the
/// fail-closed rule correctly pruned nothing. Bundling both into one value keeps
/// the seam at every layer without growing an eighth positional parameter on
/// `session_ls_at`.
/// What: [`PruneContext::production`] resolves both for real use;
/// [`PruneContext::hermetic`] takes explicit values for tests.
pub(crate) struct PruneContext {
    /// Where the two-sightings confirmation marker lives.
    pub(crate) marker_path: PathBuf,
    /// Live tmux session names, or `None` when tmux could not be enumerated at
    /// all — which [`auto_prune_dead_records_at`] treats as "prune nothing".
    pub(crate) live_tmux_names: Option<HashSet<String>>,
}

impl PruneContext {
    /// Resolve both inputs for real use: the marker under `~/.trusty-mpm`, and a
    /// live tmux enumeration.
    pub(crate) fn production() -> Self {
        Self {
            marker_path: default_marker_path(),
            live_tmux_names: live_tmux_session_names(),
        }
    }

    /// Build a context with explicit values — the test seam.
    ///
    /// Why: a test must never write the operator's real marker file, and must
    /// never depend on whether the machine running it happens to have tmux.
    #[cfg(test)]
    pub(crate) fn hermetic(marker_path: PathBuf, live_tmux_names: Option<HashSet<String>>) -> Self {
        Self {
            marker_path,
            live_tmux_names,
        }
    }
}

/// Resolve the confirmation marker file under the default framework root.
///
/// Why: the framework root (`~/.trusty-mpm`) is where every other piece of
/// `tm`'s per-user state already lives (`FrameworkPaths::default`); resolving
/// it here (rather than inside the testable core) keeps the core injectable
/// for tests without touching the real home directory. Exposed to `managed.rs`
/// so `tm session ls`'s production wrapper resolves the SAME file the picker
/// path uses — two marker files would mean two independent confirmation
/// windows for one fleet (#4702).
/// What: `FrameworkPaths::default().root` joined with
/// [`SEEN_MARKER_FILENAME`].
pub(crate) fn default_marker_path() -> PathBuf {
    trusty_mpm::core::paths::FrameworkPaths::default()
        .root
        .join(SEEN_MARKER_FILENAME)
}

/// Production entry point for EVERY listing surface: prune, print the operator
/// summary, and hand back the surviving sessions (#4702).
///
/// Why: before #4702 the prune fired only where a caller had explicitly opted
/// in with `allow_auto_prune = true`, and the only callers that did were gated
/// on `stdin.is_terminal() && stdout.is_terminal()`. Every piped, scripted, and
/// `--json` invocation therefore accumulated dead records forever — 48 of 66
/// records stale on the reporting machine. The opt-in existed because
/// auto-prune was read as destructive; it is not, and cannot be: the ONLY
/// mutation it performs is [`decommission_dead_record`]'s `record_only=true`
/// call, verified against the response body, which never removes a git
/// worktree, a branch, or any file on disk. Consistency across invocations is
/// therefore strictly safer than an inconsistent opt-in that leaves the list
/// unusable as a picker.
/// What: [`auto_prune_dead_records_at`] against [`default_marker_path`], then
/// the two operator-facing stderr lines (stdout stays clean for `--json` and
/// for pipes), then `outcome.kept`.
/// Test: `session_ls_prunes_dead_records_on_piped_invocation`,
/// `session_ls_json_passthrough_prunes_dead_records`; the decision logic
/// itself via [`auto_prune_dead_records_at`] directly.
pub(crate) async fn prune_and_report(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<ManagedSessionSummary>,
) -> Vec<ManagedSessionSummary> {
    prune_and_report_at(client, url, sessions, &PruneContext::production()).await
}

/// [`prune_and_report`] with an injected [`PruneContext`] — the testable core.
///
/// Why: a test must never write the operator's real
/// `~/.trusty-mpm/auto-prune-seen.json`, and must never depend on whether the
/// machine running it has tmux. See [`PruneContext`] for the CI failure that
/// made the second half of that non-negotiable.
/// What: see [`prune_and_report`].
/// Test: `session_ls_prunes_dead_records_on_piped_invocation`.
pub(crate) async fn prune_and_report_at(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<ManagedSessionSummary>,
    ctx: &PruneContext,
) -> Vec<ManagedSessionSummary> {
    let outcome = auto_prune_dead_records_at(
        client,
        url,
        sessions,
        &ctx.marker_path,
        ctx.live_tmux_names.clone(),
    )
    .await;
    if outcome.pruned > 0 {
        eprintln!(
            "tm: pruned {} dead record{} (workspace gone)",
            outcome.pruned,
            if outcome.pruned == 1 { "" } else { "s" }
        );
    }
    if outcome.pending > 0 {
        eprintln!(
            "tm: {} more dead record{} pending confirmation",
            outcome.pending,
            if outcome.pending == 1 { "" } else { "s" }
        );
    }
    outcome.kept
}

/// The workdir candidates a LISTED session carries on the wire (#4702).
///
/// Why: `session_manager::resume_workdir::workdir_candidates` probes
/// `[last_cwd, workspace_path, cwd]` against the daemon's own `SessionRecord`.
/// `last_cwd` is not part of `ManagedSessionSummary`, so the client can only
/// see the other two — which is why [`workspace_verified_gone`] is deliberately
/// a NARROWER predicate than the daemon's `is_unresumable`, never a wider one:
/// a record this returns "gone" for would also be gone by the daemon's list.
/// What: `[workspace_path, cwd]`, `None`-filtered by the caller.
fn workdir_candidates(s: &ManagedSessionSummary) -> [Option<&str>; 2] {
    [s.workspace_path.as_deref(), s.cwd.as_deref()]
}

/// Independently verify, from the CLI, that a listed session has no workdir
/// left on disk (#4702).
///
/// Why: the daemon's `unresumable` flag is computed only for records whose
/// PERSISTED state is `Stopped`/`Errored`. The list endpoint then
/// DISPLAY-reconciles state against live tmux, so a zombie record (persisted
/// `Active`, its pane long gone) shows as `stopped` while its `unresumable`
/// flag was never computed at all — it reads `false` forever and no prune ever
/// touched it. That class is exactly what the reporter saw accumulating. This
/// probe closes the gap client-side rather than changing the daemon, so the fix
/// works against an ALREADY-RUNNING older daemon (a CLI upgrade never bounces
/// it).
///
/// 🔴 This predicate is the ONLY thing standing between a still-recoverable
/// stopped session and a tombstone, so it is deliberately FAIL-CLOSED-TOWARD-
/// KEEPING. `true` requires ALL of:
///   * at least one candidate path was present on the wire (a record carrying
///     none is unverifiable, and is kept);
///   * every candidate's probe came back a definitive `Ok(false)`. A probe
///     `Err` (permission denied, transient I/O) counts as "possibly present";
///   * every candidate's PARENT DIRECTORY exists — see below.
///
/// 🔴 The parent check is not defensive padding, it is the whole point of the
/// word "verified". `try_exists("/Volumes/Unplugged/work/proj")` returns
/// `Ok(false)`, NOT `Err`, when the volume is not mounted — ENOENT on an
/// intermediate component is indistinguishable from ENOENT on the leaf. Without
/// this check, unplugging an external drive reads as "verified gone" for every
/// session on it, and one `tm ls` tombstones the lot. Requiring the parent to
/// exist means the only thing we ever call gone is a leaf missing from a
/// directory we can still see. This errs toward keeping — a workspace whose
/// whole parent tree was deleted is kept rather than cleared, which costs one
/// stale row and risks nothing. Measured 2026-08-03: 4 of 5 spot-checked
/// stopped workspaces still existed on disk.
///
/// What: for each of [`workdir_candidates`], `tokio::fs::try_exists` on the path
/// itself and on `Path::parent()`.
/// Test: `auto_prune_clears_stopped_record_whose_workspace_is_gone`,
/// `auto_prune_keeps_stopped_record_whose_workspace_still_exists`,
/// `auto_prune_keeps_record_whose_parent_directory_is_unreachable`.
async fn workspace_verified_gone(s: &ManagedSessionSummary) -> bool {
    let mut probed_any = false;
    for candidate in workdir_candidates(s).into_iter().flatten() {
        probed_any = true;
        match tokio::fs::try_exists(candidate).await {
            // Definitively present, or the probe could not tell — either way,
            // never treat this workspace as gone.
            Ok(true) | Err(_) => return false,
            Ok(false) => {}
        }
        // #4702: the leaf is absent — but is its directory even reachable? An
        // unmounted volume answers `Ok(false)` for every path under it.
        let Some(parent) = std::path::Path::new(candidate).parent() else {
            // No parent (a bare relative name, or the filesystem root itself) —
            // nothing to corroborate against, so refuse to call it gone.
            return false;
        };
        if !tokio::fs::try_exists(parent).await.unwrap_or(false) {
            return false;
        }
    }
    probed_any
}

/// Whether a record's lifecycle state makes it eligible for a record-only
/// clear at all (#4702).
///
/// Why: the auto-prune must stay strictly inside the "session is over" class.
/// `decommissioned` records (35 on the reporting machine) are explicitly out of
/// scope — nobody has assessed what discarding them costs — and a `deleted`
/// slot tombstone is a rendering placeholder, not a session. Anything running
/// (`active`, `provisioning`) is untouchable by construction, because the list
/// endpoint reconciles a live tmux pane back to `active` before the CLI ever
/// sees the row.
///
/// 🔴 `errored` records escape that reconciliation entirely:
/// `reconcile_live_state` only rewrites `state` for records persisted
/// `Active`/`Stopped`, so an `Errored` row is served with its persisted state
/// verbatim and a live pane never turns it back into `active`. Tombstoning such
/// a record is not cosmetic — `Decommissioned` is TERMINAL, so a running agent
/// silently becomes unreachable through `tm`: no resume, no reattach, hidden
/// from the picker.
///
/// `attached` alone does not close that: it proves a client is attached, not
/// that no pane exists, so a live-but-DETACHED errored session would still be
/// cleared. `live_tmux_names` closes it (PR #4725 review round 2) — the CLI
/// enumerates live tmux sessions itself and never clears a record whose name is
/// among them. Name membership is a weak signal, but here it is used to KEEP,
/// never to act: a name collision costs one stale row, whereas the same weak
/// signal used to KILL is exactly what made #4728 destructive. When the caller
/// could not enumerate tmux at all it passes an empty set only if it also
/// declines to prune — see [`auto_prune_dead_records_at`].
///
/// What: the DISPLAY state must be `stopped`/`errored`; the row must not be a
/// slot tombstone, must not be `attached`, and must not name a live tmux
/// session; and the PERSISTED state (when the daemon sends it) must not already
/// be terminal.
/// Test: `auto_prune_never_touches_a_running_record`,
/// `auto_prune_never_touches_a_decommissioned_record`,
/// `auto_prune_never_touches_an_attached_record`,
/// `auto_prune_never_touches_an_errored_record_with_a_live_detached_pane`.
fn is_clearable_state(s: &ManagedSessionSummary, live_tmux_names: &HashSet<String>) -> bool {
    if s.deleted || s.attached || !matches!(s.state.as_str(), "stopped" | "errored") {
        return false;
    }
    if live_tmux_names.contains(&s.name) {
        return false;
    }
    !matches!(
        s.persisted_state.as_deref(),
        Some("decommissioned" | "deleted")
    )
}

/// Enumerate live tmux session names from the CLI (#4702, PR #4725 review).
///
/// Why: the wire summary carries no "this errored record's pane is alive but
/// detached" signal, and adding one would need a daemon change — which this fix
/// deliberately avoids, since a CLI upgrade never bounces the long-lived daemon.
/// Enumerating client-side gives the second liveness signal with no protocol
/// change at all.
/// What: routes through the crate's single tmux spawn point
/// (`core::tmux::run_tmux`, which owns the macOS TCC-disclaim wrapper) with the
/// shared `TmuxCommand::ListSessions` argv, and parses `SESSION_LIST_FORMAT`'s
/// `name:created:attached` rows. Returns `None` when tmux could not be
/// enumerated (not installed, no server, non-zero exit) — the caller treats
/// that as "cannot determine liveness" and prunes nothing, mirroring
/// `reconcile_against_tmux`'s fail-closed contract on the daemon side.
/// Test: `auto_prune_never_touches_an_errored_record_with_a_live_detached_pane`
/// covers the consuming predicate with an injected set; this thin adapter needs
/// a live tmux binary and is exercised in normal operation.
fn live_tmux_session_names() -> Option<HashSet<String>> {
    let out = trusty_mpm::core::tmux::run_tmux(&trusty_mpm::core::tmux::TmuxCommand::ListSessions)
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split(':').next())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// The full "this record is definitively dead" predicate (#4702).
///
/// Why: `unresumable` alone missed the majority of what operators see as dead
/// (see [`workspace_verified_gone`]). Widening it is half the fix; the other
/// half is that the daemon's verdict is no longer trusted ON ITS OWN.
///
/// 🔴 There is deliberately NO `if s.unresumable { return true }` short-circuit
/// (PR #4725 review). `is_unresumable` probes with a bare `try_exists`, which
/// answers `Ok(false)` for every path on an unmounted volume — so a daemon can
/// and will flag an entire unplugged external drive's worth of sessions dead. A
/// short-circuit on that flag would hand exactly the scenario
/// [`workspace_verified_gone`]'s parent check exists to prevent straight past
/// the check. Requiring the CLI's own confirmation for EVERY record closes that.
///
/// Dropping the short-circuit costs nothing in coverage: `is_unresumable`
/// requires all three of `last_cwd`/`workspace_path`/`cwd` absent, so any record
/// it flags is one this predicate also finds gone — provided the record carries
/// a path on the wire. A legacy record with neither `workspace_path` nor `cwd`
/// serialized is now kept rather than cleared; unverifiable is not dead.
///
/// What: `is_clearable_state(s, live) && workspace_verified_gone(s)`.
/// Test: the `auto_prune_*` suite in `tests_behavior_d_tests.rs`, in particular
/// `auto_prune_ignores_daemon_unresumable_when_parent_is_unreachable`.
async fn is_dead_record(s: &ManagedSessionSummary, live_tmux_names: &HashSet<String>) -> bool {
    is_clearable_state(s, live_tmux_names) && workspace_verified_gone(s).await
}

/// Auto-prune definitively-dead (`unresumable`) session records at listing
/// time — the testable core (owner request 2026-07-29).
///
/// Why: before this, an operator had to notice the `DEAD: workspace removed`
/// row and manually type `d<N>` for every session whose workspace had been
/// GC-pruned out from under it — Bob's ask was for the picker to just clean
/// these up on its own. The critic's WARN required this to never be a
/// single-observation, uncapped, disk-mutating action — see the module doc's
/// three hardenings.
///
/// SAFETY BOUNDARY (#4344, `114de333`): a record whose worktree removal was
/// previously REFUSED because the tree was dirty keeps its `workspace_path`
/// pointed at a directory that still genuinely exists on disk. `unresumable`
/// (`session_manager::resume_workdir::is_unresumable`) probes that exact
/// path with `tokio::fs::try_exists` and returns `false` the instant ANY
/// candidate (`last_cwd`/`workspace_path`/`cwd`) is found present — so a
/// dirty-retained record can never read `unresumable == true` in the first
/// place, and never enters this function's `dead` partition at all.
///
/// What: partitions `sessions` into `(keep, dead)` by [`is_dead_record`] —
/// the daemon's `unresumable` verdict OR (#4702) a stopped/errored record whose
/// every wire-visible workdir candidate the CLI independently confirmed absent.
/// Any `keep` session that still has a confirmation-marker entry has recovered
/// (its workspace reappeared) — that entry is dropped so a later
/// re-appearance starts a fresh confirmation window. Each `dead` record is
/// then classified against the persisted marker file ([`load_seen`]): a
/// fresh id is recorded as a first sighting (NOT acted on this call); an id
/// already present (seen on a STRICTLY earlier call) is CONFIRMED-eligible.
/// At most [`AUTO_PRUNE_CAP`] confirmed records are decommissioned via
/// [`decommission_dead_record`] — the record-only route, never a raw
/// `fs::remove` or a hand-edited store entry. Everything else (healthy,
/// first-sighting, over-cap, or a failed decommission attempt) is returned
/// in `kept`. The marker file is persisted only when it actually changed.
/// Test: `auto_prune_dead_records_removes_confirmed_unresumable_records`,
/// `auto_prune_dead_records_first_sighting_is_not_pruned`,
/// `auto_prune_dead_records_keeps_workspace_present_records`,
/// `auto_prune_dead_records_is_noop_when_nothing_is_dead`,
/// `auto_prune_dead_records_honors_the_cap`,
/// `auto_prune_dead_records_stale_daemon_sentinel_expires_after_ttl`.
pub(crate) async fn auto_prune_dead_records_at(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<ManagedSessionSummary>,
    marker_path: &Path,
    live_tmux_names: Option<HashSet<String>>,
) -> AutoPruneOutcome {
    // #4702 (PR #4725 review round 2): a `None` live-set means tmux could not be
    // enumerated at all. Without it there is no liveness signal for `errored`
    // records — which the daemon never display-reconciles — so clearing anything
    // could tombstone a running agent into a terminal state. Prune nothing;
    // mirrors `reconcile_against_tmux`'s fail-closed contract.
    let Some(live_tmux_names) = live_tmux_names else {
        return AutoPruneOutcome {
            kept: sessions,
            pruned: 0,
            pending: 0,
        };
    };

    // #4702: the partition predicate is now async (it probes the filesystem for
    // the stopped-record case), so this cannot be `Iterator::partition`.
    let mut dead: Vec<ManagedSessionSummary> = Vec::new();
    let mut kept: Vec<ManagedSessionSummary> = Vec::new();
    for s in sessions {
        if is_dead_record(&s, &live_tmux_names).await {
            dead.push(s);
        } else {
            kept.push(s);
        }
    }

    let mut seen = load_seen(marker_path);
    let mut changed = false;

    // A session that surfaced HEALTHY this call has recovered — clear any
    // stale marker so a later re-appearance of unresumable starts fresh.
    for s in &kept {
        if seen.remove(&s.id).is_some() {
            changed = true;
        }
    }

    if dead.is_empty() {
        if changed {
            save_seen(marker_path, &seen);
        }
        return AutoPruneOutcome {
            kept,
            pruned: 0,
            pending: 0,
        };
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut confirmed = Vec::new();
    let mut first_sighting = Vec::new();
    for s in dead {
        // #4702: the stored first-sighting timestamp must actually be COMPARED,
        // and — equally important — NOT rewritten while the window is running.
        // Presence-only checking let two calls milliseconds apart confirm;
        // unconditional restamping then made the window measure the gap between
        // consecutive listings instead of age since first sighting, so a
        // frequent `tm ls` kept auto-prune permanently inert.
        match classify_sighting(seen.get(&s.id)) {
            Sighting::Confirmed => confirmed.push(s),
            Sighting::TooRecent => {
                // Leave the stamp alone — this is the whole fix.
                first_sighting.push(s);
            }
            Sighting::Absent => {
                seen.insert(s.id.clone(), now.clone());
                changed = true;
                first_sighting.push(s);
            }
        }
    }

    let cap = AUTO_PRUNE_CAP.min(confirmed.len());
    let to_prune: Vec<_> = confirmed.drain(..cap).collect();

    // Critic CRITICAL (2026-07-30): a daemon that already proved it ignores
    // `record_only` — within its 1-hour TTL — must never be asked to
    // decommission again this call. An EXPIRED sentinel is cleared here so a
    // now-current daemon isn't wedged off forever.
    let already_stale_daemon = stale_daemon_sentinel_active(&seen);
    if !already_stale_daemon && seen.remove(STALE_DAEMON_MARKER_KEY).is_some() {
        changed = true;
    }
    let mut newly_stale_daemon = false;
    let mut pruned = 0usize;
    // Owner request 2026-07-30 follow-up: records held back by the
    // stale-daemon gate (already-active OR newly-tripped-mid-loop) must fold
    // into `pending` — otherwise "N more dead records pending confirmation"
    // under-reports exactly the batch this gate is protecting.
    let mut stale_daemon_held = 0usize;

    if already_stale_daemon {
        stale_daemon_held = to_prune.len();
        kept.extend(to_prune);
    } else {
        let mut iter = to_prune.into_iter();
        for s in iter.by_ref() {
            match decommission_dead_record(client, url, &s.id).await {
                DecommissionOutcome::Pruned => {
                    pruned += 1;
                    seen.remove(&s.id);
                    changed = true;
                }
                DecommissionOutcome::Failed => kept.push(s),
                DecommissionOutcome::StaleDaemon => {
                    newly_stale_daemon = true;
                    stale_daemon_held += 1;
                    kept.push(s);
                    break;
                }
            }
        }
        // `iter.by_ref()` leaves every un-visited item (after the `break`)
        // in `iter` — collect them back rather than silently dropping them.
        let remainder: Vec<_> = iter.collect();
        stale_daemon_held += remainder.len();
        kept.extend(remainder);
    }

    if newly_stale_daemon {
        seen.insert(STALE_DAEMON_MARKER_KEY.to_string(), now);
        changed = true;
    }
    if already_stale_daemon || newly_stale_daemon {
        eprintln!(
            "tm: auto-prune: daemon may be running an older build; skipping \
             automatic pruning until it's restarted"
        );
    }

    let pending = confirmed.len() + first_sighting.len() + stale_daemon_held;
    kept.extend(confirmed);
    kept.extend(first_sighting);

    if changed {
        save_seen(marker_path, &seen);
    }

    AutoPruneOutcome {
        kept,
        pruned,
        pending,
    }
}

/// What the persisted marker says about one record's first sighting (#4702).
///
/// Why: the caller must distinguish three cases, not two. Collapsing "no usable
/// stamp" and "stamp present but too recent" into one `false` is what made the
/// window measure the gap between CONSECUTIVE LISTINGS instead of age since
/// first sighting (PR #4725 review): a too-recent entry was restamped with
/// `now` on every call, so any `tm ls` cadence tighter than
/// [`SIGHTING_MIN_AGE_SECS`] reset the clock forever and auto-prune could never
/// fire. On the machine #4702 was filed from, `tm ls` runs far more often than
/// every ten minutes — the feature would have been permanently inert.
enum Sighting {
    /// No entry, or one that does not parse — stamp it and start the window.
    Absent,
    /// A valid stamp, still inside the window. Leave it EXACTLY as it is; any
    /// rewrite here restarts the clock and is the defect described above.
    TooRecent,
    /// A valid stamp at least [`SIGHTING_MIN_AGE_SECS`] old.
    Confirmed,
}

/// Classify a stored first-sighting timestamp (#4702).
///
/// Why: see [`Sighting`]. The confirmation rule is "first seen dead at least
/// [`SIGHTING_MIN_AGE_SECS`] ago", which only holds if the ORIGINAL stamp
/// survives every intervening listing.
/// What: parses `ts` as RFC 3339 and compares its age. A missing or unparseable
/// value is [`Sighting::Absent`] — a corrupt marker must restart the window,
/// never satisfy it.
/// Test: `auto_prune_does_not_confirm_two_calls_in_quick_succession`,
/// `auto_prune_does_not_restamp_a_sighting_still_inside_the_window`,
/// `auto_prune_confirms_once_the_sighting_window_has_elapsed`,
/// `auto_prune_treats_an_unparseable_sighting_as_a_fresh_one`.
fn classify_sighting(ts: Option<&String>) -> Sighting {
    let Some(first) = ts.and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok()) else {
        return Sighting::Absent;
    };
    if chrono::Utc::now().signed_duration_since(first)
        >= chrono::Duration::seconds(SIGHTING_MIN_AGE_SECS)
    {
        Sighting::Confirmed
    } else {
        Sighting::TooRecent
    }
}

/// Whether the persisted `STALE_DAEMON_MARKER_KEY` sentinel is still within
/// its [`STALE_DAEMON_TTL_SECS`] TTL (owner request 2026-07-30 follow-up).
///
/// Why: see [`STALE_DAEMON_TTL_SECS`]'s doc — a permanent lockout would
/// outlive the daemon restart that fixes it.
/// What: `true` only when the sentinel is present AND parses as an RFC 3339
/// timestamp no more than [`STALE_DAEMON_TTL_SECS`] old. A missing, corrupt,
/// or expired sentinel returns `false` (retry allowed) — the caller is
/// responsible for clearing an expired entry from `seen` so it doesn't
/// linger.
/// Test: `auto_prune_dead_records_stale_daemon_sentinel_expires_after_ttl`.
fn stale_daemon_sentinel_active(seen: &HashMap<String, String>) -> bool {
    seen.get(STALE_DAEMON_MARKER_KEY)
        .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .is_some_and(|ts| {
            chrono::Utc::now().signed_duration_since(ts)
                < chrono::Duration::seconds(STALE_DAEMON_TTL_SECS)
        })
}

/// Load the persisted first-sighting map, tolerating a missing or corrupt
/// file (treated as empty — a listing must never fail merely because its
/// confirmation marker couldn't be read).
///
/// What: `{ "<session-id>": "<rfc3339 first-sighting timestamp>" }`.
fn load_seen(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Best-effort persist of the first-sighting map; a write failure is logged
/// to stderr and never propagated (the listing must never fail merely
/// because the marker file couldn't be written).
fn save_seen(path: &Path, seen: &HashMap<String, String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string(seen) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                eprintln!("tm: auto-prune: failed to persist confirmation marker: {e}");
            }
        }
        Err(e) => eprintln!("tm: auto-prune: failed to serialize confirmation marker: {e}"),
    }
}

/// Outcome of one [`decommission_dead_record`] call.
///
/// Why: an HTTP 200 alone is NOT proof that `record_only` was honored — a
/// stale daemon lacking the `Query<DecommissionQuery>` extractor also
/// returns 200 (from the OLD unconditional full-teardown path). Only the
/// response body's `workspace_removed` field can tell the two apart, so the
/// caller needs a THIRD outcome distinct from plain success/failure.
enum DecommissionOutcome {
    /// The daemon confirmed `workspace_removed == false` — genuinely
    /// record-only, safe to count as pruned.
    Pruned,
    /// The response proves (or fails to rule out) that the daemon actually
    /// removed the workspace — `workspace_removed == true`, the field was
    /// missing, or the body didn't even parse as JSON. Only reachable on a
    /// 2xx status; treated as "a stale daemon may have just deleted this,"
    /// never assumed safe.
    StaleDaemon,
    /// Non-2xx status or a transport error — the call simply failed.
    Failed,
}

/// POST the existing `/decommission` route in RECORD-ONLY mode for one
/// confirmed-dead record, best-effort (critic HIGH finding #1; response-body
/// verification added per critic CRITICAL finding, 2026-07-30).
///
/// Why: kept as the single I/O primitive behind [`auto_prune_dead_records_at`]
/// so a per-record transport/HTTP failure can never abort the rest of the
/// listing — one stuck record must not hide every other session behind it.
/// POSTing `?record_only=true` alone is NOT sufficient proof of safety: a
/// daemon process still running a build that predates this route's
/// `Query<DecommissionQuery>` extractor silently ignores the param (axum
/// drops query params a handler doesn't declare) and falls through to the
/// OLD unconditional full-teardown `decommission()` — which also returns
/// 200. This project's daemon is long-lived by design (a CLI upgrade never
/// bounces it), so that version skew is the ROUTINE case, not an edge case.
/// What: POSTs `?record_only=true`; on a 2xx response, parses the JSON body
/// and requires `workspace_removed == false` before returning
/// [`DecommissionOutcome::Pruned`] — the ONLY outcome that proves nothing was
/// deleted. `workspace_removed == true`, a missing field, or an unparseable
/// body all return [`DecommissionOutcome::StaleDaemon`]. A non-2xx status or
/// a transport error is logged to stderr and returns
/// [`DecommissionOutcome::Failed`].
/// Test: `auto_prune_dead_records_removes_confirmed_unresumable_records`
/// drives the happy path through a real loopback daemon;
/// `auto_prune_dead_records_stops_sweep_when_daemon_reports_workspace_removed`
/// drives the stale-daemon path through a stub server that ignores
/// `record_only` and always reports `workspace_removed: true`.
async fn decommission_dead_record(
    client: &reqwest::Client,
    url: &str,
    id: &str,
) -> DecommissionOutcome {
    let resp = match client
        .post(format!("{url}/api/v1/sessions/managed/{id}/decommission"))
        .query(&[("record_only", "true")])
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("tm: auto-prune: failed to remove dead record {id}: {e}");
            return DecommissionOutcome::Failed;
        }
    };
    if !resp.status().is_success() {
        eprintln!(
            "tm: auto-prune: failed to remove dead record {id}: HTTP {}",
            resp.status()
        );
        return DecommissionOutcome::Failed;
    }
    match resp.json::<serde_json::Value>().await {
        Ok(body) => match body.get("workspace_removed").and_then(|v| v.as_bool()) {
            Some(false) => DecommissionOutcome::Pruned,
            _ => DecommissionOutcome::StaleDaemon,
        },
        Err(_) => DecommissionOutcome::StaleDaemon,
    }
}
