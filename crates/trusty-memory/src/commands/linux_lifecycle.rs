//! Linux `start`, `stop` and `restart` for trusty-memory (#11085, epic #11083).
//!
//! Why: on Linux, `stop` scanned argv, which cannot tell one data dir's daemon
//! from another's, and there was no `restart`, so `tctl restart` could not put
//! a daemon on a newly installed binary. D2 keeps macOS unchanged, so this file
//! is the Linux path only; it hands every rule — identity by `SO_PEERCRED`,
//! SIGTERM then SIGKILL, "stopped" proven, spawn from the install dir — to
//! `trusty_common::linux_lifecycle` and supplies only the descriptor.
//! What: [`descriptor`] builds the daemon's descriptor; [`stop`], [`start`] and
//! [`restart`] run the module and turn its outcome into the CLI's exit status.
//! `stop` on a daemon that is not running exits 1; `restart` starts it (P-b).
//! `start` and `restart` hold `start.lock`, the lock a `serve --stdio` bridge
//! takes before it auto-starts the daemon, so no bridge can spawn an old binary
//! in the gap between the stop and the start (#5267).
//! Test: `linux_lifecycle_tests.rs`.

use std::ffi::OsString;

use anyhow::{Context as _, Result, bail};
use colored::Colorize;
use trusty_common::DATA_DIR_OVERRIDE_ENV;
use trusty_common::linux_lifecycle::{
    self as lc, DaemonDescriptor, KernelDelivery, LifecycleError, SignalDelivery, StopOutcome,
    StoppedSignal,
};

/// The executable the module spawns from the install dir and requires of the
/// socket's peer.
pub(crate) const BINARY: &str = "trusty-memory";

/// The daemon's argv after `argv[0]`: the spawn args, and the liveness args
/// that tell it from a `serve --stdio` bridge of the same binary.
pub(crate) const DAEMON_ARGS: [&str; 2] = ["serve", "--foreground"];

/// Caller variables the daemon still needs once the module clears its
/// environment (#11098), beyond `DaemonDescriptor::BASE_ENV`.
///
/// Why: a bridge auto-start inherits the caller's whole environment, so a
/// variable missing here makes a `start`/`restart` daemon behave differently
/// from a bridge-started one (#11085: `TRUSTY_MEMORY_MULTI_TENANT` off, the
/// other embedder model). Register here every NON-credential variable a new
/// `std::env::var` read in the daemon path adds — this crate's `serve` code or
/// the trusty-common code it calls. Credentials (`OPENROUTER_API_KEY` and any
/// `*_KEY`/`*_TOKEN`/`*_SECRET`) never go here; the daemon reads them from the
/// `.env.local` tiers and the credential store instead.
/// Test: `descriptor_passes_the_security_and_data_variables_but_no_credential`.
pub(crate) const ENV_PASSTHROUGH: &[&str] = &[
    // Process and network: logging, proxies and CA bundles for model
    // downloads, the session bus for the Secret Service credential store,
    // `CARGO_HOME` for the install-dir lookup, and the ORT/CUDA loader paths.
    "RUST_LOG",
    "RUST_BACKTRACE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "CARGO_HOME",
    "ORT_DYLIB_PATH",
    // #11085: the loader path ORT's CUDA provider libraries resolve through.
    "LD_LIBRARY_PATH",
    // #11085: trusty-common, embedder (`embedder::{fast_embedder,types,chunk}`).
    "TRUSTY_EMBEDDER_MODEL",
    "TRUSTY_DEVICE",
    "FASTEMBED_CACHE_DIR",
    "FASTEMBED_CACHE_PATH",
    "HF_HOME",
    "TRUSTY_EMBED_ONNX_BATCH",
    "TRUSTY_ORT_INTRA_THREADS",
    "TRUSTY_ORT_INTER_THREADS",
    "TRUSTY_ORT_ALLOW_SPINNING",
    "TRUSTY_GPU_MEM_LIMIT_BYTES",
    "TRUSTY_GPU_MEM_LIMIT_MB",
    // #11085: trusty-common, storage, shutdown, bug capture, update check and
    // the local model probe (`bm25`, `redb_cache`, `shutdown`, `error_capture`,
    // `update`, `local_probe`).
    "TRUSTY_BM25_CORPUS_CAP",
    "TRUSTY_MEMORY_REDB_CACHE_MB",
    "TRUSTY_TERMINATION_GRACE_SECS",
    "TRUSTY_NO_BUG_CAPTURE",
    "TRUSTY_NO_UPDATE_CHECK",
    "CI",
    "OLLAMA_HOST",
    // #11085: this crate, security and tenancy (`lib.rs`, `tools::palace_ops`).
    "TRUSTY_MEMORY_MULTI_TENANT",
    "TRUSTY_SKIP_PALACE_ENFORCEMENT",
    "TRUSTY_MEMORY_RULINGS_PALACES",
    // #11085: this crate, BM25 lane and repair.
    "TRUSTY_BM25_DAEMON",
    "TRUSTY_BM25_MAX_PALACES",
    "TRUSTY_BM25_TEXT_BUDGET_MB",
    "TRUSTY_BM25_NO_BACKFILL",
    "TRUSTY_BM25_REPAIR_INTERVAL_SECS",
    // #11085: this crate, palace and session-store residency.
    "TRUSTY_MEMORY_MAX_OPEN_PALACES",
    "TRUSTY_MEMORY_MAX_OPEN_SESSION_STORES",
    "TRUSTY_MEMORY_IDLE_EVICT_SECS",
    "TRUSTY_MEMORY_STARTUP_OPEN_LIMIT",
    // #11085: this crate, timeouts and budgets (`engine::timeouts` and the
    // HNSW op budget, worker liveness).
    "TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS",
    "TRUSTY_EMBED_BATCH_TIMEOUT_SECS",
    "TRUSTY_EMBED_CHUNKED_CAP_SECS",
    "TRUSTY_OPEN_QUEUE_TIMEOUT_SECS",
    "TRUSTY_SLOW_WRITE_WARN_SECS",
    "TRUSTY_WRITE_LOCK_TIMEOUT_SECS",
    "TRUSTY_WRITE_OP_BUDGET_SECS",
    "TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS",
    "TRUSTY_WRITE_TXN_DEADLINE_SECS",
    "TRUSTY_DREAM_PERMIT_WAIT_SECS",
    "TRUSTY_HNSW_OP_BUDGET_SECS",
    "TRUSTY_WEDGE_THRESHOLD_SECS",
    // #11085: this crate, background work (dream and chunk backfill tickers).
    "TRUSTY_DREAM_DISABLED",
    "TRUSTY_DREAM_MAX_CONCURRENT",
    "TRUSTY_CHUNK_BACKFILL_DISABLED",
    "TRUSTY_CHUNK_BACKFILL_BACKOFF_SECS",
    "TRUSTY_CHUNK_BACKFILL_BUSY_SECS",
    "TRUSTY_CHUNK_BACKFILL_EMBED_SECS",
    "TRUSTY_CHUNK_BACKFILL_ERROR_BACKOFF_SECS",
    "TRUSTY_CHUNK_BACKFILL_HOST_FAILURES",
    "TRUSTY_CHUNK_BACKFILL_IDLE_SECS",
    "TRUSTY_CHUNK_BACKFILL_LOCK_WAIT_SECS",
    "TRUSTY_CHUNK_BACKFILL_MAX_CHUNKS",
    "TRUSTY_CHUNK_BACKFILL_MAX_DRAWERS",
    "TRUSTY_CHUNK_BACKFILL_MIN_AGE_SECS",
    "TRUSTY_CHUNK_BACKFILL_PROBE_SECS",
    "TRUSTY_CHUNK_BACKFILL_SLICE_MS",
    "TRUSTY_CHUNK_BACKFILL_TICK_MS",
];

/// The trusty-memory descriptor for this process's environment.
///
/// Why: the socket derives from the data dir, so it names the instance; the
/// module's default `instance_env` (`TRUSTY_DATA_DIR_OVERRIDE`) is the only
/// variable that moves it.
/// What: [`descriptor_for`] with the caller's override and socket.
/// Test: `descriptor_names_the_daemon_args_and_the_socket_stop_signal`.
pub(crate) fn descriptor() -> Result<DaemonDescriptor> {
    let socket = crate::transport::uds::socket_path()?;
    descriptor_for(std::env::var_os(DATA_DIR_OVERRIDE_ENV).as_ref(), socket)
}

/// [`descriptor`] with the override value and socket supplied.
///
/// Why: #11085 — the module compares `environ` bytes exactly, so a blank
/// override is a different instance from an unset one, while
/// `resolve_data_dir` reads both as the live data dir. A blank override would
/// therefore address the live socket yet miss the live daemon in the module's
/// `/proc` scan, and `restart` could start a second daemon beside it. It is
/// refused, as `stop` under an override already refuses it (#9140).
/// What: `serve --foreground` as spawn and liveness args, the socket-refuses
/// stopped signal, [`ENV_PASSTHROUGH`], and the install dir from
/// `canonical_bin_dir`.
///
/// # Errors
///
/// A blank or non-UTF-8 override, or no resolvable install dir.
///
/// Test: `descriptor_names_the_daemon_args_and_the_socket_stop_signal`,
/// `descriptor_refuses_a_blank_data_dir_override`,
/// `descriptor_refuses_a_non_utf8_data_dir_override`.
pub(crate) fn descriptor_for(
    override_value: Option<&OsString>,
    socket: std::path::PathBuf,
) -> Result<DaemonDescriptor> {
    // #11085: `resolve_data_dir` reads a non-UTF-8 value as unset, so it too
    // names the live data dir while its raw bytes miss the live daemon.
    match override_value.map(|v| v.to_str()) {
        Some(None) => bail!(
            "{DATA_DIR_OVERRIDE_ENV} is not valid UTF-8, which resolves the live data dir; \
             refusing to act on it. Unset it to manage the live daemon (#11085)"
        ),
        Some(Some(v)) if v.trim().is_empty() => bail!(
            "{DATA_DIR_OVERRIDE_ENV} is set but blank, which resolves the live data dir; \
             refusing to act on it. Unset it to manage the live daemon (#11085)"
        ),
        _ => {}
    }
    let mut desc = DaemonDescriptor::new(BINARY, socket, StoppedSignal::SocketRefuses)
        .context("build the trusty-memory lifecycle descriptor")?;
    desc.spawn_args = DAEMON_ARGS.map(String::from).to_vec();
    desc.liveness_args = DAEMON_ARGS.map(String::from).to_vec();
    desc.env_passthrough = ENV_PASSTHROUGH.iter().map(|s| (*s).to_string()).collect();
    Ok(desc)
}

/// `trusty-memory stop` on Linux.
///
/// What: stops only the daemon serving this data dir's socket; exits 1 when
/// none runs, and when any process survives. Removes the stale `http_addr` on
/// a clean stop.
/// Test: `stop_report_maps_each_outcome_to_its_exit_status`.
pub async fn stop() -> Result<()> {
    let desc = descriptor()?;
    let stopped = stop_report(lc::stop(&desc).await)?;
    // The daemon does not remove `http_addr` on SIGKILL; a client must not
    // chase the dead address (as `stop::cleanup_addr_file` does on macOS).
    if stopped && let Ok(dir) = trusty_common::resolve_data_dir(BINARY) {
        let _ = std::fs::remove_file(dir.join("http_addr"));
    }
    Ok(())
}

/// `trusty-memory start` on Linux.
///
/// What: `start.lock` held, the install-dir binary spawned and waited on until
/// it serves; an already-serving daemon is success, as it was before.
/// Test: `start_report_treats_only_already_running_as_success`.
pub async fn start() -> Result<()> {
    let desc = descriptor()?;
    let _lock = take_start_lock().await?;
    start_report(lc::start(&desc).await)
}

/// `trusty-memory restart` on Linux (D1, P-b).
///
/// What: [`restart_with`] through the kernel.
/// Test: `restart_report_names_a_survivor_and_starts_nothing`.
pub async fn restart() -> Result<()> {
    restart_with(&descriptor()?, &KernelDelivery).await
}

/// [`restart`] with the descriptor and signal delivery supplied.
///
/// What: `start.lock` held across the stop and the start; any stop error is
/// returned before a spawn, and `NotRunning` proceeds to the start.
/// Test: `restart_report_names_a_survivor_and_starts_nothing`.
pub(crate) async fn restart_with(
    desc: &DaemonDescriptor,
    delivery: &dyn SignalDelivery,
) -> Result<()> {
    let _lock = take_start_lock().await?;
    restart_report(lc::restart_with(desc, delivery).await)
}

/// The exit status of a stop: `Ok(true)` stopped, `Ok(false)` never returned.
///
/// Why: P-b keeps `stop` exiting 1 when nothing runs, so scripts can tell
/// "stopped" from "nothing to stop"; a survivor is an error, never a warning.
/// What: `Stopped` prints and is `Ok(true)`; `NotRunning` is "No daemon
/// running"; every module error is an error that names its pid.
/// Test: `stop_report_maps_each_outcome_to_its_exit_status`.
pub(crate) fn stop_report(result: Result<StopOutcome, LifecycleError>) -> Result<bool> {
    match result.context("trusty-memory stop")? {
        StopOutcome::Stopped { pid, forced } => {
            let how = if forced { " (SIGKILL)" } else { "" };
            println!("{} Daemon stopped (pid {pid}){how}", "✓".green());
            Ok(true)
        }
        StopOutcome::NotRunning => bail!("No daemon running"),
        other => bail!("trusty-memory stop: unexpected outcome {other:?}"),
    }
}

/// The exit status of a start.
///
/// Why: a second `start` while the daemon serves was a no-op before #11085 and
/// stays one; every other module error is a failed start. This is the only arm
/// here that turns a module error into success.
/// Test: `start_report_treats_only_already_running_as_success`.
pub(crate) fn start_report(result: Result<lc::StartOutcome, LifecycleError>) -> Result<()> {
    match result {
        Ok(started) => {
            eprintln!(
                "{} trusty-memory daemon ready (pid {})",
                "✓".green(),
                started.pid
            );
            Ok(())
        }
        Err(LifecycleError::AlreadyRunning { socket, .. }) => {
            eprintln!(
                "{} trusty-memory is already running on {}",
                "◉".green(),
                socket.display()
            );
            Ok(())
        }
        Err(e) => Err(anyhow::Error::new(e).context("trusty-memory start")),
    }
}

/// The exit status of a restart: success only when the new pid serves.
///
/// Test: `restart_report_names_a_survivor_and_starts_nothing`.
pub(crate) fn restart_report(result: Result<lc::RestartOutcome, LifecycleError>) -> Result<()> {
    let restarted = result.context("trusty-memory restart")?;
    let was = match restarted.stop {
        StopOutcome::Stopped { pid, .. } => format!("pid {pid} stopped"),
        _ => "was not running".to_string(),
    };
    println!(
        "{} trusty-memory restarted ({was}; pid {} serving)",
        "✓".green(),
        restarted.start.pid
    );
    Ok(())
}

/// Take `start.lock` on a blocking-safe thread.
async fn take_start_lock() -> Result<trusty_mcp::StartLock> {
    let path = super::start::start_lock_path()
        .context("could not resolve the trusty-memory data directory")?;
    tokio::task::spawn_blocking(move || trusty_mcp::StartLock::acquire_blocking(&path))
        .await
        .context("start-lock acquisition task panicked")?
}

#[cfg(test)]
#[path = "linux_lifecycle_tests.rs"]
mod tests;
