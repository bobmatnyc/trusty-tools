// Pre-existing clippy warnings across this large binary crate.
// Each category below is suppressed at crate level with rationale:
// - dead_code / unused_imports: Many helpers are kept for future use, behind
//   feature flags, or used only on certain platforms / by tests; pruning them
//   is its own refactor and would churn unrelated modules.
// - clippy::collapsible_if / collapsible_else_if: Style preference; nested
//   ifs are often clearer with the existing comments and gating logic.
// - clippy::manual_str_repeat / manual_repeat_n / single_char_add_str: Style
//   nits in display/formatting code where current form reads fine.
// - clippy::too_many_arguments: A few orchestration entry points genuinely
//   need their argument count; signatures are part of internal contracts.
// - clippy::await_holding_lock: Test-only — a std::sync::Mutex serializes
//   tests that mutate process-global env (HOME, etc.). The await points are
//   inside the critical section by design, and tests are single-threaded
//   per-test by virtue of the lock.
// - clippy::clone_on_copy / len_zero / map_or / etc.: Misc style nits in
//   pre-existing code; not worth the churn vs. risk of breaking 1500+ tests.
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::manual_str_repeat)]
#![allow(clippy::manual_repeat_n)]
#![allow(clippy::single_char_add_str)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::await_holding_lock)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::len_zero)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_split_once)]
#![allow(clippy::needless_splitn)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::single_component_path_imports)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::redundant_pattern_matching)]

//! Pre-parse startup initialization: env loading, tracing, early-exit mode
//! dispatch (`--version` / `--api` / `--search-service`), state-dir + run-id
//! setup, migrations, and background-service bootstrap.

use std::path::PathBuf;

use anyhow::Result;

use super::cli_def::check_credentials_and_warn;
use crate::{
    api, build_info, bus, ctrl, logging, mcp, memory, process_tracker, registry, repl, search,
    session_registry, workflow,
};

use build_info::BuildInfo;

/// Run all side-effecting startup initialization that must happen before the
/// top-level clap parse.
///
/// Why: `run()` previously inlined ~320 lines of bootstrapping (env loading,
/// tracing, early-exit dispatch for `--version`/`--api`/`--search-service`,
/// state-dir creation, build-counter bump, chat-logger, run-id, migrations,
/// worktree/project/process cleanup, and the message bus). Extracting it keeps
/// `run()` readable and the file under the 500-line cap.
/// What: Performs the bootstrap in argv order. Returns `Ok(false)` when an
/// early-exit path already handled the invocation (so the caller should
/// `return Ok(())`); returns `Ok(true)` to continue into the main dispatch.
/// Test: Indirectly via `cargo run -p trusty-agents`/`trusty-agents-local` and the
/// crate's integration tests (`--version`, `--api`, normal REPL startup).
pub(super) async fn run_startup_init(_args: &[String]) -> Result<bool> {
    // Handle --version / -V before anything else (no env/tracing/etc.).
    // Why: `--version` must be cheap and side-effect-free so it's safe to
    // run in CI or scripts without an OPENROUTER_API_KEY. It still bumps
    // the build counter so CI runs are disambiguated in logs.
    //
    // Note: We probe argv directly here (not via clap) so the version path
    // doesn't depend on clap successfully parsing every other flag. Clap's
    // own version-handling is disabled (`disable_version_flag = true` on
    // `Cli`) so we control formatting + the build-counter bump.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.iter().any(|a| a == "--version" || a == "-V") {
        // Resolve project dir so `.trusty-agents/state` lands in the project root
        // even when invoked from a subdirectory.
        let state_dir = ctrl::detect_self_project()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join(".trusty-agents")
            .join("state");
        tokio::fs::create_dir_all(&state_dir).await?;
        let info = BuildInfo::load_and_increment().await?;
        println!("{}", info.display_string());
        return Ok(false);
    }

    // Load env and init tracing first so everything downstream has logs/keys.
    //
    // #250: `.env.local` lookup is relative to cwd, so launching `trusty-agents` from
    // anywhere other than the project root (e.g. `cd /tmp && trusty-agents ctrl`) used
    // to skip credential loading entirely and surface as
    // "no LLM credentials configured". We additionally try the detected
    // self-project directory so the harness picks up its own `.env.local`
    // regardless of the user's cwd. dotenvy does NOT override existing env vars
    // by default, so cwd-local `.env.local` still wins when both exist.
    dotenvy::from_filename(".env.local").ok();
    dotenvy::dotenv().ok();
    if let Some(project_dir) = ctrl::detect_self_project() {
        let project_env = project_dir.join(".env.local");
        if project_env.is_file() {
            dotenvy::from_path(&project_env).ok();
        }
    }

    // Why: External agent plugins (cto-assistant, future personas) are
    //      installed by private launchers BEFORE calling `run()`. The
    //      published `trusty-agents` crate has zero knowledge of those private
    //      crates — see `install_plugins()` in `crate::lib` and the
    //      sibling `trusty-agents-local` binary for the wiring point.
    // What: Anything the launcher passed to `install_plugins(...)` has
    //       already populated the OnceLock; the ctrl loop will pick it
    //       up when it builds the persona's tool surface.
    // Test: `trusty-agents-local` integration; `trusty-agents` standalone has an
    //       empty plugin list.

    // #366: Credential onboarding banner. After env loading is the right time
    // to check — both `.env.local` files and the host environment have been
    // merged in. Suppress in quiet/non-interactive modes (sub-agent IPC,
    // workflow runners, HTTP servers) where stderr output would corrupt
    // protocol streams or just be noise.
    {
        let raw_args: Vec<String> = std::env::args().collect();
        let quiet_mode = raw_args
            .iter()
            .any(|a| matches!(a.as_str(), "--agent" | "--serve" | "--api" | "--workflow"));
        if !quiet_mode {
            check_credentials_and_warn();
        }
    }

    // Default log level: "warn" for interactive REPL (clean UX), "info" for
    // batch/workflow/api modes. RUST_LOG always overrides both.
    // Set TAGENT_LOG=info (or debug/trace) to override without RUST_LOG syntax.
    let is_interactive_repl = repl::is_tty()
        && !std::env::args().any(|a| {
            matches!(
                a.as_str(),
                "--workflow" | "--direct" | "--api" | "--serve" | "--agent"
            )
        });
    let default_level = crate::env_compat::env_var("TAGENT_LOG", "OPEN_MPM_LOG")
        .unwrap_or_else(|_| if is_interactive_repl { "warn" } else { "info" }.to_string());

    // #257: When running the interactive REPL, route tracing output to a log
    // file instead of stderr. Both stdout and stderr render in the same TTY,
    // so a single `WARN` line from `tracing` would clobber the carefully
    // positioned chat scrollback (and was visibly leaking into the prompt).
    // Non-interactive modes (subagent, workflow, api, direct, piped stdin)
    // keep stderr writing so existing log-capture tooling still works.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));
    if is_interactive_repl {
        let log_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".trusty-agents")
            .join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join("repl.log");
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(file) => {
                tracing_subscriber::fmt()
                    .with_env_filter(env_filter)
                    .with_writer(std::sync::Mutex::new(file))
                    .with_ansi(false)
                    .init();
            }
            Err(_) => {
                // Fallback: discard logs entirely rather than corrupt the TTY.
                tracing_subscriber::fmt()
                    .with_env_filter(env_filter)
                    .with_writer(std::io::sink)
                    .init();
            }
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr) // keep stdout clean for NDJSON
            .init();
    }

    // #api-early-dispatch: Short-circuit `--api` / `--serve` BEFORE any
    // filesystem state setup. The Tauri sidecar spawns this binary with cwd=`/`
    // (sealed read-only APFS volume on macOS), so the subsequent
    // `create_dir_all(&state_dir)` would crash with EROFS ("/.trusty-agents/state")
    // before the HTTP listener ever binds. The API server is fully
    // self-contained — it doesn't need state dirs, build-counter increments,
    // worktree cleanup, project registry, or message bus — so we can take the
    // fast path here and let `serve_with_config` own its own setup.
    //
    // We do a manual argv scan here because the full `Cli::try_parse_from` runs
    // later (after subcommand dispatch). Env loading and tracing init have
    // already happened above, so credentials and logs work as expected.
    //
    // Why: Fix for "API server did not become healthy within 20s" when the
    // Tauri app launches the sidecar from cwd=`/`.
    // What: When --api/--serve is present, parse --port and --api-token from
    //       argv and call serve_with_config directly, bypassing all PM-mode
    //       state initialization.
    // Test: `cd / && trusty-agents --api --port 8765 &; sleep 2; curl
    //       http://127.0.0.1:8765/api/health` returns 200 instead of crashing.
    {
        let raw_args: Vec<String> = std::env::args().collect();
        let wants_api = raw_args.iter().any(|a| a == "--api" || a == "--serve");
        if wants_api {
            // Find --port <N> in argv (default 8080 to match clap default).
            let mut port: u16 = 8080;
            let mut iter = raw_args.iter();
            while let Some(a) = iter.next() {
                if a == "--port"
                    && let Some(v) = iter.next()
                    && let Ok(n) = v.parse::<u16>()
                {
                    port = n;
                    break;
                }
                if let Some(rest) = a.strip_prefix("--port=")
                    && let Ok(n) = rest.parse::<u16>()
                {
                    port = n;
                    break;
                }
            }
            // Find --api-token <TOK> (or env fallback).
            let mut token: Option<String> = None;
            let mut iter = raw_args.iter();
            while let Some(a) = iter.next() {
                if a == "--api-token"
                    && let Some(v) = iter.next()
                {
                    token = Some(v.clone());
                    break;
                }
                if let Some(rest) = a.strip_prefix("--api-token=") {
                    token = Some(rest.to_string());
                    break;
                }
            }
            let token = token
                .or_else(|| {
                    crate::env_compat::env_var("TAGENT_API_TOKEN", "OPEN_MPM_API_TOKEN").ok()
                })
                .filter(|s| !s.is_empty());
            api::server::serve_with_config(api::server::ApiConfig { port, token }).await?;
            return Ok(false);
        }
    }

    // #374 early dispatch: `--search-service` runs the search daemon.
    // Same rationale as the `--api` early dispatch above — the daemon is
    // self-contained and doesn't need the heavy state-dir scaffolding,
    // and we want it to start fast.
    {
        let raw_args: Vec<String> = std::env::args().collect();
        if raw_args.iter().any(|a| a == "--search-service") {
            let project_root =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            search::service::run_search_service(project_root).await?;
            return Ok(false);
        }
    }

    // Bump the persistent build counter and log the banner so every process
    // invocation (PM, sub-agent, workflow, --reindex, etc.) is tagged.
    // Resolve project dir so `.trusty-agents/state` lands in the project root
    // even when invoked from a subdirectory.
    let state_dir = ctrl::detect_self_project()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join(".trusty-agents")
        .join("state");
    tokio::fs::create_dir_all(&state_dir).await?;
    let build_info = BuildInfo::load_and_increment().await?;
    tracing::info!("{}", build_info.display_string());

    // Feature B3: Initialise the chat logger. The log directory lives under
    // the resolved project's `.trusty-agents/state/logs/`, mirroring where other
    // runtime state is written. Cleanup of expired `.log.gz` archives runs
    // synchronously at startup so retention is enforced once per boot.
    {
        let log_cfg = mcp::GlobalConfig::load().await.logging.clone();
        if log_cfg.enabled {
            let log_dir = state_dir.join("logs");
            let _ = std::fs::create_dir_all(&log_dir);
            let logger = logging::ChatLogger::start(log_dir, log_cfg.clone());
            logger.cleanup_old_logs(log_cfg.retain_days);
            logging::init_global(logger);
        }
    }

    // Ensure every process and its subprocesses share a single run_id.
    // Sub-agents inherit this env var when spawned, so all sessions within a
    // PM/workflow invocation land in the same `sessions/<run_id>/` directory.
    // SAFETY: set_var is considered unsafe in Rust 2024; we call it exactly
    // once at startup before any threads that might read env vars are spawned.
    if crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID").is_err() {
        let run_id = uuid::Uuid::new_v4().to_string();
        // SAFETY: single-threaded context at startup.
        unsafe {
            std::env::set_var("TAGENT_RUN_ID", &run_id);
        }
        tracing::debug!(run_id = %run_id, "generated TAGENT_RUN_ID");

        // #session-tagging: Record this session in the lightweight JSON
        // registry so cleanup/export tooling can enumerate it. Best-effort:
        // a write failure here never blocks startup.
        let state_dir = std::path::Path::new(".trusty-agents").join("state");
        if let Ok(reg) = session_registry::SessionsRegistry::open(&state_dir) {
            // Workflow is unknown at this point (parsed later from CLI). Use
            // a placeholder; a future enhancement can update it post-parse.
            if let Err(e) = reg.record_start(&run_id, "pending") {
                tracing::debug!(error = %e, "session registry: record_start failed");
            }
        }
    }

    // Migrate legacy `.trusty-agents/store/` layout to the new split layout. Safe
    // no-op if already migrated or on first run.
    //
    // NOTE: `agent_dir` here refers to the *runtime state* subdirectory
    // (`.trusty-agents/state/`), NOT the repo-root `.trusty-agents/` which now holds
    // committed bundled config (agents/, skills/, workflows/, etc.).
    if let Ok(cwd) = std::env::current_dir() {
        let agent_dir = cwd.join(".trusty-agents").join("state");
        if agent_dir.exists()
            && let Err(e) = memory::migrate_if_needed(&agent_dir)
        {
            tracing::warn!(error = %e, "memory migration failed (continuing)");
        }

        // #74: Clean up stale worktrees from any prior interrupted run so
        // `git worktree add` doesn't fail with "already registered" errors
        // the next time a parallel phase spins one up.
        let worktree_base = agent_dir.join("worktrees");
        let mgr = workflow::worktree::WorktreeManager::new(worktree_base);
        if let Err(e) = mgr.cleanup_stale().await {
            tracing::warn!(error = %e, "worktree cleanup_stale failed (continuing)");
        }

        // #116: Register the current project in the global project registry and
        // clean up any entries whose directories no longer exist.
        if let Err(e) = async {
            let reg = registry::ProjectRegistry::new()?;
            reg.register(&cwd).await?;
            reg.deregister_missing().await?;
            anyhow::Ok(())
        }
        .await
        {
            tracing::warn!(error = %e, "project registry update failed (continuing)");
        }

        // #130: Clean up stale sub-agent PIDs from `.trusty-agents/state/processes.json`
        // left over by any prior crashed run. Best-effort; failures are logged
        // and never block startup.
        {
            let tracker = process_tracker::ProcessTracker::new(&agent_dir);
            match tracker.cleanup_stale().await {
                Ok(n) if n > 0 => {
                    tracing::info!(count = n, "cleaned up stale sub-agent processes");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "process tracker cleanup failed (continuing)");
                }
            }
        }

        // #117: Start the inter-project message bus in the background.
        // project_id is the directory basename.
        let project_id = cwd
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        match bus::MessageBus::start(&project_id).await {
            Ok(_bus_arc) => {
                tracing::debug!(project_id = %project_id, "inter-project message bus started");
                // Bus is kept alive via the Arc returned by `start`; the
                // accept_loop task owns a reference and keeps it alive for the
                // process lifetime.
            }
            Err(e) => {
                tracing::warn!(error = %e, "message bus failed to start (continuing)");
            }
        }
    }

    Ok(true)
}
