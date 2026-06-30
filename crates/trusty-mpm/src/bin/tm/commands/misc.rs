//! Miscellaneous command handlers that don't belong to a larger group.
//!
//! Why: `status`, `events`, `doctor`, `hook`, `coordinator`, `overseer`,
//! `optimizer`, and `attach` are small, loosely related commands that are
//! easier to find together than scattered across the codebase.
//! What: one public `async fn` per subcommand, plus `status_icon` helper.
//! Test: `cli_parses_*` parse tests for each command in `tests.rs`.

use crate::cli::{CliCompressionLevel, OptimizerAction, OverseerAction};
use crate::commands::daemon::{daemon_healthy, print_status};
use crate::formatters::session::short_id;
use crate::types::EventRow;

/// Environment variable set by every MPM-spawned sub-agent process so its
/// nested Claude Code session can suppress hook traffic.
///
/// Why: a sub-agent is already running inside an MPM-supervised context;
/// re-emitting hook events from inside it just doubles every tool call in
/// the daemon's audit log without adding signal. Stamping the env var on
/// the spawn side and gating the hook handler on the same var keeps the
/// suppression cheap, explicit, and process-local. Re-exporting the shared
/// constant from `trusty_common::claude_config` ensures the spawn site
/// (`open-mpm`) and this consumer never drift apart on the literal name.
/// What: thin alias for
/// [`trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR`]. Presence
/// is what matters; the canonical value used by spawn helpers is `"1"`.
pub(crate) const SUB_AGENT_ENV: &str = trusty_common::claude_config::CLAUDE_MPM_SUB_AGENT_ENV_VAR;

/// Opt-out environment variable that disables the hook handler entirely.
///
/// Why: provides an escape hatch for any environment (CI, SAM deploys, custom
/// build shells) where the hook must not fire at all — without requiring the
/// user to uninstall it from `.claude/settings.json`. Sitting alongside the
/// `SUB_AGENT_ENV` guard makes the two opt-out mechanisms symmetric and
/// discoverable. Setting this in the build shell's env is sufficient; no
/// Claude Code restart is required (it is read per-invocation).
/// What: the literal env var name `"TRUSTY_MPM_DISABLE_HOOKS"`. Presence
/// (any value) causes the hook handler to short-circuit immediately.
/// Test: `hook_disable_env_short_circuits` in `tests_behavior_a.rs`.
pub(crate) const DISABLE_HOOKS_ENV: &str = "TRUSTY_MPM_DISABLE_HOOKS";

/// Catalog status message shown when the catalog has never been synced.
///
/// Why: extracting this as a constant lets tests import the canonical string
/// rather than asserting against a hardcoded copy that could silently diverge
/// from the production output (#1839 review finding 4).
/// What: the `catalog:` line shown by `tm health` for a brand-new install.
/// Test: `health_catalog_stale_message_contains_action` in `tests.rs`.
pub(crate) const CATALOG_UNKNOWN_MSG: &str = "never synced (run 'tm catalog sync' to initialize)";

/// Catalog status message shown when the catalog is stale.
///
/// Why: same rationale as `CATALOG_UNKNOWN_MSG` — tests import the constant
/// so a string regression is caught by the test, not silently ignored.
/// What: the `catalog:` line shown by `tm health` when newer agents exist.
/// Test: `health_catalog_stale_message_contains_action` in `tests.rs`.
pub(crate) const CATALOG_STALE_MSG: &str = "updates available (run 'tm catalog sync' to update)";

/// Help hint printed to stderr when `tm` is invoked outside a git project.
///
/// Why: extracting this as a constant lets the test in `tests.rs` import the
/// actual production string rather than a hardcoded copy (#1839 review finding 4).
/// `guided.rs` is at the 500-SLOC cap, so the constant lives here and is
/// referenced from guided.rs via `super::misc::NON_GIT_FALLBACK_HINT`.
/// What: the one-line hint printed (to stderr) by `fallback_protected` when the
/// CWD is not inside any git working tree.
/// Test: `non_git_dir_fallback_prints_help_hint` in `tests.rs`.
pub(crate) const NON_GIT_FALLBACK_HINT: &str =
    "tm: not in a git project — run 'tm --help' for available commands";

/// `status` subcommand — probe daemon health and list sessions.
///
/// Why: the first thing an operator runs to see if the daemon is alive.
/// What: `GET /health` then `GET /sessions`, printing one line per session.
/// Test: run against a live daemon; "daemon: unreachable" when it is down.
pub(crate) async fn status(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    if !daemon_healthy(client, url).await {
        println!("daemon: unreachable");
        return Ok(());
    }
    print_status(client, url).await
}

/// `events` subcommand — print the recent hook-event feed.
///
/// Why: gives operators a quick tail of daemon activity without the TUI. The
/// daemon serves a live SSE stream at `/events`; this CLI command polls the
/// legacy snapshot at `/events/poll`, which mirrors the historical behaviour.
/// What: `GET /events/poll`, printing `{timestamp} {session_short} {event}`.
/// Test: run against a daemon that has ingested hook events.
pub(crate) async fn events(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    use serde::Deserialize;
    #[derive(Deserialize)]
    struct Body {
        events: Vec<EventRow>,
    }
    let body: Body = client
        .get(format!("{url}/events/poll"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    for e in &body.events {
        println!("{} {} {}", e.at, short_id(&e.session), e.event);
    }
    Ok(())
}

/// `doctor` subcommand — run and print the full system diagnostic.
///
/// Why: a misconfigured trusty-mpm stack fails confusingly; `tm doctor` runs
/// every health probe in one command and prints a formatted verdict so the
/// operator can confirm — or fix — a broken install at a glance.
/// What: runs [`TrustyCommand::Doctor`] through the shared [`CommandExecutor`]
/// (which calls `GET /api/v1/doctor`), then prints one status-tagged line per
/// check plus an overall verdict. An unreachable daemon prints an error line.
/// Test: `cli_parses_doctor` covers parsing; the report path is covered by the
/// executor's `execute_doctor_against_test_daemon` test.
pub(crate) async fn doctor(url: &str) -> anyhow::Result<()> {
    use trusty_mpm::client::{CommandExecutor, CommandResult, TrustyCommand};
    use trusty_mpm::core::doctor::CheckStatus;

    let executor = CommandExecutor::new(url.to_string());
    match executor.execute(TrustyCommand::Doctor).await {
        CommandResult::Doctor(report) => {
            println!("trusty-mpm doctor");
            for check in &report.checks {
                println!(
                    "  {} {:<13} {}",
                    status_icon(check.status),
                    check.name,
                    check.message,
                );
            }
            println!(
                "\noverall: {} {}",
                status_icon(report.overall),
                match report.overall {
                    CheckStatus::Ok => "all checks passed",
                    CheckStatus::Warn => "passed with warnings",
                    CheckStatus::Fail => "one or more checks failed",
                },
            );
        }
        CommandResult::Error(msg) => eprintln!("doctor failed: {msg}"),
        other => eprintln!("doctor: unexpected result {other:?}"),
    }
    Ok(())
}

/// `health` subcommand — report daemon health through chat-core.
///
/// Why: the operator (and scripts) want a quick "is the daemon up, is the catalog
/// fresh, how big is the fleet?" probe. Routing it through the shared
/// [`CommandExecutor`] (rather than a bespoke `reqwest` call here) means the CLI
/// shares the exact `health` dispatch with every other adapter — a single source
/// of truth per the chat-core nucleus. The `resolved:` line tells operators
/// whether traffic is flowing via the trusty-console gateway or direct to the
/// daemon, making the resolution path transparent (#1849 Phase 2).
/// What: runs [`TrustyCommand::Health`] through the executor and prints a compact,
/// scriptable summary. A dead daemon prints `daemon: unreachable` and is NOT an
/// error exit (the probe succeeded in determining the daemon is down). The
/// `resolved:` line is printed only on success so unreachable output is unchanged.
/// Test: `cli_parses_health` covers parsing; the executor's `execute_health_*`
/// tests cover the live and dead-daemon report paths.
pub(crate) async fn health(url: &str) -> anyhow::Result<()> {
    use trusty_mpm::client::{CommandExecutor, CommandResult, TrustyCommand};
    use trusty_mpm::core::GATEWAY_PATH;

    let executor = CommandExecutor::new(url.to_string());
    match executor.execute(TrustyCommand::Health).await {
        CommandResult::Health(report) if !report.reachable => {
            println!("daemon: unreachable ({})", report.url);
        }
        CommandResult::Health(report) => {
            let catalog = if report.catalog_unknown {
                CATALOG_UNKNOWN_MSG
            } else if report.catalog_stale {
                CATALOG_STALE_MSG
            } else {
                "up to date"
            };
            // Indicate the resolution path so operators can see whether traffic
            // is flowing via the console gateway or direct to the daemon.
            let resolved_via = if url.contains(GATEWAY_PATH) {
                format!("via gateway {url}")
            } else {
                format!("direct {url}")
            };
            println!("daemon: {} ({})", report.status, report.url);
            println!("resolved: {resolved_via}");
            println!("catalog: {catalog}");
            println!(
                "fleet: {} session(s), {} awaiting a decision",
                report.managed_total, report.managed_pending_decisions
            );
        }
        CommandResult::Error(msg) => eprintln!("health failed: {msg}"),
        other => eprintln!("health: unexpected result {other:?}"),
    }
    Ok(())
}

/// Render a [`CheckStatus`] as a status icon for terminal output.
///
/// Why: the `tm doctor` table marks each check with a glanceable symbol; one
/// helper keeps the mapping consistent between the per-check lines and the
/// overall verdict.
/// What: `Ok → ✅`, `Warn → ⚠️`, `Fail → ❌`.
/// Test: covered indirectly by the `doctor` output.
fn status_icon(status: trusty_mpm::core::doctor::CheckStatus) -> &'static str {
    use trusty_mpm::core::doctor::CheckStatus;
    match status {
        CheckStatus::Ok => "\u{2705}",
        CheckStatus::Warn => "\u{26a0}\u{fe0f}",
        CheckStatus::Fail => "\u{274c}",
    }
}

/// `hook` subcommand — handle a Claude Code lifecycle hook event.
///
/// Why: Claude Code invokes the configured hook command on every PreToolUse /
/// PostToolUse / Stop event. The handler posts a minimal `hook_event` to the
/// daemon so the circuit breaker, audit log, and dashboard can react. To keep
/// nested MPM sub-agents from doubling every tool call in the audit feed, the
/// very first thing the handler does is check `CLAUDE_MPM_SUB_AGENT`; when
/// that env var is present the handler exits 0 immediately without contacting
/// the daemon. `TRUSTY_MPM_DISABLE_HOOKS` provides an additional escape hatch
/// for any environment (SAM deploys, CI, stripped-PATH build shells) where
/// the hook must not fire at all.
/// What: reads the event name from `CLAUDE_HOOK_EVENT` and the session id
/// from `CLAUDE_SESSION_ID` (both populated by Claude Code; missing values
/// degrade to placeholders so the daemon still gets a record). POSTs to
/// `<url>/hooks` with a 500 ms connect timeout and a 2-second total timeout.
/// Every failure path returns `Ok(())` — failing the hook would block the
/// user's prompt.
/// Test: `cli_parses_hook` covers parse routing; the guard branches are
/// exercised via `hook_guard_short_circuits` and
/// `hook_disable_env_short_circuits` in `tests_behavior_a.rs`.
pub(crate) async fn hook(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    // Guard 1: suppress inside MPM-spawned sub-agents.
    if std::env::var_os(SUB_AGENT_ENV).is_some() {
        return Ok(());
    }
    // Guard 2: universal opt-out for build shells / CI that can't remove the
    // hook from settings.json without a restart.
    if std::env::var_os(DISABLE_HOOKS_ENV).is_some() {
        return Ok(());
    }

    // Pull the minimal context Claude Code stamps into the environment. Both
    // values may be absent in test harnesses or future Claude Code versions;
    // degrade to placeholders so the daemon still gets a record.
    let event = std::env::var("CLAUDE_HOOK_EVENT").unwrap_or_else(|_| "Unknown".to_string());
    let session_id = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();

    // Include the caller's cwd so the daemon can correlate this SessionStart
    // event to the right managed session by workspace_path (issue #1744).
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_default();
    let body = serde_json::json!({
        "session_id": session_id,
        "event": event,
        "payload": { "cwd": cwd }
    });

    // Build a hook-specific client with a tight connect timeout so a
    // pathological OS-level TCP-connect stall never eats into the 2 s
    // total budget. The shared `client` parameter is used for all other
    // subcommands; for the hook we build a short-lived client here so
    // the connect guard applies only to this hot path.
    let hook_client = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            // Client build failure is programmer-class; degrade to the
            // shared client (no connect timeout) rather than blocking.
            let req = client
                .post(format!("{url}/hooks"))
                .timeout(std::time::Duration::from_secs(2))
                .json(&body)
                .send();
            let _ = req.await;
            return Ok(());
        }
    };

    // Best-effort POST — any failure (daemon down, network blip, malformed
    // url) becomes a silent Ok(()) so Claude Code never sees a non-zero exit.
    let _ = hook_client
        .post(format!("{url}/hooks"))
        .json(&body)
        .send()
        .await;
    Ok(())
}

/// `coordinator` / `coord` subcommand — message the cross-session coordinator.
///
/// Why: a scriptable, one-shot entry point to the coordinator so Telegram, cron
/// jobs, and shell scripts can ask "what is happening across my sessions?" or
/// route a `@session:` command without the TUI.
/// What: dispatches a [`TrustyCommand::CoordinatorChat`] through the shared
/// [`CommandExecutor`] and prints the reply (or the routed command's output);
/// a daemon/LLM failure becomes a non-zero-exit error line.
/// Test: covered by the executor's coordinator wire-shape tests.
pub(crate) async fn coordinator(url: &str, message: String) -> anyhow::Result<()> {
    use trusty_mpm::client::{CommandExecutor, CommandResult, TrustyCommand};
    let executor = CommandExecutor::new(url.to_string());
    match executor
        .execute(TrustyCommand::CoordinatorChat { message })
        .await
    {
        CommandResult::ChatReply { reply } => println!("{reply}"),
        CommandResult::Error(msg) => {
            eprintln!("coordinator: {msg}");
            std::process::exit(1);
        }
        other => eprintln!("unexpected coordinator result: {other:?}"),
    }
    Ok(())
}

/// `overseer` subcommand — inspect the session overseer.
///
/// Why: operators need to see whether oversight is active without the TUI.
/// What: `Status` calls `GET /overseer` and prints the enabled flag and
/// handler type.
/// Test: `cli_parses_overseer_status`.
pub(crate) async fn overseer(
    client: &reqwest::Client,
    url: &str,
    action: OverseerAction,
) -> anyhow::Result<()> {
    match action {
        OverseerAction::Status => {
            let body: serde_json::Value = client
                .get(format!("{url}/overseer"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let overseer = &body["overseer"];
            let enabled = overseer
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let handler = overseer
                .get("handler")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!(
                "overseer: {} (handler: {handler})",
                if enabled { "enabled" } else { "disabled" }
            );
        }
    }
    Ok(())
}

/// Inspect or configure the token-use optimizer.
///
/// Why: the optimizer policy is framework-managed on disk; `Status` reads the
/// daemon's live view via `GET /optimizer`, while `Set` rewrites the policy
/// file itself (`~/.trusty-mpm/framework/hooks/optimizer.toml`) — the daemon's
/// watcher then reloads it.
/// What: `Status` prints the current config; `Set` writes a new `[default]`
/// level into the policy file, creating the `hooks/` directory if needed.
/// Test: `cli_parses_optimizer_status`, `cli_parses_optimizer_set`.
pub(crate) async fn optimizer(
    client: &reqwest::Client,
    url: &str,
    action: OptimizerAction,
) -> anyhow::Result<()> {
    match action {
        OptimizerAction::Status => {
            let body: serde_json::Value = client
                .get(format!("{url}/optimizer"))
                .send()
                .await?
                .json()
                .await?;
            println!("{}", serde_json::to_string_pretty(&body["optimizer"])?);
        }
        OptimizerAction::Set { level } => {
            let level_name = match level {
                CliCompressionLevel::Off => "Off",
                CliCompressionLevel::Trim => "Trim",
                CliCompressionLevel::Summarise => "Summarise",
                CliCompressionLevel::Caveman => "Caveman",
            };
            let paths = trusty_mpm::core::paths::FrameworkPaths::default();
            let path = paths.optimizer_config();
            std::fs::create_dir_all(&paths.hooks)?;
            let contents = format!(
                "# trusty-mpm token optimizer — framework hook configuration\n\
                 # Edited by: trusty-mpm optimizer set\n\n\
                 [default]\nlevel = \"{level_name}\"\n\n\
                 [tools]\n"
            );
            std::fs::write(&path, contents)?;
            println!("optimizer level set to {level_name} ({})", path.display());
        }
    }
    Ok(())
}

/// Resolve `target` to a session and either print JSON or open the TUI.
///
/// Why: `tm attach` is the ergonomic entry point for an *existing* session —
/// operators type a short name or path rather than a full UUID.
/// What: Fetches `/sessions`, resolves via `resolve_target`, then either
/// serialises to JSON (`--json`) or launches the TUI pre-focused on the match.
/// Test: Resolution logic is tested in `trusty-mpm-core::connect`; here we
/// test CLI flag parsing only (see unit tests).
pub(crate) async fn attach_cmd(
    client: &reqwest::Client,
    url: &str,
    target: &str,
    json: bool,
) -> anyhow::Result<()> {
    use trusty_mpm::core::{ResolveResult, SessionSummary, resolve_target};

    // The daemon wraps the session array in a `{ "sessions": [...] }` envelope.
    let resp: serde_json::Value = client
        .get(format!("{url}/sessions"))
        .send()
        .await?
        .json()
        .await?;
    let empty = vec![];
    let raw = resp
        .get("sessions")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    // Parse sessions array — be tolerant of missing fields. `SessionId` is
    // serialized as a bare UUID string; the friendly name is `tmux_name`.
    let sessions: Vec<SessionSummary> = raw
        .iter()
        .filter_map(|v| {
            Some(SessionSummary {
                id: v["id"].as_str()?.to_string(),
                name: v["tmux_name"].as_str().map(str::to_string),
                workdir: v["workdir"].as_str().unwrap_or("").to_string(),
                last_active: v["last_seen"]["secs_since_epoch"].as_u64().unwrap_or(0),
            })
        })
        .collect();

    match resolve_target(target, &sessions) {
        ResolveResult::Found(id) => {
            if json {
                // Find the full session JSON and print it.
                if let Some(s) = raw.iter().find(|v| v["id"].as_str() == Some(&id)) {
                    println!("{}", serde_json::to_string_pretty(s)?);
                }
                return Ok(());
            }
            // Launch the TUI focused on this session.
            let resolved_url = trusty_mpm::core::resolve_daemon_url(Some(url));
            trusty_mpm::tui::run_focused(resolved_url, 1000, Some(id)).await
        }
        ResolveResult::Ambiguous(ids) => {
            eprintln!(
                "Ambiguous target '{target}' — matched {} sessions:",
                ids.len()
            );
            for id in &ids {
                eprintln!("  {id}");
            }
            std::process::exit(1);
        }
        ResolveResult::NotFound => {
            eprintln!("No session matched '{target}'.");
            if !sessions.is_empty() {
                eprintln!("Available sessions:");
                for s in &sessions {
                    let name = s.name.as_deref().unwrap_or("-");
                    eprintln!("  {} ({})  {}", s.id, name, s.workdir);
                }
            }
            std::process::exit(1);
        }
    }
}
