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
//!      (`SessionManager::decommission_record_only`), which never touches
//!      the filesystem. A remount between the listing-time probe and this
//!      call can never lose data, because nothing is ever deleted here.
//!   2. **Two-sightings confirmation** — [`auto_prune_dead_records_at`] only
//!      acts on a session id whose `unresumable` flag was ALSO seen on a
//!      strictly earlier call, tracked via a small persisted marker file
//!      ([`load_seen`]/[`save_seen`]). A single-shot blip (one bad listing)
//!      never triggers a prune.
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

use std::collections::HashMap;
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
    prune_and_report_at(client, url, sessions, &default_marker_path()).await
}

/// [`prune_and_report`] with an explicit marker-file path — the testable core.
///
/// Why: a test must never write the operator's real
/// `~/.trusty-mpm/auto-prune-seen.json`; injecting the path keeps every test
/// hermetic in its own tempdir (the same seam [`auto_prune_dead_records_at`]
/// already provides one level down).
/// What: see [`prune_and_report`].
/// Test: `session_ls_prunes_dead_records_on_piped_invocation`.
pub(crate) async fn prune_and_report_at(
    client: &reqwest::Client,
    url: &str,
    sessions: Vec<ManagedSessionSummary>,
    marker_path: &Path,
) -> Vec<ManagedSessionSummary> {
    let outcome = auto_prune_dead_records_at(client, url, sessions, marker_path).await;
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
/// stopped session and a tombstone, so it is deliberately FAIL-CLOSED-toward-
/// KEEPING: `true` requires that at least one candidate path was present on the
/// wire AND every candidate probe came back a definitive `Ok(false)`. A probe
/// `Err` (permission denied, unmounted network volume, transient I/O) counts as
/// "possibly present" and returns `false`, mirroring `is_unresumable`'s own
/// fail-open rule. A record carrying NO path at all is unverifiable and is
/// likewise kept. Measured 2026-08-03: 4 of 5 spot-checked stopped workspaces
/// still existed on disk — those must never be cleared.
/// What: `tokio::fs::try_exists` over [`workdir_candidates`].
/// Test: `auto_prune_clears_stopped_record_whose_workspace_is_gone`,
/// `auto_prune_keeps_stopped_record_whose_workspace_still_exists`.
async fn workspace_verified_gone(s: &ManagedSessionSummary) -> bool {
    let mut probed_any = false;
    for candidate in workdir_candidates(s).into_iter().flatten() {
        probed_any = true;
        match tokio::fs::try_exists(candidate).await {
            // Definitively present, or the probe could not tell — either way,
            // never treat this workspace as gone.
            Ok(true) | Err(_) => return false,
            Ok(false) => continue,
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
/// (`active`, `provisioning`, `attached`) is untouchable by construction.
/// What: the DISPLAY state must be `stopped`/`errored`, the row must not be a
/// slot tombstone, and the PERSISTED state (when the daemon sends it) must not
/// already be terminal.
/// Test: `auto_prune_never_touches_a_running_record`,
/// `auto_prune_never_touches_a_decommissioned_record`.
fn is_clearable_state(s: &ManagedSessionSummary) -> bool {
    if s.deleted || !matches!(s.state.as_str(), "stopped" | "errored") {
        return false;
    }
    !matches!(
        s.persisted_state.as_deref(),
        Some("decommissioned" | "deleted")
    )
}

/// The full "this record is definitively dead" predicate (#4702).
///
/// Why: `unresumable` alone missed the majority of what operators see as dead
/// (see [`workspace_verified_gone`]). Widening it here — rather than at the
/// partition site — keeps the two conditions readable as one contract: the
/// daemon's verdict, OR a stopped-class record the CLI independently confirmed
/// has no workdir left.
/// What: `s.unresumable || (is_clearable_state(s) && workspace_verified_gone(s))`.
/// Test: the `auto_prune_*` suite in `tests_behavior_d_tests.rs`.
async fn is_dead_record(s: &ManagedSessionSummary) -> bool {
    if s.unresumable {
        return true;
    }
    is_clearable_state(s) && workspace_verified_gone(s).await
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
) -> AutoPruneOutcome {
    // #4702: the partition predicate is now async (it probes the filesystem for
    // the stopped-record case), so this cannot be `Iterator::partition`.
    let mut dead: Vec<ManagedSessionSummary> = Vec::new();
    let mut kept: Vec<ManagedSessionSummary> = Vec::new();
    for s in sessions {
        if is_dead_record(&s).await {
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
        if seen.contains_key(&s.id) {
            confirmed.push(s);
        } else {
            seen.insert(s.id.clone(), now.clone());
            changed = true;
            first_sighting.push(s);
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
