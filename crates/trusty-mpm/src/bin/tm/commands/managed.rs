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

/// `tm sessions ls` — list managed sessions.
///
/// Why: operators need a quick view of every managed session and its pending
/// decision.
/// What: GETs `/api/v1/sessions/managed` and prints a table or raw JSON.
/// Test: HTTP path covered by the integration test.
pub(crate) async fn session_ls(
    client: &reqwest::Client,
    url: &str,
    json: bool,
) -> anyhow::Result<()> {
    // Fetch the response body ONCE. `--json` echoes that raw text verbatim
    // (byte-for-byte — preserving exact field order/whitespace for scripts);
    // the table path deserializes the SAME text rather than issuing a second GET.
    let raw = client
        .get(format!("{url}/api/v1/sessions/managed"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    if json {
        println!("{raw}");
        return Ok(());
    }
    let sessions = serde_json::from_str::<trusty_mpm::client::ManagedListResponse>(&raw)?.sessions;
    if sessions.is_empty() {
        println!("no managed sessions");
    }
    for s in &sessions {
        let pending = s
            .pending_decision
            .as_deref()
            .map(|d| format!(" pending=\"{d}\""))
            .unwrap_or_default();
        println!("{} {} {}{}", s.id, s.name, s.state, pending);
    }
    Ok(())
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
        println!("--- raw pane (last 60 lines) ---");
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

/// `tm sessions decommission <id>` — full teardown (remove workspace from disk).
///
/// Why: the ONLY operation that permanently removes the workspace directory.
/// Unlike `runtime-stop`, decommission is terminal — no resume is possible.
/// A tombstone record is kept so `ls` shows history.
/// What: POSTs `/api/v1/sessions/managed/{id}/decommission`.
/// Test: HTTP path covered by the integration test.
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
    resp.error_for_status()?;
    println!("decommissioned {id} (workspace removed; tombstone record kept)");
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
