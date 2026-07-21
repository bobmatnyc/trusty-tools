//! `handle_start` — the main entry point for `trusty-search start`.
//!
//! Why: the boot sequence is intricate (lockfile probe, embedder, app state,
//! background tasks) and benefits from being isolated from the adapter /
//! restore machinery.
//!
//! What: probes the lockfile fast-path, constructs `SearchAppState`, kicks off
//! embedder init on a background task, launches warm-boot, schema migrations,
//! auto-discovery, and finally hands off to `run_daemon`.
//!
//! Test: run `trusty-search start` twice in a row — the second invocation
//! must exit 1 with the "another daemon is already running" message. Run with
//! `--data-dir /tmp/ts-x` and confirm the lockfile lands there. Run with
//! `--no-auto-discover` and verify no "auto-discover" log lines appear.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use colored::Colorize;

use super::embedder::build_embedder;
use super::restore::restore_indexes;
use crate::commands::prior_index_count::load_prior_index_count;

/// Main entry point for `trusty-search start`.
///
/// Why: extracted from `main()`. The boot sequence is intricate (lockfile
/// probe, embedder, app state) and benefits from being its own unit.
/// What: probes the lockfile fast-path, then constructs `SearchAppState` and
/// hands off to `run_daemon`. Maps `DaemonError::AlreadyRunning` to a friendly
/// exit-1 message. When `data_dir` is `Some`, the override is stamped into
/// `TRUSTY_DATA_DIR` for the foreground path AND passed as an explicit
/// `--data-dir` CLI arg to the background self-spawn (issue #1182: the explicit
/// flag now always wins over any pre-existing `TRUSTY_DATA_DIR` in the
/// environment). When `no_auto_discover` is `true` (or
/// `TRUSTY_NO_AUTO_DISCOVER=1` is set), the post-hydration auto-discovery scan
/// is skipped entirely so the daemon serves only indexes already in
/// `indexes.toml` or registered at runtime.
/// Test: run twice in a row — the second invocation must exit 1 with the
/// "another daemon is already running" message. Run with `--data-dir /tmp/ts-x`
/// and confirm the lockfile lands in `/tmp/ts-x/daemon.lock`. Unit coverage:
/// `spawn_args_include_data_dir_when_preset_env` in `tests/data_dir_forward.rs`.
#[allow(clippy::too_many_arguments)]
pub async fn handle_start(
    port: u16,
    foreground: bool,
    device: &str,
    data_dir: Option<&std::path::Path>,
    verbose: bool,
    no_auto_discover: bool,
    fanout_concurrency: Option<usize>,
    serial: bool,
) -> Result<()> {
    // Apply the --data-dir override as early as possible so the foreground path
    // sees the correct root in TRUSTY_DATA_DIR.  The background self-spawn path
    // receives the explicit --data-dir CLI arg directly (issue #1182) so the
    // CLI flag wins over any pre-set TRUSTY_DATA_DIR.  Precedence for the
    // foreground path: TRUSTY_DATA_DIR (env) > --data-dir (flag).
    //
    // SAFETY: `set_var` is sound here because nothing else reads/writes
    // process env concurrently yet — this runs before the daemon starts
    // accepting requests (the tokio runtime and its worker threads already
    // exist at this point, since `handle_start` is itself async; the
    // soundness argument is "no concurrent env access", not "no workers").
    if std::env::var_os("TRUSTY_DATA_DIR").is_none() {
        if let Some(dir) = data_dir {
            let dir = dir.to_path_buf();
            // Reject relative paths — a relative data-dir would resolve
            // differently depending on CWD, breaking daemon re-discovery.
            anyhow::ensure!(
                dir.is_absolute(),
                "--data-dir must be an absolute path (got: {})",
                dir.display()
            );
            // Create the directory now so the child daemon can acquire its
            // lockfile immediately on first start.
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("create --data-dir directory: {}", dir.display()))?;
            if std::fs::read_dir(&dir)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false)
            {
                tracing::warn!(
                    "--data-dir {} is empty (no existing indexes); daemon will start fresh",
                    dir.display()
                );
            }
            unsafe {
                std::env::set_var("TRUSTY_DATA_DIR", &dir);
            }
            tracing::info!("data-dir override: {}", dir.display());
        }
    }

    // Background self-spawn path: when invoked without `--foreground`, fork a
    // detached copy of ourselves with `--foreground` and return immediately.
    //
    // Why: prior to this, `run_daemon().await` blocked the current process
    // forever and held the controlling terminal. By detaching stdio to
    // `/dev/null` and returning, we let the caller (shell, make, tmux)
    // continue without owning the daemon's lifetime.
    //
    // launchd/systemd/Docker users (and `trusty-search service`) always pass
    // `--foreground` and stay on the inline path.
    if !foreground {
        // Validate `--device` here too so a typo doesn't silently spawn a
        // child that will then immediately exit non-zero.
        match device {
            "auto" | "cpu" | "gpu" => {}
            other => {
                anyhow::bail!("invalid --device value '{other}'; expected one of: auto, cpu, gpu")
            }
        }
        let exe = std::env::current_exe()
            .map_err(|e| anyhow::anyhow!("could not resolve current_exe: {e}"))?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("start")
            .arg("--foreground")
            .arg("--port")
            .arg(port.to_string())
            .arg("--device")
            .arg(device);
        if no_auto_discover {
            cmd.arg("--no-auto-discover");
        }
        // Issue #2845: forward the fan-out bounding flags to the foreground
        // child so the detached daemon honours them (mirrors --device).
        if serial {
            cmd.arg("--serial");
        }
        if let Some(n) = fanout_concurrency {
            cmd.arg("--fanout-concurrency").arg(n.to_string());
        }
        // Issue #1182: pass --data-dir explicitly so the CLI flag wins even
        // when TRUSTY_DATA_DIR was already set in the parent environment.
        // Relying solely on env inheritance loses the explicit flag when the
        // parent's TRUSTY_DATA_DIR differs from the requested --data-dir.
        if let Some(dir) = data_dir {
            cmd.arg("--data-dir").arg(dir);
        }
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("could not spawn detached daemon: {e}"))?;
        let pid = child.id();
        eprintln!(
            "{} Daemon starting in background (pid {pid}). Run `trusty-search status` to verify readiness.",
            "✓".green()
        );
        return Ok(());
    }

    // Issue #35: the foreground daemon owns tracing init so it can wire the
    // in-memory `LogBuffer` that backs `GET /logs/tail`.
    let (log_buffer, _error_store) = trusty_common::init_tracing_with_buffer_and_capture(
        if verbose { 2 } else { 0 },
        trusty_common::log_buffer::DEFAULT_LOG_CAPACITY,
        "trusty-search",
        env!("CARGO_PKG_VERSION"),
    );

    // Translate the `--device` CLI flag into the `TRUSTY_DEVICE` env var.
    //
    // SAFETY: invoked on the main thread before tokio spawns any workers.
    if std::env::var_os("TRUSTY_DEVICE").is_none() {
        match device {
            "cpu" | "gpu" => unsafe {
                std::env::set_var("TRUSTY_DEVICE", device);
            },
            "auto" => {}
            other => {
                anyhow::bail!("invalid --device value '{other}'; expected one of: auto, cpu, gpu")
            }
        }
    }

    // Issue #2845: translate the fan-out bounding flags into the
    // `TRUSTY_SEARCH_FANOUT_CONCURRENCY` env var the daemon reads per request.
    // `--serial` == concurrency 1 and wins over `--fanout-concurrency`.
    // An explicit env var set in the shell always wins (matches --device).
    //
    // SAFETY: `set_var` is sound here because nothing else reads/writes
    // process env concurrently yet — this runs before the daemon starts
    // accepting requests (the tokio runtime and its worker threads already
    // exist at this point, since `handle_start` is itself async; the
    // soundness argument is "no concurrent env access", not "no workers").
    if std::env::var_os("TRUSTY_SEARCH_FANOUT_CONCURRENCY").is_none() {
        let resolved = if serial {
            Some(1_usize)
        } else {
            fanout_concurrency.map(|n| n.max(1))
        };
        if let Some(n) = resolved {
            unsafe {
                std::env::set_var("TRUSTY_SEARCH_FANOUT_CONCURRENCY", n.to_string());
            }
            tracing::info!("fan-out concurrency cap set to {n} (serial={serial})");
        }
    }

    // Persist any memory-limit env vars set in the current shell so that
    // launchd restarts pick them up via `daemon.env`.
    crate::service::save_daemon_env();

    // Source daemon.env into the current environment (env var > file > default).
    crate::service::load_daemon_env();

    // Issue #747 Fix D: warn on stale TRUSTY_DEVICE=cpu (Apple Silicon).
    crate::commands::startup_checks::warn_if_stale_cpu_device_on_apple_silicon();

    // Auto-tune memory caps based on detected system RAM (issue: memory-tier
    // autosizing).
    //
    // SAFETY: invoked before tokio spawns any indexing workers.
    let policy = crate::core::MemoryPolicy::detect();

    // Hard 16 GB minimum: trusty-search is designed for developer workstations.
    const MIN_RAM_MB: u64 = 16 * 1024;
    if policy.total_ram_mb < MIN_RAM_MB
        && std::env::var("TRUSTY_SKIP_RAM_CHECK").as_deref() != Ok("1")
    {
        anyhow::bail!(
            "trusty-search requires at least 16 GB of RAM.\n\
             Detected: {} MB ({:.1} GB)\n\
             Indexing large codebases on machines with less memory is not supported.\n\
             To bypass on this host set TRUSTY_SKIP_RAM_CHECK=1 in the daemon environment.",
            policy.total_ram_mb,
            policy.total_ram_mb as f64 / 1024.0
        );
    }

    policy.log_summary();
    let _ = foreground;

    // Fast-path: bail before loading the 86 MB embedding model when
    // another daemon is already running.
    //
    // Issue #87 (Option A): if the lockfile exists and the recorded PID is
    // *alive*, print an actionable warning and exit 1.
    if let Some(pid) = crate::service::running_daemon_pid() {
        tracing::warn!("daemon already running (pid {pid}); refusing to start a second instance");
        anyhow::bail!(
            "Daemon already running (pid {pid}).\n\
             Stop it first with `trusty-search stop`, then re-run `trusty-search start`.\n\
             Replacing the binary while the daemon is running causes macOS to SIGKILL \
             the process (Code Signature Invalid)."
        );
    }

    // Issue #81: detect orphan daemons whose PIDs are NOT recorded in the
    // lockfile. Reap them now so we don't end up with two daemons fighting
    // over `bind_with_auto_port`.
    let orphans = crate::commands::stop::find_daemon_pids();
    if !orphans.is_empty() {
        tracing::warn!(
            "found {} existing trusty-search daemon process(es) not tracked by lockfile: {:?} — terminating before start",
            orphans.len(),
            orphans
        );
        eprintln!(
            "{} found {} existing trusty-search daemon process(es) not tracked by lockfile — stopping them first",
            "⚠".yellow(),
            orphans.len()
        );
        #[cfg(unix)]
        for pid in &orphans {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(*pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
        }
        // Give them 3 s to exit cleanly, then SIGKILL stragglers.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            #[cfg(unix)]
            let any_alive = orphans.iter().any(|p| {
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(*p as i32), None).is_ok()
            });
            #[cfg(not(unix))]
            let any_alive = false;
            if !any_alive || std::time::Instant::now() >= deadline {
                break;
            }
        }
        #[cfg(unix)]
        for pid in &orphans {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid as i32), None).is_ok() {
                tracing::warn!("orphan pid {pid} ignored SIGTERM — sending SIGKILL");
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(*pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        // Clear stale lock/port files left behind by the killed orphans.
        if let Ok(lock) = crate::service::daemon_lock_path() {
            let _ = std::fs::remove_file(&lock);
        }
        if let Some(port) = super::super::daemon_utils::daemon_port_path() {
            let _ = std::fs::remove_file(&port);
        }
    }

    // Construct `SearchAppState` immediately; kick off model loading on
    // a background task, and let `run_daemon` bind the HTTP port right away.
    // Handlers that need the embedder return `503 Service Unavailable` until
    // `state.install_embedder()` flips the watch channel.
    let cfg = crate::service::load_user_config();

    let metrics_state = match crate::service::metrics::install_recorder() {
        Ok(state) => Some(state),
        Err(e) => {
            tracing::warn!("could not install prometheus recorder: {e} (metrics disabled)");
            None
        }
    };

    let mut state =
        crate::service::SearchAppState::new(crate::core::registry::IndexRegistry::new())
            .with_local_model(cfg.local_model)
            .with_openrouter_model(cfg.openrouter_model)
            .with_openrouter_api_key(cfg.openrouter_api_key)
            .with_log_buffer(log_buffer);
    if let Some(m) = metrics_state {
        state = state.with_metrics(m);
    }

    // Issue #110 Phase 2: the embedder init runs on a dedicated background
    // task so the HTTP listener can bind and accept requests while the ONNX
    // model (or trusty-embedderd subprocess) is coming up.
    let init_timeout_secs: u64 = std::env::var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    let install_state = state.clone();
    tokio::spawn(async move {
        let runtime_handle = tokio::runtime::Handle::current();

        let init_handle = tokio::task::spawn_blocking(move || {
            // `build_embedder` is async; using `block_on` here is correct
            // because we're on a `spawn_blocking` thread (not a Tokio worker).
            runtime_handle.block_on(build_embedder())
        });

        let init_result =
            tokio::time::timeout(Duration::from_secs(init_timeout_secs), init_handle).await;

        match init_result {
            Ok(Ok(Ok((embedder, pid_slot, switchable)))) => {
                // Issue #282: if the sidecar path returned a PID slot, install
                // it on the state so `/health` and the reindex RSS poller can
                // read the child PID lock-free.
                if let Some(slot) = pid_slot {
                    install_state.install_embedderd_pid_slot(slot).await;
                }
                install_state.install_embedder(Arc::clone(&embedder)).await;
                // Epic #3524 slice 6: stash the concrete SwitchableEmbedder
                // handle so a later slice's hot-swap orchestrator and /health
                // work can reach `swap_to`/`active()` without downcasting the
                // trait object.
                install_state.install_switchable_embedder(switchable).await;
                // Issue #41: worker pool — priority-aware dispatch, bounded queue.
                let tracker = Arc::clone(&install_state.embedder_stall_tracker);
                use crate::service::embed_pool::EmbedPool;
                let base = EmbedPool::with_autotune(Arc::clone(&embedder));
                let pool = std::sync::Arc::new(base.with_stall_tracker(tracker));
                install_state.install_embed_pool(pool).await;
                tracing::info!("embedder ready — vector lane online");
                let prior = load_prior_index_count(); // Issue #873: FDA regression baseline.
                install_state
                    .prior_index_count
                    .store(prior, std::sync::atomic::Ordering::Relaxed);
                // Issue #85: restore every index recorded in `indexes.toml`.
                restore_indexes(&install_state, &embedder).await;
                // Issue #1670: boot-time stale-index reconciliation — for each
                // restored index, compare the stored indexed_head_sha against
                // the current git HEAD and reindex only what changed (or fall
                // back to a full background reindex if the delta is too large).
                // Gated by TRUSTY_NO_BOOT_RECONCILE=1.
                crate::service::reconcile::reconcile_stale_indexes(&install_state).await;
                // Schema migration: spawn a per-index background migration task.
                if std::env::var("TRUSTY_DISABLE_MIGRATIONS").as_deref() != Ok("1") {
                    let registry =
                        std::sync::Arc::new(crate::core::migration::MigrationRegistry::new());
                    for index_id in install_state.registry.list() {
                        let Some(handle) = install_state.registry.get(&index_id) else {
                            continue;
                        };
                        let reg = std::sync::Arc::clone(&registry);
                        tokio::spawn(async move {
                            if let Err(e) =
                                crate::core::migration::run_migrations(&handle, &reg).await
                            {
                                tracing::warn!(
                                    index_id = %handle.id,
                                    "schema migration failed (index kept at current schema): {e:#}"
                                );
                            }
                        });
                    }
                }
                // Issue #41 Phase 1: prime the `trusty_index_count` gauge.
                crate::service::metrics::set_index_count(install_state.registry.list().len());
                // Issue #40: auto-discover Claude Code / git projects.
                // Issue #314: skip when `--no-auto-discover` / `TRUSTY_NO_AUTO_DISCOVER=1`.
                if !no_auto_discover {
                    tokio::spawn(crate::commands::discover::auto_discover_and_index());
                } else {
                    tracing::info!(
                        "auto-discover: disabled via --no-auto-discover / TRUSTY_NO_AUTO_DISCOVER"
                    );
                }
            }
            Ok(Ok(Err(e))) => {
                let msg = format!("embedder init failed: {e}");
                install_state.install_embedder_error(msg.clone()).await;
                eprintln!(
                    "{} embedder failed to initialize: {e}\n\
                     Daemon is up but running BM25-only. Check the model cache at \
                     ~/Library/Caches/trusty-search/models/ and network access.",
                    "✗".red()
                );
            }
            Ok(Err(join_err)) => {
                let msg = format!("embedder init task panicked: {join_err}");
                install_state.install_embedder_error(msg).await;
            }
            Err(_elapsed) => {
                // Timeout fired. The blocking init thread is intentionally
                // leaked — we cannot safely cancel native ORT code.
                tracing::error!(
                    "embedder init timed out after {init_timeout_secs}s — \
                     ONNX session did not start (try \
                     TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS=120 or check ORT \
                     compatibility, see GitHub #121)"
                );
                let msg = format!("init timed out after {init_timeout_secs}s");
                install_state.install_embedder_error(msg).await;
                eprintln!(
                    "{} embedder init timed out after {init_timeout_secs}s — \
                     see GitHub #121. Daemon is up in BM25-only mode.",
                    "✗".red()
                );
            }
        }
    });

    // Issue #537: throttled startup update check. Runs once per 24h (on-disk
    // cache). Non-blocking: failure degrades to "no update info".
    {
        let update_available = state.update_available.clone();
        tokio::spawn(async move {
            let crate_name = env!("CARGO_PKG_NAME");
            let current = env!("CARGO_PKG_VERSION");
            if let Some(info) = trusty_common::update::check_throttled(crate_name, current).await {
                tracing::info!(
                    latest = %info.latest,
                    "update available: {}",
                    trusty_common::update::notice(&info)
                );
                eprintln!("{}", trusty_common::update::notice(&info));
                if let Ok(mut guard) = update_available.lock() {
                    *guard = Some(info.latest);
                }
            }
        });
    }

    // The auto-spawned trusty-embedderd subprocess needs no explicit shutdown
    // hook: on the ordinary `run_daemon` `Err`/`bail!` paths below the
    // supervisor task's `kill_on_drop(true)` Child reaps it when dropped.
    // Issue #1746: the graceful-exit path below this match instead calls
    // `std::process::exit(0)`, which bypasses `Drop` (and `kill_on_drop`)
    // entirely — on that path the sidecar is reaped by the stdin-EOF path in
    // `trusty-embedderd`'s own stdio loop (`stdio_server::run_stdio_server`,
    // `crates/trusty-embedderd/src/stdio_server.rs:57-60`): when this process
    // dies, the OS closes its end of the sidecar's stdin pipe, `read_line`
    // returns `Ok(0)`, and the sidecar exits cleanly on its own.
    match crate::service::run_daemon(state, port).await {
        Ok(()) => {}
        Err(crate::service::DaemonError::AlreadyRunning(p)) => {
            // Issue #126: launchd respawns `start` every ~30 s while the
            // daemon is up. Exit non-zero so `SuccessfulExit=false` stops
            // the respawn loop. Suppress the stderr message (demote to debug).
            tracing::debug!(
                "daemon already running (lock at {}), exiting non-zero to stop launchd respawn",
                p.display()
            );
            std::process::exit(1);
        }
        Err(e) => anyhow::bail!("daemon failed: {e}"),
    }
    // Issue #1746: by the time `run_daemon` returns `Ok`, the graceful drain,
    // best-effort per-index flush, and PID-lockfile release have all completed.
    // Exit the process immediately instead of returning up through `main` and
    // dropping the tokio runtime: a worker left stuck in a blocking HNSW
    // `Index::save` FFI call, or a CUDA/embedder teardown that deadlocks at
    // exit, would otherwise wedge runtime teardown until systemd's
    // `TimeoutStopSec` (300 s) fired a SIGKILL. See the comment above this
    // match for how the embedder sidecar is still reaped on this path despite
    // `exit(0)` bypassing `Drop`.
    std::process::exit(0)
}
