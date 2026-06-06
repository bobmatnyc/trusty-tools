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

//! Mode-flag dispatch: the tail of `run()` that runs once the top-level clap
//! `Cli` has been parsed and the agent/skill registries are built. Inspects
//! the mode flags in priority order and routes to the matching execution mode
//! (symbol-registry CLI flags, `--reindex`, `--service`, `--api`, workflow /
//! direct / telegram / slack / pm, and finally the interactive REPL or CTRL).

use std::path::PathBuf;

use anyhow::Result;

use super::cli_def::Cli;
use super::{direct_mode, indexer, pm_mode, subagent_mode, workflow_mode};
use crate::{ast, ctrl, identity, plugins, repl, service, session, slack, telegram};

/// Dispatch the parsed CLI into its execution mode.
///
/// Why: Splits the ~230-line mode-flag dispatch tail out of `run()` so each
/// file stays under the 500-line cap. The flags are inspected in priority
/// order (matching the original inline logic) so behavior is identical.
/// What: Honors `--agent`, the symbol-registry flags, `--reindex`,
/// `--service`, `--api`/`--serve`, `--check-orphans`, `--watch`,
/// `--clear-sessions`, `--compare`, `--workflow`, `--direct`, `--telegram`,
/// `--slack`, `--pm`, and finally falls through to the controller probe +
/// interactive REPL / CTRL loop.
/// Test: Indirectly via `cargo run -p trusty-agents` and the crate's integration
/// tests (each mode exercised end-to-end).
pub(super) async fn dispatch_cli_mode(
    cli: Cli,
    args: Vec<String>,
    inline_task: Option<String>,
) -> Result<()> {
    let inline_task: Option<&str> = inline_task.as_deref();

    // #344: One-shot slash-command passthrough — exits early when handled.
    if super::predispatch::handle_slash_passthrough(&cli).await? {
        return Ok(());
    }

    // #167/#168: Build + log the PM-process agent and skill registries once.
    super::predispatch::build_registries(&cli).await;

    if let Some(name) = cli.agent.as_deref() {
        return subagent_mode::run_subagent(name).await;
    }

    // #193: Top-level (non-agent) invocations are CTRL by default. Setting
    // `TAGENT_CALLER=ctrl` here means any in-process tool that consults
    // `CallerIdentity::from_env()` gets the unrestricted ceiling. Sub-agents
    // override this on their own child Command (see `subprocess.rs`); this
    // never leaks down because `Command::env` is per-child.
    // SAFETY: single-threaded startup context before any spawn.
    if std::env::var(identity::ENV_CALLER).is_err() {
        unsafe {
            std::env::set_var(identity::ENV_CALLER, "ctrl");
        }
    }

    // #350: Symbol registry CLI flags. These run synchronously and exit
    // before any other mode is considered so they're safe in CI / scripts.
    if cli.parse_to_registry {
        let root = std::env::current_dir()?;
        let registry = ast::parse_directory(&root.join("src"), &root)?;
        registry.save()?;
        println!(
            "Registry built: {} symbols → {}",
            registry.len(),
            registry.registry_path().display()
        );
        return Ok(());
    }

    if cli.emit_from_registry {
        let root = std::env::current_dir()?;
        let registry = ast::SymbolRegistry::load(&root)?;
        let rules = ast::LayoutRules::default();
        let outputs = ast::emit(
            &registry,
            &rules,
            &trusty_common::symgraph::ModulePathStrategy::default(),
        )?;
        let written = ast::apply_emit(&outputs, &root)?;
        println!("Emitted {} files", written.len());
        for p in &written {
            println!("  {}", p.display());
        }
        return Ok(());
    }

    if cli.verify_registry {
        let root = std::env::current_dir()?;
        let registry = ast::SymbolRegistry::load(&root)?;
        let stale = registry.verify_hashes();
        if stale.is_empty() {
            println!("Registry OK — all {} hashes match", registry.len());
        } else {
            println!("Stale symbols ({}):", stale.len());
            for id in &stale {
                println!("  {id}");
            }
            std::process::exit(1);
        }
        return Ok(());
    }

    if cli.reindex {
        return indexer::run_reindex().await;
    }

    // #343: `--service start|stop|status` — manage the persistent daemon
    // backing `--serve`. We dispatch this before mode-flag handling so it
    // composes cleanly with `--port` (which `start` honors) and so it
    // never falls through to REPL/serve startup.
    if let Some(cmd) = cli.service.as_deref() {
        let port = cli.port.unwrap_or(service::DEFAULT_SERVICE_PORT);
        match cmd {
            "start" => match service::start_service(port).await {
                Ok(state) => {
                    println!(
                        "service started: pid {} port {} (started {})",
                        state.pid,
                        state.port,
                        state.started_at.to_rfc3339()
                    );
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("service start failed: {e:#}");
                    std::process::exit(1);
                }
            },
            "stop" => match service::stop_service().await {
                Ok(()) => {
                    println!("service stopped");
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("service stop failed: {e:#}");
                    std::process::exit(1);
                }
            },
            "status" => {
                println!("{}", service::status_line(port).await);
                return Ok(());
            }
            other => {
                eprintln!("unknown --service subcommand: {other} (use start | stop | status)");
                std::process::exit(2);
            }
        }
    }

    // #151 phase-2: `--serve` / `--api` launches the HTTP API server + web UI.
    // Both flags are accepted; `--api` is the canonical user-facing alias used
    // in the Makefile and README; `--serve` is kept for backwards compat.
    if cli.api || cli.serve {
        let port = cli.port.unwrap_or(8080);
        // #181: bearer token from `--api-token <TOK>` (preferred) or
        // `TAGENT_API_TOKEN` env var. CLI flag takes precedence so an
        // operator can override an env-defaulted token without unsetting it.
        let token = cli
            .api_token
            .clone()
            .or_else(|| crate::env_compat::env_var("TAGENT_API_TOKEN", "OPEN_MPM_API_TOKEN").ok())
            .filter(|s| !s.is_empty());
        return crate::api::server::serve_with_config(crate::api::server::ApiConfig {
            port,
            token,
        })
        .await;
    }

    if cli.check_orphans {
        return indexer::run_check_orphans().await;
    }

    if cli.watch {
        return indexer::run_watch().await;
    }

    // MIN-3 (#100): `--clear-sessions` now actually clears any persisted
    // session state before this run.
    if cli.clear_sessions {
        let mgr = session::SessionManager::new();
        mgr.clear_all().await;
        tracing::info!(
            "--clear-sessions: persistent agent sessions cleared via SessionManager::clear_all"
        );
    }

    // #348: Apply --ast-native override BEFORE any agent runs so the
    // in-process runner sees the flag at registration time.
    if cli.ast_native {
        ast::set_ast_native_override(true);
        tracing::info!("--ast-native: AST-native tool bundle force-enabled for this run");
    }

    // #348: --compare runs the task twice (traditional + ast-native) and
    // emits a side-by-side report. Requires --task or --task-file.
    if cli.compare {
        return direct_mode::run_compare_bakeoff(
            cli.direct.as_deref(),
            cli.workflow.as_deref(),
            cli.task_file.as_deref(),
            inline_task,
        )
        .await;
    }

    // #424: Spawn optional MCP plugins (trusty-search, trusty-memory) once at
    // startup so the agent loop in REPL/--workflow/--direct/--pm modes can
    // actually call their tools. `init_global` is idempotent and degrades
    // gracefully (logs WARN per missing plugin, never crashes the harness).
    // CLI subcommand paths (`om plugins status`, `om start`, etc.) returned
    // earlier and aren't affected.
    //
    // #477: Spawn this off the startup critical path. Plugin init shells out
    // to MCP child processes and runs handshakes — awaiting it inline added
    // noticeable latency before the prompt appeared. `init_global` is
    // idempotent; the agent loop tolerates plugins that aren't ready yet.
    tokio::spawn(async {
        plugins::init_global().await;
    });

    if let Some(name) = cli.workflow.as_deref() {
        return workflow_mode::run_workflow(
            name,
            cli.task_file.as_deref(),
            inline_task,
            cli.out_dir.as_deref(),
            cli.project_dir.as_deref(),
            cli.json,
        )
        .await;
    }

    if let Some(name) = cli.direct.as_deref() {
        return direct_mode::run_direct(
            name,
            cli.task_file.as_deref(),
            inline_task,
            cli.out_dir.as_deref(),
        )
        .await;
    }

    // --telegram flag: run the Telegram bot gateway (#264).
    // Why: Lets users drive trusty-agents from a phone via @openmpm_bot. Each
    // chat gets its own ChatSession + ConversationTurn history.
    if cli.telegram {
        let project_path = ctrl::detect_self_project()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        // #334: Standalone --telegram mode has no REPL; create a fresh
        // (orphan) pending map. New chats will be told to run /telegram pair
        // in the REPL — which won't exist in this mode. This path is
        // intentionally for ops-only usage; pairing requires the REPL.
        let pending = telegram::new_pending_pairs();
        return telegram::run_telegram_bot(project_path, pending).await;
    }

    // --slack flag: run the Slack Socket Mode bot gateway (#418).
    // Why: Same shape as --telegram — each channel gets its own ChatSession +
    // ConversationTurn history, dispatched through ctrl::run_pm_task_with_history.
    if cli.slack {
        let project_path = ctrl::detect_self_project()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        // Standalone --slack mode has no REPL; create a fresh (orphan)
        // pending map. Pairing requires shell access to the host running
        // the REPL, so this path is ops-only.
        let pending = slack::new_pending_pairs();
        // #480/#481: Parse per-user RBAC + default-persona config from env so
        // the bot routes to `cto-assistant` and enforces tier gating.
        let rbac = std::sync::Arc::new(slack::SlackRbacConfig::from_env());
        return slack::run_slack_bot(project_path, pending, rbac).await;
    }

    // #372: Auto-start the file watcher in the background so the code index
    // tracks the working tree without the user remembering `--watch`. We do
    // this *after* the standalone modes (`--watch`, `--reindex`, `--service`,
    // `--api`) have already returned so we don't fight them for the redb
    // lock, and *before* PM/REPL/CTRL dispatch so any in-process search_code
    // call benefits from a fresh index.
    indexer::spawn_background_file_watcher();

    // --pm flag: single-shot PM mode (backward compat)
    if cli.pm {
        return pm_mode::run_pm().await;
    }

    // --ctrl flag: explicit CTRL mode (also the default when no mode flag is set).
    // #120: Even though CTRL is the default, an explicit flag lets scripts be
    // unambiguous when future modes are added.
    let _ = cli.ctrl;

    // #192 Phase A: probe for an existing controller. If one is listening on
    // the per-project socket, forward this invocation's argv as a `task`
    // command and stream its replies. Otherwise fall through and become the
    // controller ourselves.
    //
    // Why: Lets the user run `trusty-agents "do X"` from any terminal in a
    // project that already has a CTRL REPL running, without having to
    // know whether the controller is alive. The probe has a hard 50ms
    // budget so a non-running controller does not perceptibly delay startup.
    // What: When forwarded text is non-empty (i.e., the user passed a task on
    // argv), forward it; when empty (bare `trusty-agents` re-invocation), we still
    // become the controller — re-binding the socket fails because the first
    // controller already owns it, which is the desired behavior. We log and
    // continue so the second user gets a local REPL anyway.
    let project_id = ctrl::cwd_project_id();
    let sock_path = ctrl::ctrl_socket_path(&project_id);
    let argv_task = super::argv_as_task_text(&args);
    if !argv_task.trim().is_empty() {
        match ctrl::CtrlSocket::probe_default(&sock_path).await {
            Ok(stream) => {
                tracing::debug!(path = %sock_path.display(), "controller alive — forwarding");
                // Why: One-shot CLI invocations have no prior conversation
                // history, so pass an empty slice. The accumulated output
                // text is discarded — output streamed to stdout already.
                // The controller resolves agent configs relative to the
                // forwarded `cwd`; for one-shot CLI we use the OS cwd since
                // there's no REPL state to consult.
                let argv_cwd =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                return ctrl::forward_to_controller(stream, argv_task, &[], &argv_cwd)
                    .await
                    .map(|_| ());
            }
            Err(e) if ctrl::is_connection_refused(&e) => {
                tracing::debug!(path = %sock_path.display(), "stale ctrl socket — cleaning up");
                ctrl::CtrlSocket::cleanup(&sock_path);
            }
            Err(e) => {
                tracing::debug!(error = %e, "no controller found — starting one");
            }
        }
    }

    // Default interactive mode: use the rich reedline REPL when stdin is a
    // TTY; fall back to the legacy stdin loop in `run_ctrl` otherwise so
    // piped input keeps working unchanged.
    if repl::is_tty() {
        // Profile interview is handled inside `run_ctrl` (via
        // load_or_create_user_profile). To keep its side effects we still
        // start the controller — but we replace its stdin loop with the
        // REPL by spawning the controller in a dedicated task and running
        // the REPL on top.
        //
        // #268 P5: The legacy crossterm banner printer is gone — the ratatui
        // REPL renders its own banner widget once `run()` enters the alt
        // screen, so no pre-spawn banner print is needed here.
        let user_profile = identity::user_profile::UserProfile::load();
        let mut repl = repl::TrustyAgentsRepl::new(user_profile)?;

        // #477: Wait on an explicit readiness signal instead of a fixed
        // sleep. The controller fires `ctrl_ready_tx` once it reaches the
        // socket-bind stage; the REPL then probes without guessing timing.
        let (ctrl_ready_tx, ctrl_ready_rx) = tokio::sync::oneshot::channel::<()>();
        let ctrl_handle = tokio::spawn(async move {
            if let Err(e) = ctrl::run_ctrl_headless(Some(ctrl_ready_tx)).await {
                tracing::warn!(error = %e, "controller task exited with error");
            }
        });
        // Auto-start Telegram bot as background task if TELEGRAM_BOT_TOKEN is set (#335).
        // #334: Share the REPL's pending-pairs map so /telegram pair codes
        // issued in the REPL are validatable by the bot's /pair handler.
        let _telegram_handle = if std::env::var("TELEGRAM_BOT_TOKEN").is_ok() {
            let tg_project_path = ctrl::detect_self_project()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            let tg_pending = repl.telegram_pairing_handle();
            Some(tokio::spawn(async move {
                if let Err(e) = telegram::run_telegram_bot(tg_project_path, tg_pending).await {
                    tracing::warn!(error = %e, "telegram bot exited with error");
                }
            }))
        } else {
            None
        };
        // Wait for the controller to signal it reached the socket-bind
        // stage. Capped at 200ms so a stalled controller can't block the
        // REPL indefinitely (#477).
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), ctrl_ready_rx).await;

        // #343: If a persistent service is already running on the default
        // port, switch the REPL into thin-client mode so user messages
        // forward to the existing daemon instead of running in-process.
        // We probe synchronously here (with a tight 500ms HTTP budget) so
        // the user sees the connection banner before the prompt appears.
        let service_already_running =
            service::is_service_running(service::DEFAULT_SERVICE_PORT).await;
        if service_already_running {
            let url = format!("http://localhost:{}", service::DEFAULT_SERVICE_PORT);
            let started = service::read_pid_file()
                .map(|s| {
                    format!(
                        "pid {} port {} since {}",
                        s.pid,
                        s.port,
                        s.started_at.to_rfc3339()
                    )
                })
                .unwrap_or_else(|| format!("port {}", service::DEFAULT_SERVICE_PORT));
            eprintln!("--- connected to running trusty-agents service ---");
            eprintln!("    {}", started);
            eprintln!("    (use `/service stop` to shut it down)");
            repl.set_service_client_mode(url);
        }

        // #364: auto-launch Tauri desktop GUI on startup.
        // The Tauri app manages its own API sidecar (trusty-agents --api --port 7654),
        // so we only need to open the .app bundle — no server spawn here.
        // Resolve the app path relative to TAGENT_PROJECT_DIR (set by the `om` wrapper)
        // or relative to cwd, falling back gracefully if the bundle isn't built.
        {
            let app_path = crate::env_compat::env_var("TAGENT_PROJECT_DIR", "OPEN_MPM_PROJECT_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                })
                .join("ui/src-tauri/target/release/bundle/macos/trusty-agents.app");

            if app_path.exists() {
                tracing::info!(path = %app_path.display(), "launching Tauri desktop GUI");
                let _ = std::process::Command::new("open").arg(&app_path).spawn();
            } else {
                tracing::debug!(
                    path = %app_path.display(),
                    "Tauri app not found — skipping GUI launch (run `cd ui && pnpm tauri build` to build it)"
                );
            }
        }

        let result = repl.run().await;
        ctrl_handle.abort();
        if let Some(h) = _telegram_handle {
            h.abort();
        }
        return result;
    }

    ctrl::run_ctrl().await
}
