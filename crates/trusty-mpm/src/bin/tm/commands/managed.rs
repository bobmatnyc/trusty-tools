//! Managed session-manager CLI handlers — the verbs NOT routed through chat-core.
//!
//! Why: Phase 1B (refs #1283) routed the managed operator verbs
//! (`new`/`ls`/`send`/`answer`/`attach`/`stop`/`resume`/`decommission`) through
//! the shared chat-core layer (`commands::managed_route`), retiring this file's
//! bespoke resolvers (`classify_managed_target`/`resolve_managed_id`). What
//! remains here are the handlers chat-core does not (yet) cover faithfully: the
//! `--json` `ls` raw passthrough, the token/cache-rich `activity` render, the
//! id-direct `stop`/`resume`/`decommission` helpers reused by the managed-aware
//! `Stop`/`Resume` verbs and `prune`, the `deprecation_*` helpers, and the local
//! `catalog` command. Keeping them in their own file keeps `session.rs` under the
//! SLOC cap.
//! What: thin async functions that issue HTTP requests via `reqwest` and render
//! the JSON responses; plus the local `catalog` handler that drives `CatalogSync`.
//! Test: `cli_parses_catalog_sync` exercises the parse path; the HTTP round-trip
//! is covered by `tests/session_manager_mvp.rs`.
//!
//! `"deleted"` slot tombstones vs. `"decommissioned"` sessions (issue #3034):
//! these are two deliberately different states. A `"decommissioned"` session
//! still has a live record in the store (soft-retired) and stays hidden from
//! the default view per #1809 — that filter is unchanged here. A `"deleted"`
//! slot tombstone is a stable-numbering placeholder for a session whose record
//! has left the store entirely (hard-delete, decommission-reap, or prune); per
//! Bob's explicit directive on #3034, tombstones are INTENTIONALLY shown in the
//! default `tm ls`/picker view at their original slot position — hiding them
//! would defeat the stable-numbering feature's whole point (an operator must
//! see exactly why a captured number no longer resolves, not have it silently
//! vanish). This is NOT unbounded growth: the slot registry is in-memory only
//! and resets to empty on every daemon restart (see `SlotRegistry`'s doc), so
//! tombstone count is bounded by daemon process lifetime, not by history.

use std::io::IsTerminal as _;

use trusty_mpm::client::ManagedSessionSummary;

use super::managed_route::decommission_message;
use crate::cli::CatalogAction;

/// Build the one-line deprecation message for a renamed CLI verb.
///
/// Why: splitting message construction from the stderr write makes the wording
/// unit-testable without capturing process stderr (#1205).
/// What: returns `warning: '<old>' is deprecated; use '<new>'`.
/// Test: `deprecation_notice_format` in `tests.rs` asserts the exact text.
pub(crate) fn deprecation_message(old: &str, new: &str) -> String {
    format!("warning: '{old}' is deprecated; use '{new}'")
}

/// Emit a one-line deprecation notice to stderr for a renamed CLI verb.
///
/// Why: the verbose managed-lifecycle verbs (`runtime-stop`, `managed-resume`,
/// `managed-stop`) were renamed to the cleaner `stop`/`resume`/`decommission`
/// family (#1205). The old spellings still parse for backward compatibility, but
/// every invocation must nudge the operator toward the canonical verb so the
/// aliases can eventually be retired.
/// What: writes `deprecation_message(old, new)` to stderr, leaving stdout clean
/// for scriptable output.
/// Test: `cli_parses_session_runtime_stop`/`_managed_resume` assert the aliases
/// still parse; the message text is asserted by `deprecation_notice_format`.
pub(crate) fn deprecation_notice(old: &str, new: &str) {
    eprintln!("{}", deprecation_message(old, new));
}

/// Return true when a session row should be shown in the default picker/list view.
///
/// Why: four directives have landed on this one predicate. Every one of them is
/// a VISIBILITY rule — no record is deleted, decommissioned, or mutated here,
/// and `tm ls --all` still lists everything this returns `false` for.
///   * #1809 — a `"decommissioned"` record is soft-retired and never belongs in
///     the default view.
///   * #3302 (`51243ea5`, a code-critic CRITICAL) — a real, still-in-store
///     record soft-deleted via `tm sessions delete` (#2012) carries wire
///     `state == "deleted"` too, and surfacing it risked the zombie-reconcile
///     path resurrecting it.
///   * #4994 — a record whose tmux pane is gone AND whose workspace directory
///     no longer exists anywhere on disk took the catch-all arm into the
///     default view. Six such rows sat in the owner's `tm ls`, each already
///     rendered `[dead]`.
///   * #5952 — a #3034 numbered-slot tombstone (the placeholder
///     `daemon::managed_routes::summary::tombstone_summary` emits for every
///     slot the registry still holds whose record has left the store,
///     `ManagedSessionSummary::deleted == true`) was the one row that still
///     passed through. At 44 tombstones in a 130-row listing it was a third of
///     the table, so it now follows the same `--all` opt-in as the other three.
///
/// #5952 is why this predicate no longer takes an `is_slot_tombstone` input:
/// that flag existed only to tell the slot tombstone apart from the
/// soft-deleted record so the two could be shown differently, and both are now
/// hidden by default and shown by `--all`.
///
/// 🔴 #3034's no-silent-reuse guarantee is untouched. The slot registry, slot
/// release, and `SessionManager::numbered_snapshot` are unchanged, so a slot
/// number is still never reused and never reassigned; a hidden slot still
/// leaves a visible gap in the table's NUM column; and `--all` still renders
/// the tombstone at its ORIGINAL slot position, because
/// [`crate::commands::session_picker::scope_for_display`] deliberately excludes
/// tombstones from its sink-to-bottom sort. What #5952 changes is only whether
/// the placeholder occupies a row in the DEFAULT table.
///
/// 🔴 `classified_dead` is the listing-time sweep's own verdict
/// ([`AutoPruneOutcome::dead_ids`](crate::commands::session_picker_prune::AutoPruneOutcome::dead_ids)),
/// NEVER the wire's `unresumable` flag. The flag is computed daemon-side from a
/// record's PERSISTED state and `reconcile_live_state` then overwrites the
/// DISPLAY state without recomputing it, so the wire ships `state: "active"`
/// with `unresumable: true` for a session whose pane is running right now —
/// reachable any time a daemon restart resets records to persisted `Stopped`
/// and this repo's own post-merge `git worktree remove` takes the workspace
/// while the pane survives. Hiding on the flag would hide that session, and the
/// prune would then refuse to reap it (`is_clearable_state`'s `stopped|errored`
/// gate; the `live_tmux_names` guard from PR #4725 round 2) — hidden AND
/// unreapable, which is precisely the "running agent unreachable through `tm`"
/// outcome those guards exist to prevent. Keying off the classification instead
/// makes `hidden ⟺ prunable` hold by construction.
///
/// Resurrection-safety for a tombstone the operator DOES see (under `--all`)
/// never depended on this predicate: the picker's `decide_for_index` checks
/// `ManagedSessionSummary::deleted` ahead of every other branch and returns
/// `PickerDecision::SlotDeleted`, and `guided_resume`'s terminal-state refusal
/// (`plan_resume` → `ResumeAction::Terminal`) refuses to attach to or restart
/// any `"decommissioned"`/`"deleted"` session before any daemon round-trip.
/// What: returns `false` for `"decommissioned"`, for `"deleted"` (both the
/// soft-deleted record and the #3034 slot tombstone, #5952), and for any other
/// state the sweep classified dead (#4994); returns `true` otherwise.
/// Test: `picker_filter_excludes_decommissioned_keeps_active`,
/// `is_live_session_state_excludes_soft_deleted_record`,
/// `is_live_session_state_hides_slot_tombstone_from_default_view`,
/// `is_live_session_state_hides_a_record_the_prune_classified_dead`,
/// `is_live_session_state_keeps_resumable_stopped_record`,
/// `scope_for_display_hides_tombstone_by_default_and_all_shows_it` in
/// `tests_behavior_e_tests.rs`.
pub(crate) fn is_live_session_state(state: &str, classified_dead: bool) -> bool {
    match state {
        // #5952: "deleted" covers BOTH the soft-deleted in-store record (#3302)
        // and the #3034 slot tombstone; `--all` is the way to see either one.
        "decommissioned" | "deleted" => false,
        // #4994: the sweep confirmed no pane and no workspace on disk, so this
        // row is hidden from the default view exactly like `decommissioned`.
        _ => !classified_dead,
    }
}

/// Filter a session list to only live sessions for display in the picker (#1809).
///
/// Why: the picker must never show decommissioned tombstones (or a
/// soft-deleted, still-in-store record, or — since #4994 — a record the
/// listing-time sweep classified dead, or — since #5952 — a #3034 numbered-slot
/// tombstone) by default; the `--all` opt-in re-enables all four for `tm ls` and
/// `tm session ls` via this module's path. See [`is_live_session_state`]'s doc.
///
/// 🔴 `dead_ids` must come from the SAME sweep that just ran over this list —
/// see [`crate::commands::session_picker::scope_for_display`]. That forces two
/// things at once: the filter runs AFTER the prune (dropping a dead row
/// upstream would starve the sweep of the records it exists to reap, so they
/// would accumulate in the store forever, now invisibly), and it hides only
/// what the sweep is willing to reap.
/// What: retains only sessions whose `(state, dead_ids-membership)` pair passes
/// [`is_live_session_state`].
/// Test: `picker_filter_excludes_decommissioned_keeps_active`,
/// `picker_filter_hides_only_what_the_prune_classified_dead`,
/// `picker_filter_hides_slot_tombstone_and_soft_deleted_record`,
/// `picker_filter_keeps_a_live_row_flagged_unresumable`.
pub(crate) fn filter_live_sessions(
    sessions: Vec<ManagedSessionSummary>,
    dead_ids: &std::collections::HashSet<String>,
) -> Vec<ManagedSessionSummary> {
    sessions
        .into_iter()
        .filter(|s| is_live_session_state(&s.state, dead_ids.contains(&s.id)))
        .collect()
}

/// `tm session ls` — list managed sessions.
///
/// Why: operators need a quick view of every managed session and its pending
/// decision, optionally scoped to a single project so the firehose is tamed.
/// What: GETs `/api/v1/sessions/managed` (with an optional `?source_id=` filter
/// that the daemon already supports) and prints a table with id, state, name,
/// task (truncated to 30 chars), and created_at; or raw JSON with `--json`.
/// By default, decommissioned tombstone sessions (#1809), dead `unresumable`
/// records (#4994), and deleted-slot tombstones (#5952) are hidden from the
/// table; `--all` (i.e. `all=true`) opts in to the full unfiltered list. The
/// `--json` path always returns the raw daemon response unfiltered — a script
/// parsing it sees every row it saw before #5952.
/// The source_id filter is passed straight through as a query parameter rather
/// than doing client-side filtering so callers get the daemon's authoritative view.
/// `sort`/`term` (#3483 — the `tm ls [recent|alpha] [filter]` inline
/// grammar) apply ONLY to the table path; `--json` stays a raw, unfiltered,
/// unsorted passthrough, matching `--all`'s existing "no effect on --json"
/// precedent.
///
/// #4702: this path — every piped, scripted, and `--json` listing — runs the
/// same dead-record auto-prune the interactive picker runs. It previously ran
/// none, which is why dead records accumulated without bound for anyone not
/// using the TTY picker. See
/// [`session_picker_prune::prune_and_report`](crate::commands::session_picker_prune::prune_and_report)
/// for why doing this by default is safe.
///
/// #5950: a listing verb that mutates surprised an operator taking a fleet
/// census, so `no_prune` (`tm ls --no-prune`, `tm sessions ls --no-prune`)
/// turns it off for one invocation and the default is stated in the `ls`
/// help rather than only in this doc comment. The prune's own operator lines
/// go to STDERR (`prune_and_report_at`), so `--json` stdout is parseable JSON
/// with or without the flag.
/// Test: HTTP path covered by the integration test; filter logic unit-tested by
/// `ls_source_id_filter_selects_correct_slug`,
/// `picker_filter_excludes_decommissioned_keeps_active`; sort/filter logic by
/// `sort_sessions_*` / `filter_sessions_by_term_*` in `session_picker.rs`; the
/// prune coverage by `session_ls_prunes_dead_records_on_piped_invocation` and
/// `session_ls_json_passthrough_prunes_dead_records`; the `attached` filter by
/// `filter_attached_*`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn session_ls(
    client: &reqwest::Client,
    url: &str,
    json: bool,
    source_id: Option<&str>,
    all: bool,
    attached: bool,
    sort: crate::commands::session_picker::SessionSortArg,
    term: Option<crate::commands::session_picker::SessionFilter>,
    no_prune: bool,
) -> anyhow::Result<()> {
    // #5950: `--no-prune` is the operator's explicit "this read must not
    // mutate" — it selects a context that sweeps nothing.
    let ctx = if no_prune {
        crate::commands::session_picker_prune::PruneContext::disabled()
    } else {
        crate::commands::session_picker_prune::PruneContext::production()
    };
    session_ls_at(
        client, url, json, source_id, all, attached, sort, term, &ctx,
    )
    .await
}

/// [`session_ls`] with an injected auto-prune context — the testable core
/// (#4702).
///
/// Why: a test must never write the operator's real
/// `~/.trusty-mpm/auto-prune-seen.json`, and must never depend on whether the
/// machine running it has tmux. Passing a
/// [`PruneContext`](crate::commands::session_picker_prune::PruneContext) injects
/// both — CI caught the second half when these tests, reaching the real tmux
/// enumeration, passed locally and failed on a runner with no tmux.
/// What: see [`session_ls`]. The `--json` branch echoes the ORIGINAL response
/// body and never re-GETs (PR #4725 review). A re-GET looked like it kept the
/// output fresh but did neither job well: the raw passthrough is unfiltered, so
/// a just-pruned row came back with `state` flipped to `"decommissioned"` rather
/// than absent, and the second fetch's `?` could fail the command AFTER the
/// registry had already been mutated — the caller would see an error and have no
/// idea the prune landed. `--json` is a point-in-time snapshot of the fleet;
/// a prune triggered by this invocation shows up on the next one. A response the
/// client cannot parse degrades to a plain passthrough with no prune: the raw
/// echo is the contract there, the prune is a best-effort side task.
///
/// #4994: the pipeline is parse → prune → scope. The prune runs on the FULL
/// deserialized list, and
/// [`scope_for_display`](crate::commands::session_picker::scope_for_display)
/// applies the default-view filter afterwards. Scoping first would hide dead
/// rows from the sweep that reaps them.
/// Test: `session_ls_prunes_dead_records_on_piped_invocation`,
/// `session_ls_json_passthrough_prunes_dead_records`,
/// `session_ls_json_never_refetches_after_pruning`; the scoping half by
/// `picker_filter_hides_only_what_the_prune_classified_dead` and
/// `scope_for_display_all_keeps_dead_record_visible`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn session_ls_at(
    client: &reqwest::Client,
    url: &str,
    json: bool,
    source_id: Option<&str>,
    all: bool,
    attached: bool,
    sort: crate::commands::session_picker::SessionSortArg,
    term: Option<crate::commands::session_picker::SessionFilter>,
    ctx: &crate::commands::session_picker_prune::PruneContext,
) -> anyhow::Result<()> {
    // Fetch the response body ONCE via the shared fetch path. `--json` echoes
    // that raw text verbatim (byte-for-byte — preserving exact field
    // order/whitespace for scripts); the table path deserializes the SAME text
    // rather than issuing a second GET.
    let raw = crate::commands::session_picker::fetch_managed_raw(client, url, source_id).await?;
    let parsed = crate::commands::session_picker::parse_managed_sessions(&raw);
    if json {
        // Raw JSON passthrough is always unfiltered/unsorted — scripts rely on
        // byte-for-byte. #4702: prune as a side effect, then echo the body we
        // ALREADY have. No re-GET — see this function's doc.
        if let Ok(sessions) = parsed {
            // `--json` echoes the raw body, dead rows included, so the banner
            // must not claim anything was hidden (#4994).
            crate::commands::session_picker_prune::prune_and_report_at(
                client, url, sessions, ctx, false,
            )
            .await;
        }
        println!("{raw}");
        return Ok(());
    }
    let listing =
        crate::commands::session_picker_prune::prune_and_report_at(client, url, parsed?, ctx, !all)
            .await;
    // #4994: scope AFTER the prune, never before — and against that sweep's own
    // verdict, so the rows hidden here are exactly the rows it will reap.
    let sessions = crate::commands::session_picker::scope_for_display(
        listing.sessions,
        all,
        &listing.dead_ids,
    );
    let sessions =
        crate::commands::session_picker::filter_sessions_by_term(sessions, term.as_ref());
    // `tm ls -a`: keep only what a tmux client is on RIGHT NOW.
    let mut sessions = filter_attached(sessions, attached);
    crate::commands::session_picker::sort_sessions(&mut sessions, sort);
    if attached && sessions.is_empty() {
        // An empty table under `-a` is a real answer, not a failure — say so
        // explicitly and exit 0 rather than printing a bare header.
        crate::commands::managed_render::render_no_attached_sessions(source_id);
        return Ok(());
    }
    crate::commands::managed_render::render_session_table(&sessions, source_id);
    Ok(())
}

/// Keep only sessions with a tmux client attached (`tm ls -a`).
///
/// Why: "attached" and "running" are different states, and only the first
/// answers "which session am I looking at?". A session whose tmux entity is
/// live but has no client is reported by the daemon as `active` with
/// `attached == false` — this filter must drop it, or `-a` degrades into a
/// second spelling of the default listing.
/// What: `attached_only == false` returns `sessions` untouched (so every
/// caller that does not pass `-a` is byte-identically unaffected); otherwise
/// keeps exactly the summaries whose `attached` flag is set. That flag
/// originates in tmux's `#{session_attached}`, reconciled by the daemon's list
/// endpoint — it is never inferred from the lifecycle `state` word.
/// Test: `filter_attached_keeps_only_attached`,
/// `filter_attached_excludes_running_but_detached`,
/// `filter_attached_disabled_is_noop`.
pub(crate) fn filter_attached(
    sessions: Vec<trusty_mpm::client::ManagedSessionSummary>,
    attached_only: bool,
) -> Vec<trusty_mpm::client::ManagedSessionSummary> {
    if !attached_only {
        return sessions;
    }
    sessions.into_iter().filter(|s| s.attached).collect()
}

// The `tm ls` table renderer — `render_session_table` and its pure row/column
// formatters — lives in the sibling `commands::managed_render` module (this
// file is at the 500-SLOC production cap).

/// `tm session activity <id>` — inspect a managed session's activity state.
///
/// Why: inspect what a session is doing without attaching; the raw pane is
/// always returned for the calling agentic process to reason over. The LLM
/// classification is shown when available (OpenRouter key set); when absent,
/// `classification: null` and the raw pane are still returned with no error.
/// A truly missing id is a genuine failure (#2457) — printing "not found" and
/// returning `Ok(())` let a script/CI check treat a nonexistent id as success,
/// so this now returns `Err` (non-zero exit) instead.
/// What: GETs `/api/v1/sessions/managed/{id}/activity` and prints the raw pane,
/// structured state, classification (or "no classifier"), and pending decision;
/// a 404 bails with an error instead of printing "not found".
/// Test: HTTP path covered by the integration test;
/// `session_activity_not_found_errors` covers the #2457 exit-code fix.
pub(crate) async fn session_activity(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    // The raw request is retained here (rather than the typed `DaemonClient`
    // method) only to preserve the 404 → "not found" output contract; the
    // response body is deserialized into the SHARED `ManagedActivityResponse`,
    // dropping the former ad-hoc local struct.
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed/{id}/activity"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("managed session '{id}' not found");
    }
    let a: trusty_mpm::client::ManagedActivityResponse = resp.error_for_status()?.json().await?;
    let runtime_str = if a.runtime_active {
        "running"
    } else {
        "stopped"
    };
    println!("runtime:    {runtime_str}");
    println!("state:      {} (confidence: {:.2})", a.state, a.confidence);
    println!("summary:    {}", a.summary);
    let classification_str = a
        .classification
        .as_deref()
        .unwrap_or("(no classifier — raw pane available for agentic inference)");
    println!("classification: {classification_str}");
    let cache = if a.cache_hit { "hit" } else { "miss" };
    println!(
        "cache:      {} | tokens: in={} out={} | latency: {}ms",
        cache, a.input_tokens, a.output_tokens, a.latency_ms
    );
    println!(
        "total:      in={} out={}",
        a.total_input_tokens, a.total_output_tokens
    );
    if let Some(pending) = &a.pending_decision {
        println!("pending decision: {pending}");
        if let Some(default) = &a.proposed_default {
            println!("  proposed default: {default}");
        }
    }
    if !a.raw_pane.is_empty() {
        // #1840: when the runtime is stopped and the live capture was empty, the
        // daemon falls back to the stop-time scrollback snapshot. Display a clear
        // annotation so operators know they're reading last-known-good data.
        if a.pane_stale {
            println!("--- pane content (stop-time scrollback snapshot — not real-time) ---");
        } else {
            println!("--- raw pane (last 60 lines) ---");
        }
        println!("{}", a.raw_pane);
    }
    Ok(())
}

/// `tm session stop <id>` — stop the runtime of a managed session, keep the workspace.
///
/// Why: a session ENDURES beyond its runtime; `stop` kills only the tmux session
/// and claude process, preserving the workspace for later `resume`. Renamed from
/// the verbose `runtime-stop` in #1205 (which remains a deprecated alias). A
/// missing id is a genuine failure to stop the requested session (#2457) —
/// printing "not found" and returning `Ok(())` let a script/CI check treat it
/// as success, so this now returns `Err` (non-zero exit) instead. `prune.rs`'s
/// bulk teardown loop (the only other caller) already propagates this `Err`
/// with `?`, matching its established fail-closed convention (#1508).
/// What: POSTs `/api/v1/sessions/managed/{id}/runtime-stop`; a 404 bails with
/// an error instead of printing "not found".
/// Test: HTTP path covered by the integration test; parse by
/// `cli_parses_session_managed_stop_verb`; `session_stop_not_found_errors`
/// covers the #2457 exit-code fix.
pub(crate) async fn session_stop(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/runtime-stop"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("managed session '{id}' not found");
    }
    resp.error_for_status()?;
    println!("runtime stopped {id} (workspace intact; use 'resume' to restart)");
    Ok(())
}

/// `tm session resume <id>` — resume a stopped managed session in its existing
/// workspace AND hand the terminal over to it.
///
/// Why (#2649): after `stop`, the workspace is still on disk; `resume`
/// re-spawns the runtime there without re-cloning. Renamed from the verbose
/// `managed-resume` in #1205 (which remains a deprecated alias). Before #2649
/// this handler only printed a status line after the daemon-side resume — the
/// operator's terminal never moved into the resumed session's tmux window,
/// unlike the bare-`tm` guided picker's resume path (fixed across #1742/#2001/
/// #2148/#2453/#2456/#2467). Rather than re-implement that whole state
/// machine, this now fetches the session record and delegates to the SAME
/// [`crate::commands::guided_resume::resume_guided_session`] helper the picker
/// uses, so the two entry points ("resume a session I just picked" and
/// "resume a session I already know the id of") can never drift apart again —
/// the exact gap that let this bug survive five rounds of fixes to the
/// sibling path. That helper also ports the #2001 zombie/stale-Active
/// auto-reconcile: a session the daemon still marks active/provisioning but
/// whose tmux pane is gone is auto-stopped then restarted, rather than
/// dead-ending on a 409. A missing id is still a genuine failure to perform
/// the requested resume (#2457) — bailing instead of printing a message and
/// returning `Ok(())`.
/// What: GETs `/api/v1/sessions/managed/{id}` (a 404 bails with an error); on
/// success, hands the resulting [`ManagedSessionSummary`] to
/// [`crate::commands::guided_resume::resume_session`], which picks the right
/// action (attach directly / restart via the daemon `/resume` endpoint /
/// auto-reconcile a zombie then restart) purely from the record's state and
/// live tmux liveness. When stdin is a real terminal, it then attaches (or
/// `switch-client`s, when already inside tmux) the caller's terminal into the
/// session's tmux window; otherwise (code-critic HIGH, #2649 review — a
/// headless/scripted invocation has no controlling terminal to move) it
/// still performs the daemon-side restart/reconcile but skips the attach and
/// prints the resulting `resumed <name> (<id>) [<state>]` status line
/// instead, returning `Ok(())` on daemon-side success either way.
///
/// #2649 review, PM-accepted UX change: an `Active` session with a live tmux
/// pane now resolves to `ResumeAction::Attach` — no `/resume` POST at all,
/// just an idempotent (re)attach — rather than the pre-#2649 behavior of
/// bailing with the daemon's raw 409 ("cannot resume a session in state
/// 'active'"). This makes `tm session resume <id>` idempotent for an
/// already-running session: "resume" now means "get me into this session",
/// matching the guided picker's long-standing behavior, instead of treating
/// an already-active session as a usage error.
/// Test: HTTP path covered by the integration test; parse by
/// `cli_parses_session_managed_resume_verb`; `session_resume_not_found_errors`
/// covers the #2457 exit-code fix for the 404 branch;
/// `session_resume_restart_failure_errors` (in `managed_tests.rs`, #2649)
/// covers the same non-swallowing guarantee for a daemon-rejected restart now
/// that this handler routes through the shared restart/attach helper instead
/// of POSTing `/resume` directly; `session_resume_headless_active_live_tmux_skips_restart_and_attach`
/// (`managed_tests.rs`, #2649) proves the new Active+live-tmux idempotent-attach
/// UX never issues a `/resume` POST (asserted via the record's state staying
/// unchanged) and that headless mode exits `Ok(())` without attempting a real
/// tmux attach; `plan_resume`/`needs_restart`/`is_zombie` in
/// `guided_resume.rs`'s own tests (including
/// `guided_resume_plan_active_live_tmux_attaches`) exhaustively cover the
/// branch selection this handler now shares.
pub(crate) async fn session_resume(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .get(format!("{url}/api/v1/sessions/managed/{id}"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!("managed session '{id}' not found");
    }
    let session: ManagedSessionSummary = resp.error_for_status()?.json().await?;
    // #2649 review (code-critic HIGH): gate the terminal hand-off on actually
    // having a terminal — a headless/scripted caller (no stdin TTY) still gets
    // the daemon-side restart/reconcile, just never a doomed real tmux attach.
    let no_attach = !std::io::stdin().is_terminal();
    crate::commands::guided_resume::resume_session(client, url, &session, no_attach).await?;
    Ok(())
}

/// Decommission `id` and return the one line the CLI reports for it.
///
/// Why (#5913): this used to be a second, hand-rolled `reqwest` POST to the
/// decommission endpoint, independent of the routed
/// `tm session decommission <id>` verb. Two paths to one endpoint is what let
/// #5899's wording divergence happen, and the routing carried a second
/// divergence with it: only this path repaired the base repo's worktree
/// bookkeeping. Both now call
/// [`CommandExecutor::decommission_managed_id`](trusty_mpm::client::CommandExecutor::decommission_managed_id).
/// Splitting the line's construction from its `println!` is the same pattern
/// [`deprecation_message`] uses, and it is what lets
/// `decommission_entry_points_agree_on_every_verdict` compare this path's output
/// against the routed one without capturing process stdout.
/// What: calls the shared implementation with the canonical id the caller
/// already holds — no fuzzy resolution, so the bulk sweep still costs one
/// request per session — and renders the verdict through
/// [`super::managed_route::decommission_message`], the same renderer
/// [`super::managed_route::render_cli`] uses. A missing id propagates the shared
/// transport's `Err` (#2457); `prune.rs`'s sweep records that as a failed row.
/// Test: `decommission_entry_points_agree_on_every_verdict`;
/// `session_decommission_not_found_errors`.
async fn session_decommission_line(
    client: &reqwest::Client,
    url: &str,
    id: &str,
) -> anyhow::Result<String> {
    let outcome = super::managed_route::executor(client, url)
        .decommission_managed_id(id)
        .await?;
    Ok(decommission_message(
        &outcome.summary.id,
        outcome.workspace_removed,
    ))
}

/// `tm session decommission <id>` — full teardown (may or may not remove workspace).
///
/// Why: an adopted or local-path workspace was never tm's to delete, a worktree
/// holding unsaved work is refused, and a removal can fail — so decommission
/// often leaves the workspace on disk, and only the daemon knows which happened.
/// This handler reports that verdict rather than assuming removal.
/// What: prints [`session_decommission_line`]'s rendering of the daemon's
/// verdict. The daemon call and the base-repo worktree cleanup both live in the
/// shared implementation that helper calls (#5913).
/// A missing id is a genuine failure to decommission the requested session
/// (#2457) — printing "not found" and returning `Ok(())` let a script/CI
/// check treat it as success, so this returns `Err` (non-zero exit) instead.
/// `prune.rs`'s bulk teardown loop (the only other caller) catches that `Err`
/// and records the row as failed, matching its established fail-closed
/// convention (#1508).
/// Test: `session_decommission_prints_daemon_verdict_over_http`;
/// `session_decommission_not_found_errors` covers the #2457 exit-code fix; the
/// wording itself is covered by `decommission_message_honours_every_verdict`.
pub(crate) async fn session_decommission(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    println!("{}", session_decommission_line(client, url, &id).await?);
    Ok(())
}

// #2012: `tm session delete` lives in the sibling `commands::delete` module —
// this file is at the 500-SLOC production cap, mirroring the pattern used to
// keep `session_manager`'s files under the same cap (`adopt.rs`/`decommission.rs`/
// `prune.rs`/`delete.rs`).

/// `tm session decommission-ephemeral` — bulk-tear-down every ephemeral session (#1508).
///
/// Why: e2e harnesses and operators need a one-shot "clean up all my throwaway
/// test sessions" verb. REAL sessions default `ephemeral=false` and are
/// unreachable, so durable work is never harmed.
/// What: POSTs `/api/v1/sessions/managed/decommission-ephemeral` and prints the
/// returned `decommissioned` count.
/// Test: HTTP path covered by `decommission_ephemeral_route_tears_down_only_ephemeral`
/// in tests/session_manager_mvp.rs; CLI parse by `cli_parses_session_decommission_ephemeral`.
pub(crate) async fn session_decommission_ephemeral(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!(
            "{url}/api/v1/sessions/managed/decommission-ephemeral"
        ))
        .send()
        .await?;
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    let count = body
        .get("decommissioned")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    println!("decommissioned {count} ephemeral session(s)");
    Ok(())
}

/// `tm session prune --state <filter> [--dry-run] [--include-active]` — by-state prune (#1508).
///
/// Why: ONE tool to tear down ephemeral/stopped sessions AND compact the store by
/// dropping decommissioned tombstones, so legacy stale records can be purged with
/// the same verb that cleans up test sessions. The fail-closed default never
/// touches a RUNNING session unless `--include-active` is passed.
/// What: POSTs `/api/v1/sessions/managed/prune` with `{ state, dry_run,
/// include_active }`; on success prints one line per affected session
/// (`<action> <id> <name> [<prior-state>]`) and a summary count. A 400 (bad
/// `--state`) prints the daemon's actionable error.
/// Test: HTTP path covered by `prune_route_dry_run_reports` /
/// `prune_route_rejects_bad_state` in tests/session_manager_mvp.rs; CLI parse by
/// `cli_parses_session_prune`.
pub(crate) async fn session_prune(
    client: &reqwest::Client,
    url: &str,
    state: String,
    dry_run: bool,
    include_active: bool,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/prune"))
        .json(&serde_json::json!({
            "state": state,
            "dry_run": dry_run,
            "include_active": include_active,
        }))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::BAD_REQUEST {
        // Fail closed: a bad `--state` is a usage error, so surface it on STDERR
        // and return an error → non-zero exit, so scripts/CI don't silently
        // "succeed" on a rejected prune (#1508 review fix).
        let msg = resp.text().await.unwrap_or_default();
        eprintln!("error: {msg}");
        return Err(anyhow::anyhow!("prune rejected: {msg}"));
    }
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    let sessions = body
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for s in &sessions {
        let action = s
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let id = s
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let name = s
            .get("tmux_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let prior = s
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        println!("{action} {id} {name} [{prior}]");
        // #4344 review: `decommissioned_worktree_retained` means the record
        // was tombstoned but its in-project worktree held unsaved work and
        // was deliberately NOT deleted (see `worktree_safety::inspect_dirt`).
        // Without a distinct, visible line here, this printed identically to
        // an ordinary `decommissioned` row and the refusal was invisible at
        // the one surface an operator actually reads.
        if action == "decommissioned_worktree_retained" {
            let retained_path = s
                .get("retained_workspace_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            println!("  ! worktree retained (dirty, not deleted): {retained_path}");
        }
    }
    let verb = if dry_run { "would prune" } else { "pruned" };
    println!("{verb} {} session(s) (filter={state})", sessions.len());
    Ok(())
}

/// `tm catalog` — sync or list the claude-mpm agent/skill catalog.
///
/// Why: the session-manager MVP deploys agents/skills from the claude-mpm repo;
/// this command keeps the local cache current and lists what is available.
/// What: `Sync` drives `CatalogSync::sync`; `Ls` lists cached agents and skills.
/// Catalog operations are local (no daemon round-trip).
/// Test: `cli_parses_catalog_sync`, `cli_parses_catalog_ls`.
pub(crate) async fn catalog(action: CatalogAction) -> anyhow::Result<()> {
    // Resolve the framework root (`~/.trusty-mpm`) and build the CatalogSync from
    // the SHARED `for_framework` constructor so this CLI roots the catalog at the
    // exact same `<framework>/catalog` checkout `session_launch` reads its manifest
    // from (no independent `home/.trusty-mpm/catalog` recompute). Honour the
    // `[manifest]` config catalog-source overrides (HR-2): config > env > default.
    let framework_root = trusty_mpm::core::paths::FrameworkPaths::default().root;
    let config = trusty_mpm::core::config::MpmConfig::load_default();
    let sync = trusty_mpm::content::CatalogSync::for_framework(
        trusty_mpm::provisioner::RealGitBackend::default(),
        &framework_root,
        Some(&config.manifest),
    );
    match action {
        CatalogAction::Sync { force } => {
            let result = sync.sync(force)?;
            if result.fetched {
                println!(
                    "catalog synced: {} agents, {} skills",
                    result.agent_count, result.skill_count
                );
            } else {
                println!(
                    "catalog cache fresh ({} agents, {} skills); use --force to refetch",
                    result.agent_count, result.skill_count
                );
            }
            // #1947: be honest when the synced catalog has no composable agents.
            // The upstream claude-mpm layout moved agents to JSON templates under
            // `src/claude_mpm/agents/`, so `.claude/agents/` is empty; trusty-mpm
            // provisions from its BUNDLED agents, which are the source of truth.
            if result.agents_empty() {
                println!(
                    "warning: the synced catalog contains 0 composable agents \
                     (upstream layout moved); trusty-mpm deploys its bundled agents \
                     — the catalog agent source is not currently used."
                );
            }
        }
        CatalogAction::Ls { json } => {
            let agents = sync.list_agents();
            let skills = sync.list_skills();
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "agents": agents, "skills": skills })
                );
            } else {
                println!("agents ({}):", agents.len());
                for a in &agents {
                    println!("  {a}");
                }
                println!("skills ({}):", skills.len());
                for s in &skills {
                    println!("  {s}");
                }
            }
        }
        CatalogAction::Status { json } => {
            // HR-3: report staleness without mutating anything. The framework root
            // is the daemon-wide baseline "project" (no per-project override).
            let report = trusty_mpm::core::update_check::detect_for_framework(
                &trusty_mpm::core::paths::FrameworkPaths::default(),
                &framework_root,
            );
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "stale": report.stale,
                        "unknown": report.unknown,
                        "changes": report.summary_lines(),
                    })
                );
            } else if report.unknown {
                println!("catalog: unknown (never synced — run `tm catalog sync`)");
            } else if report.stale {
                println!("catalog: stale ({} change(s)):", report.changes.len());
                for line in report.summary_lines() {
                    println!("  {line}");
                }
                println!("run `tm catalog apply` to redeploy the updated content");
            } else {
                println!("catalog: up to date");
            }
        }
        CatalogAction::Apply { force, prune } => {
            // HR-3: accept the rebuild offer — sync, redeploy the selected set,
            // optionally prune deselected managed files.
            let report = trusty_mpm::core::update_check::apply_catalog(
                trusty_mpm::provisioner::RealGitBackend::default(),
                &trusty_mpm::core::paths::FrameworkPaths::default(),
                &framework_root,
                force,
                prune,
            )?;
            let synced = if report.fetched {
                "synced catalog"
            } else {
                "catalog cache fresh"
            };
            println!(
                "{synced}; redeployed {} agent(s), {} skill(s)",
                report.agents_deployed.len(),
                report.skills_deployed.len()
            );
            if !report.agents_skipped.is_empty() || !report.skills_skipped.is_empty() {
                println!(
                    "  skipped (user-modified/unchanged): {} agent(s), {} skill(s)",
                    report.agents_skipped.len(),
                    report.skills_skipped.len()
                );
            }
            if prune && (!report.agents_pruned.is_empty() || !report.skills_pruned.is_empty()) {
                println!(
                    "  pruned (deselected): {} agent(s), {} skill(s)",
                    report.agents_pruned.len(),
                    report.skills_pruned.len()
                );
            }
            // #391: a guard that declines to delete is invisible unless it says
            // so — the operator asked for a prune and must be told what survived
            // it, and where the deleted content went.
            if let Some(backup) = &report.prune_backup_dir {
                println!("  backed up to {}", backup.display());
            }
            for line in report
                .agents_prune_kept
                .iter()
                .chain(report.skills_prune_kept.iter())
            {
                println!("  kept: {line}");
            }
        }
    }
    Ok(())
}

// Unit tests live in managed_tests.rs (test-file budget: 1500 SLOC) —
// extracted from an inline `mod tests` so #2457's new HTTP-round-trip
// coverage doesn't push this production file toward the 500-SLOC cap.
#[cfg(test)]
#[path = "managed_tests.rs"]
mod tests;
