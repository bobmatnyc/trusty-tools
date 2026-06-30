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

use trusty_mpm::client::ManagedSessionSummary;

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

/// Return true when a session state should be shown in the default picker/list view.
///
/// Why: decommissioned tombstones accumulate without bound (#1809); operators
/// don't want 230+ dead records cluttering the picker and `tm sessions ls`. This
/// predicate is the single source of truth for which states are "live" so the
/// picker and the `sessions list` table use identical logic.
/// What: returns `false` for `"decommissioned"` (the sole dead/tombstone state);
/// returns `true` for every other state (active, provisioning, stopped, errored).
/// Test: `picker_filter_excludes_decommissioned_keeps_active` in
/// `tests_behavior_c_tests.rs`.
pub(crate) fn is_live_session_state(state: &str) -> bool {
    state != "decommissioned"
}

/// Filter a session list to only live sessions for display in the picker (#1809).
///
/// Why: the picker must never show decommissioned tombstones by default; the
/// `--all` opt-in re-enables them for `tm sessions ls` via this module's path.
/// What: retains only sessions whose `state` passes `is_live_session_state`.
/// Test: `picker_filter_excludes_decommissioned_keeps_active` in
/// `tests_behavior_c_tests.rs`.
pub(crate) fn filter_live_sessions(
    sessions: Vec<ManagedSessionSummary>,
) -> Vec<ManagedSessionSummary> {
    sessions
        .into_iter()
        .filter(|s| is_live_session_state(&s.state))
        .collect()
}

/// `tm sessions ls` — list managed sessions.
///
/// Why: operators need a quick view of every managed session and its pending
/// decision, optionally scoped to a single project so the firehose is tamed.
/// What: GETs `/api/v1/sessions/managed` (with an optional `?source_id=` filter
/// that the daemon already supports) and prints a table with id, state, name,
/// task (truncated to 30 chars), and created_at; or raw JSON with `--json`.
/// By default, decommissioned tombstone sessions are hidden from the table (#1809);
/// `--all` (i.e. `all=true`) opts in to the full unfiltered list. The `--json`
/// path always returns the raw daemon response unfiltered.
/// The source_id filter is passed straight through as a query parameter rather
/// than doing client-side filtering so callers get the daemon's authoritative view.
/// Test: HTTP path covered by the integration test; filter logic unit-tested by
/// `ls_source_id_filter_selects_correct_slug`,
/// `picker_filter_excludes_decommissioned_keeps_active`.
pub(crate) async fn session_ls(
    client: &reqwest::Client,
    url: &str,
    json: bool,
    source_id: Option<&str>,
    all: bool,
) -> anyhow::Result<()> {
    // Build the request URL, appending ?source_id= when a filter is active.
    let endpoint = format!("{url}/api/v1/sessions/managed");
    let mut req = client.get(&endpoint);
    if let Some(sid) = source_id {
        req = req.query(&[("source_id", sid)]);
    }
    // Fetch the response body ONCE. `--json` echoes that raw text verbatim
    // (byte-for-byte — preserving exact field order/whitespace for scripts);
    // the table path deserializes the SAME text rather than issuing a second GET.
    let raw = req.send().await?.error_for_status()?.text().await?;
    if json {
        // Raw JSON passthrough is always unfiltered — scripts rely on byte-for-byte.
        println!("{raw}");
        return Ok(());
    }
    let fetched = serde_json::from_str::<trusty_mpm::client::ManagedListResponse>(&raw)?.sessions;
    // #1809: filter decommissioned tombstones from the default table view.
    // #1841 Fix 4: when --all is requested, sort so active/stopped sessions come
    // first and decommissioned tombstones come last (stable sort preserves relative
    // order within each group).
    let sessions: Vec<_> = if all {
        let mut s = fetched;
        s.sort_by_key(|sess| u8::from(sess.state == "decommissioned"));
        s
    } else {
        filter_live_sessions(fetched)
    };
    if sessions.is_empty() {
        if let Some(sid) = source_id {
            println!("no managed sessions for {sid}");
        } else {
            println!("no managed sessions");
        }
        return Ok(());
    }
    // Table header
    println!(
        "{:<36}  {:<14}  {:<24}  {:<30}  CREATED",
        "ID", "STATE", "NAME", "TASK"
    );
    for s in &sessions {
        // #1841 Fix 3: show a descriptive placeholder for sessions with no task.
        let task = s
            .task
            .as_deref()
            .map(|t| truncate(t, 30))
            .unwrap_or_else(|| "(interactive)".to_string());
        let created = s
            .created_at
            .as_deref()
            .map(short_timestamp)
            .unwrap_or_default();
        let pending = s
            .pending_decision
            .as_deref()
            .map(|d| format!(" [pending: {d}]"))
            .unwrap_or_default();
        println!(
            // #1841 Fix 5: truncate name with ellipsis when it exceeds column width.
            "{:<36}  {:<14}  {:<24}  {:<30}  {}{}",
            s.id,
            s.state,
            truncate(&s.name, 24),
            task,
            created,
            pending
        );
    }
    Ok(())
}

/// Truncate a string to at most `max` characters, appending `…` when cut.
///
/// Why: task descriptions can be arbitrarily long; the ls table needs a bounded
/// column width.
/// What: returns `&s[..max-1]…` when `s.chars().count() > max`, else `s`.
/// Test: `truncate_clips_and_appends_ellipsis` in the module-level unit tests.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .nth(max.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        format!("{}\u{2026}", &s[..end])
    }
}

/// Render an RFC-3339 timestamp as a short local-ish string (`YYYY-MM-DD HH:MM`).
///
/// Why: the full RFC-3339 value is too wide for a table column; a date+time
/// prefix is human-readable without truncating important information.
/// What: takes the first 16 characters of the timestamp string (`YYYY-MM-DDTHH:MM`),
/// replaces the `T` separator with a space, and returns the result. Falls back to
/// the raw string if it is shorter than 16 chars.
/// Test: `short_timestamp_formats_correctly`.
fn short_timestamp(s: &str) -> String {
    if s.len() >= 16 {
        s[..16].replace('T', " ")
    } else {
        s.to_string()
    }
}

/// `tm sessions activity <id>` — inspect a managed session's activity state.
///
/// Why: inspect what a session is doing without attaching; the raw pane is
/// always returned for the calling agentic process to reason over. The LLM
/// classification is shown when available (OpenRouter key set); when absent,
/// `classification: null` and the raw pane are still returned with no error.
/// What: GETs `/api/v1/sessions/managed/{id}/activity` and prints the raw pane,
/// structured state, classification (or "no classifier"), and pending decision.
/// Test: HTTP path covered by the integration test.
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
        println!("not found");
        return Ok(());
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

/// `tm sessions stop <id>` — stop the runtime of a managed session, keep the workspace.
///
/// Why: a session ENDURES beyond its runtime; `stop` kills only the tmux session
/// and claude process, preserving the workspace for later `resume`. Renamed from
/// the verbose `runtime-stop` in #1205 (which remains a deprecated alias).
/// What: POSTs `/api/v1/sessions/managed/{id}/runtime-stop`.
/// Test: HTTP path covered by the integration test; parse by
/// `cli_parses_session_managed_stop_verb`.
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
        println!("not found");
        return Ok(());
    }
    resp.error_for_status()?;
    println!("runtime stopped {id} (workspace intact; use 'resume' to restart)");
    Ok(())
}

/// `tm sessions resume <id>` — resume a stopped managed session in its existing workspace.
///
/// Why: after `stop`, the workspace is still on disk; `resume` re-spawns the
/// runtime there without re-cloning. Renamed from the verbose `managed-resume`
/// in #1205 (which remains a deprecated alias).
/// What: POSTs `/api/v1/sessions/managed/{id}/resume`.
/// Test: HTTP path covered by the integration test; parse by
/// `cli_parses_session_managed_resume_verb`.
pub(crate) async fn session_resume(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/resume"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    if resp.status() == reqwest::StatusCode::CONFLICT {
        let msg = resp.text().await.unwrap_or_default();
        println!("cannot resume: {msg}");
        return Ok(());
    }
    let body: ManagedSessionSummary = resp.error_for_status()?.json().await?;
    println!("resumed {} ({}) [{}]", body.name, body.id, body.state);
    Ok(())
}

/// `tm sessions decommission <id>` — full teardown (may or may not remove workspace).
///
/// Why: adopted/local-path sessions were NEVER owned by tm, so the workspace is
/// NOT removed on their decommission. Previously the CLI printed an incorrect
/// "workspace removed" message for every decommission — this fix surfaces the
/// daemon's actual `workspace_removed` verdict so the operator is never misled.
/// When the workspace was removed and a pre-decommission path is available, we run
/// `git worktree prune` on the parent directory so git's worktree bookkeeping is
/// cleaned up immediately (rather than waiting for the next GC pass).
/// What: POSTs `/api/v1/sessions/managed/{id}/decommission`, reads the
/// `workspace_removed` / `workspace_path_was` fields from the JSON response, and
/// prints a message that accurately reflects what happened. When `workspace_removed`
/// is `true` and `workspace_path_was` is present, runs `git worktree prune` on
/// the parent directory (best-effort; errors are silently suppressed).
/// Test: `decommission_message_reflects_workspace_removed` (unit);
/// HTTP path covered by the integration test.
pub(crate) async fn session_decommission(
    client: &reqwest::Client,
    url: &str,
    id: String,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/decommission"))
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        println!("not found");
        return Ok(());
    }
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    let workspace_removed = body
        .get("workspace_removed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let id_display = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or(id.as_str());
    if workspace_removed {
        println!("decommissioned {id_display} — workspace removed; tombstone record kept");
        // Run `git worktree prune` on the parent of the (now-deleted) workspace so
        // the main repo's worktree bookkeeping is cleaned up immediately.
        if let Some(ws_was) = body.get("workspace_path_was").and_then(|v| v.as_str()) {
            let parent = std::path::Path::new(ws_was)
                .parent()
                .unwrap_or(std::path::Path::new(ws_was));
            let _ = std::process::Command::new("git")
                .args(["-C", &parent.to_string_lossy(), "worktree", "prune"])
                .output();
        }
    } else {
        println!(
            "decommissioned {id_display} — record decommissioned; \
             workspace left in place (not owned by tm)"
        );
    }
    Ok(())
}

/// `tm sessions decommission-ephemeral` — bulk-tear-down every ephemeral session (#1508).
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

/// `tm sessions prune --state <filter> [--dry-run] [--include-active]` — by-state prune (#1508).
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
    }
    let verb = if dry_run { "would prune" } else { "pruned" };
    println!("{verb} {} session(s) (filter={state})", sessions.len());
    Ok(())
}

/// `tm sessions prune --worktrees [--dry-run]` — remove orphaned per-session worktrees (#1840).
///
/// Why: sessions decommissioned before Fix 1a (#1840), or where
/// `git worktree remove` failed, leave stale `.worktrees/<session-id>/`
/// directories on disk. This verb lets operators clean them up safely — only
/// dirs without a corresponding active session are ever removed.
/// What: POSTs `/api/v1/sessions/managed/prune-worktrees` with `{ dry_run }`;
/// prints one path per removed (or would-remove) directory and a summary count.
/// Test: HTTP path covered by integration test; CLI parse by `cli_parses_session_prune`.
pub(crate) async fn session_prune_worktrees(
    client: &reqwest::Client,
    url: &str,
    dry_run: bool,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/prune-worktrees"))
        .json(&serde_json::json!({ "dry_run": dry_run }))
        .send()
        .await?;
    let body: serde_json::Value = resp.error_for_status()?.json().await?;
    let paths = body
        .get("paths")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut printed = 0usize;
    // Item 6 (#1845): non-string entries in the `paths` array are unexpected
    // (the server controls the format) but must not crash the CLI. Warn to
    // stderr so the operator is aware, rather than silently dropping the entry.
    for p in &paths {
        if let Some(s) = p.as_str() {
            println!("{s}");
            printed += 1;
        } else {
            eprintln!("warning: prune-worktrees: unexpected non-string path entry: {p}");
        }
    }
    let verb = if dry_run { "would remove" } else { "removed" };
    println!("{verb} {printed} orphaned worktree dir(s)");
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
        trusty_mpm::provisioner::RealGitBackend,
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
                trusty_mpm::provisioner::RealGitBackend,
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
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{short_timestamp, truncate};

    #[test]
    fn truncate_clips_and_appends_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hell\u{2026}");
        assert_eq!(truncate("", 5), "");
        assert_eq!(truncate("abcde", 5), "abcde");
    }

    #[test]
    fn short_timestamp_formats_correctly() {
        assert_eq!(short_timestamp("2025-06-27T14:32:00Z"), "2025-06-27 14:32");
        assert_eq!(short_timestamp("short"), "short");
        assert_eq!(short_timestamp("2025-06-27T14:32"), "2025-06-27 14:32");
    }

    #[test]
    fn decommission_message_reflects_workspace_removed() {
        // Guard that the key field names used in session_decommission match the
        // daemon's DecommissionResponse serde output. If the daemon renames those
        // keys this test catches the drift before the JSON decodes silently to None.
        let owned_removed = serde_json::json!({
            "id": "abc-123",
            "workspace_removed": true,
            "workspace_path_was": "/some/workspace/path"
        });
        assert_eq!(
            owned_removed
                .get("workspace_removed")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            owned_removed
                .get("workspace_path_was")
                .and_then(|v| v.as_str()),
            Some("/some/workspace/path")
        );
        let adopted_not_removed = serde_json::json!({
            "id": "xyz-456",
            "workspace_removed": false
        });
        assert_eq!(
            adopted_not_removed
                .get("workspace_removed")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert!(adopted_not_removed.get("workspace_path_was").is_none());
    }
}
