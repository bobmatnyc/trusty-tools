//! Background HTTP daemon: PID lockfile + auto-port + graceful shutdown.
//!
//! Why: `trusty-search daemon` is the long-lived process that owns every
//! index for a machine. Two invariants matter:
//!
//! 1. **Singleton.** Only one daemon may run per machine. We enforce this
//!    via an OS-level advisory exclusive lock on a lockfile in the user's
//!    data-local dir. If the lock is held, `run_daemon` returns
//!    [`DaemonError::AlreadyRunning`] and `main` exits 1.
//!
//! 2. **Discoverable port.** The MCP server (and `trusty-search status`)
//!    needs to know what port the daemon picked. We bind a `TcpListener`
//!    starting at the requested port and walking forward until something
//!    is free, then write the chosen port to a file siblings to the lock.
//!
//! Graceful shutdown: axum's `with_graceful_shutdown` is wired to a tokio
//! signal future that resolves on SIGTERM or SIGINT. On exit we delete the
//! port file (the lockfile is unlinked by drop semantics on Unix; on
//! Windows the `Drop` of `File` releases the lock).
//!
//! What:
//! - [`daemon_lock_path`] / [`daemon_port_path`] resolve XDG-style paths.
//! - [`run_daemon`] is the one-shot entry point used by `main`.
//! - [`DaemonHandle`] returned for tests/embedding.
//!
//! Test: `cargo test -p trusty-search-service` covers (a) port-file
//! round-trip, (b) lockfile contention (second `try_lock_exclusive` on the
//! same path errors), (c) auto-port selection when the requested port is
//! taken.

use crate::service::server::{build_router, SearchAppState};
use fs4::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
};
use thiserror::Error;
use tokio::net::TcpListener;

/// Errors raised by [`run_daemon`].
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("another trusty-search daemon is already running (lock held at {0})")]
    AlreadyRunning(PathBuf),
    #[error("could not determine data-local directory")]
    NoDataDir,
    #[error("could not find a free port starting at {0}")]
    NoFreePort(u16),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("server error: {0}")]
    Server(String),
}

/// Path to the advisory PID lockfile (`~/.local/share/trusty-search/daemon.lock`
/// on Linux, the platform equivalent elsewhere).
pub fn daemon_lock_path() -> Result<PathBuf, DaemonError> {
    Ok(daemon_dir()?.join("daemon.lock"))
}

/// Path to the file that records the listening port.
pub fn daemon_port_path() -> Result<PathBuf, DaemonError> {
    Ok(daemon_dir()?.join("daemon.port"))
}

/// Path to `daemon.env` — persisted memory-limit env vars written by
/// `trusty-search start` so launchd restarts inherit them.
///
/// Why: launchd re-spawns the daemon without the operator's shell environment,
/// causing `TRUSTY_MEMORY_LIMIT_MB` and friends to be lost after a restart.
/// Writing them to a file at `start`-time lets the daemon re-apply them on
/// every boot, regardless of how it was launched. When `TRUSTY_DATA_DIR` is
/// set the file lands in that directory so isolated daemons keep their own
/// env snapshot distinct from the production daemon.
/// What: returns `<daemon_dir>/daemon.env` (respecting `TRUSTY_DATA_DIR`).
/// Test: path ends in `daemon.env`; the parent directory is the same as the
/// lockfile directory so both are writable under the same permission set.
pub fn daemon_env_path() -> Option<PathBuf> {
    daemon_dir().ok().map(|d| d.join("daemon.env"))
}

/// The env-var keys that `trusty-search start` persists and the daemon sources
/// on startup. Ordered from most critical to least so log output is predictable.
pub const PERSISTED_ENV_VARS: &[&str] = &[
    "TRUSTY_MEMORY_LIMIT_MB",
    "TRUSTY_MAX_CHUNKS",
    "TRUSTY_EMBEDDING_CACHE",
    "TRUSTY_MAX_BATCH_SIZE",
    "TRUSTY_BM25_CORPUS_CAP",
    // Persist the device selection so launchd/systemd restarts (which run
    // without the user's shell env) keep honouring `--device cpu`. This is
    // load-bearing on Apple Silicon: CoreML inflates virtual RSS to ~100 GB
    // and triggers macOS jetsam kill on large repos, so operators who pin
    // CPU must have that pin survive every restart.
    "TRUSTY_DEVICE",
];

/// Write memory-limit env vars from the current process environment to
/// `daemon.env` so launchd restarts inherit them.
///
/// Why: called by `trusty-search start` to snapshot whatever the operator set
/// in their shell; the file is sourced by `load_daemon_env` at daemon startup.
/// What: iterates `PERSISTED_ENV_VARS`; writes only vars that are currently
/// set so the file stays minimal and the daemon's compiled-in defaults win for
/// anything absent. Uses `key=value\n` lines (POSIX dotenv subset).
/// Test: call `save_daemon_env()` after setting `TRUSTY_MEMORY_LIMIT_MB=1024`
/// in the process env; then read the file and assert it contains that line.
pub fn save_daemon_env() {
    let Some(path) = daemon_env_path() else {
        tracing::warn!("could not resolve daemon.env path — memory limits will not persist");
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut lines = Vec::new();
    for key in PERSISTED_ENV_VARS {
        if let Ok(val) = std::env::var(key) {
            lines.push(format!("{key}={val}\n"));
        }
    }
    // Only write the file when at least one memory-limit var is present.
    // This prevents a launchd restart (which inherits no shell vars) from
    // overwriting a previously-saved daemon.env with an empty file, which
    // would lose the operator's configured limits on the next restart.
    if lines.is_empty() {
        tracing::debug!("no memory-limit env vars set — daemon.env unchanged");
        return;
    }
    let content = lines.concat();
    match std::fs::write(&path, &content) {
        Ok(()) => tracing::debug!("wrote memory limits to {}", path.display()),
        Err(e) => tracing::warn!("could not write daemon.env: {e}"),
    }
}

/// Source `daemon.env` into the current process environment, skipping vars
/// that are already set (env > file > compiled-in default precedence).
///
/// Why: launchd restarts the daemon without the operator's shell env; this
/// function restores memory-limit knobs from the file written by `save_daemon_env`.
/// What: reads `daemon.env` (silently ignores missing file), parses `key=value`
/// lines, calls `std::env::set_var` only when the var is not already present.
/// Test: write `daemon.env` with `TRUSTY_MEMORY_LIMIT_MB=512`; unset the var;
/// call `load_daemon_env()`; assert `std::env::var("TRUSTY_MEMORY_LIMIT_MB") == "512"`.
pub fn load_daemon_env() {
    let Some(path) = daemon_env_path() else {
        return;
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return, // file absent is expected on first run
    };
    let mut loaded = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            // env var takes priority: only apply file value when var is unset
            if std::env::var(key).is_err() {
                // SAFETY: only called at startup before any threads read these vars;
                // `set_var` is not async-signal-safe but we are on the main thread here.
                unsafe { std::env::set_var(key, val) };
                loaded.push(key.to_owned());
            }
        }
    }
    if !loaded.is_empty() {
        tracing::info!(
            "sourced memory limits from daemon.env: {}",
            loaded.join(", ")
        );
    }
}

/// Path to `~/.trusty-search/http_addr` — the canonical address-discovery
/// file used by `trusty-search dashboard` and other client tools to locate
/// the running daemon. Distinct from the legacy `daemon.port` file (which
/// stores only the port number under the platform-specific data-local dir).
///
/// Why: aligns trusty-search with the trusty-memory address-discovery
/// contract — both daemons write a fully-qualified `host:port` line to
/// `~/.trusty-*/http_addr`. Clients can read either file to discover the
/// daemon without DNS or service registration.
/// What: returns `$HOME/.trusty-search/http_addr` (creating the parent
/// directory on demand is the caller's responsibility).
/// Test: with HOME=/tmp/xyz → returns "/tmp/xyz/.trusty-search/http_addr".
pub fn http_addr_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".trusty-search").join("http_addr"))
}

/// Resolve the root data directory for this daemon instance.
///
/// Why: `dirs::data_local_dir()` calls macOS NSFileManager and ignores HOME
/// overrides, which means only one daemon can run per Mac (issue #281). When
/// `TRUSTY_DATA_DIR` is set (by `--data-dir` or directly in the environment),
/// we use that path instead so isolated daemons (e.g. cert/benchmark runs) can
/// coexist with the production daemon.
///
/// What: returns `$TRUSTY_DATA_DIR` when set, otherwise
/// `<data_local_dir>/trusty-search`. Creates the directory if absent.
///
/// Test: set `TRUSTY_DATA_DIR=/tmp/ts-test` before calling; assert the returned
/// path equals `/tmp/ts-test` and the directory exists.
fn daemon_dir() -> Result<PathBuf, DaemonError> {
    // TRUSTY_DATA_DIR wins over the platform default so callers can run
    // isolated daemons for cert/benchmark work without conflicting with the
    // production lockfile (issue #281).
    if let Ok(override_dir) = std::env::var("TRUSTY_DATA_DIR") {
        let dir = PathBuf::from(override_dir);
        std::fs::create_dir_all(&dir)?;
        return Ok(dir);
    }
    // NB: We use `data_local_dir()` (not the shared `trusty_common::resolve_data_dir`
    // which uses `data_dir()`) because the lockfile path is replicated in `main.rs`
    // (`Stop`, `daemon_port_path`) against `data_local_dir()`. They must agree;
    // diverging would break daemon discovery on Windows where the two paths differ
    // (Roaming vs Local). If/when `trusty-common` grows a `resolve_data_local_dir`
    // helper, switch both sides at once.
    let dir = dirs::data_local_dir()
        .ok_or(DaemonError::NoDataDir)?
        .join("trusty-search");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Handle returned by [`run_daemon`] (mostly for tests).
pub struct DaemonHandle {
    pub port: u16,
    pub addr: SocketAddr,
}

/// Try to bind a `TcpListener` starting at `start_port`, walking forward up
/// to `max_attempts` ports. `0` means "let the OS pick" — handled directly.
///
/// Why: thin wrapper around `trusty_common::bind_with_auto_port` so the
/// daemon and the rest of the trusty-* family share the same port-walk
/// behaviour. We keep the wrapper to (a) preserve the `NoFreePort` typed
/// error this crate exposes and (b) translate the shared async helper into
/// a `DaemonError` boundary.
async fn bind_with_auto_port(
    start_port: u16,
    max_attempts: u16,
) -> Result<TcpListener, DaemonError> {
    let addr: SocketAddr =
        format!("127.0.0.1:{start_port}")
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                DaemonError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
            })?;
    trusty_common::bind_with_auto_port(addr, max_attempts)
        .await
        .map_err(|e| {
            tracing::warn!("auto-port exhausted from {start_port}: {e:#}");
            DaemonError::NoFreePort(start_port)
        })
}

/// Check whether a daemon is already running without starting one.
///
/// Why: callers that need to fail-fast (e.g. before loading a 86 MB embedding
/// model) can call this before doing any expensive work. Returns the lock-file
/// path when a running daemon is detected, `None` when the lock is free.
///
/// What: opens the lockfile (if it exists) and attempts a non-blocking
/// exclusive lock. If the attempt fails the lock is held by another process.
pub fn is_already_running() -> Option<PathBuf> {
    let lock_path = daemon_lock_path().ok()?;
    // If the lockfile doesn't exist there is definitely no daemon.
    if !lock_path.exists() {
        return None;
    }
    let file = OpenOptions::new()
        .create(false)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    if file.try_lock_exclusive().is_err() {
        // Lock is held — another daemon is alive.
        Some(lock_path)
    } else {
        // We acquired it; release immediately (lock drops here).
        None
    }
}

/// Inspect the lockfile and return the PID of a *running* daemon, if any.
///
/// Why: launchd treats any non-zero exit from `trusty-search start` as a
/// crash and re-spawns it after `ThrottleInterval` — producing an infinite
/// crash-loop when the daemon is already up and the second invocation
/// exits 1 with "already running". Callers (notably the `start` command)
/// need to distinguish "another live daemon is running, exit cleanly"
/// from "stale lockfile, recover and start".
///
/// What: returns `Some(pid)` if a lockfile exists, contains a parseable
/// PID, and that PID is currently alive. Returns `None` if the lockfile
/// is absent, unparseable, or records a dead PID (stale).
///
/// Test: with no lockfile, returns None. With a lockfile containing
/// `std::process::id()`, returns Some(current_pid). With a lockfile
/// containing a known-dead PID (e.g. u32::MAX), returns None.
pub fn running_daemon_pid() -> Option<u32> {
    let lock_path = daemon_lock_path().ok()?;
    if !lock_path.exists() {
        return None;
    }
    let pid = read_lockfile_pid(&lock_path)?;
    if pid_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

/// Read the PID stored in the lockfile (if any). Returns `None` on parse failure.
///
/// Why: the lockfile records the daemon PID so callers can detect stale
/// lockfiles left over from SIGKILL'd or crashed daemons (where the OS may
/// not have released the advisory lock cleanly, or the file persisted with
/// a dead PID written inside).
fn read_lockfile_pid(lock_path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(lock_path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse::<u32>().ok()
}

/// Check whether a process with the given PID is currently alive.
///
/// Why: a stale lockfile (from a SIGKILL'd or crashed daemon) records a PID
/// that no longer exists. Treat such lockfiles as removable so the next
/// daemon can start on the preferred port instead of bumping.
///
/// What: on Unix, `kill(pid, 0)` returns 0 if the process exists, ESRCH if
/// not, EPERM if it exists but is owned by another user (still alive). On
/// non-Unix targets we conservatively assume the PID is alive.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Use nix's safe wrapper over kill(pid, 0); signal None performs no
    // action, only error checking. We accept i32 narrowing — PIDs always
    // fit on platforms we support.
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        // EPERM means the process exists but we cannot signal it.
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Acquire an exclusive advisory lock on the daemon lockfile. The returned
/// `File` must outlive the daemon — drop releases the lock.
///
/// Why stale-lock handling: when a daemon is SIGKILL'd mid-run, the file
/// may persist with the dead PID recorded inside. On some platforms or
/// filesystems the advisory lock can also outlive the process. Before
/// reporting `AlreadyRunning`, we check whether the PID stored in the file
/// is still alive — if not, we remove the stale file and retry once.
fn acquire_lock(lock_path: &PathBuf) -> Result<File, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    if file.try_lock_exclusive().is_ok() {
        return Ok(file);
    }

    // Lock is held — but is it stale? Inspect the PID written by the previous
    // daemon. If the recorded PID is dead, treat the lockfile as abandoned
    // and recreate it.
    if let Some(prev_pid) = read_lockfile_pid(lock_path) {
        if !pid_alive(prev_pid) {
            tracing::warn!(
                "stale lockfile at {} (pid {prev_pid} is dead) — removing and retrying",
                lock_path.display()
            );
            drop(file);
            let _ = std::fs::remove_file(lock_path);
            let retry = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(lock_path)?;
            if retry.try_lock_exclusive().is_ok() {
                return Ok(retry);
            }
        }
    }

    Err(DaemonError::AlreadyRunning(lock_path.clone()))
}

/// Future that resolves on SIGTERM or SIGINT.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("install SIGTERM handler failed: {e}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Start the daemon: acquire the lock, bind a port, write the port file,
/// serve the axum router until SIGTERM/SIGINT, then clean up the port file.
pub async fn run_daemon(state: SearchAppState, requested_port: u16) -> Result<(), DaemonError> {
    let lock_path = daemon_lock_path()?;
    let port_path = daemon_port_path()?;

    // Lock first — second daemon must error before binding a port.
    let mut lock_file = acquire_lock(&lock_path)?;
    let pid_string = std::process::id().to_string();
    // Best-effort: write PID into the lockfile so `ps`/`lsof` can confirm.
    let _ = lock_file.set_len(0);
    let _ = lock_file.write_all(pid_string.as_bytes());

    let listener = bind_with_auto_port(requested_port, 64).await?;
    let addr = listener.local_addr()?;
    let port = addr.port();

    // Atomically write the port file (write + rename).
    write_port_file(&port_path, port)?;

    // Also write the canonical `~/.trusty-search/http_addr` discovery file
    // (full `host:port` line) so `trusty-search dashboard` and other clients
    // can locate the daemon without depending on the platform-specific
    // data-local dir. Best-effort: a missing $HOME is not fatal — the legacy
    // `daemon.port` file above is still authoritative for the local CLI.
    //
    // Note (issue #117): `write_http_addr_file` is unconditional and uses
    // tmp+rename, so a freshly-booted daemon always corrects a stale file
    // left behind by a crashed previous daemon or a SIGKILL'd `serve --http`.
    let http_addr_written = match http_addr_path() {
        Some(path) => match write_http_addr_file(&path, &addr) {
            Ok(()) => Some(path),
            Err(e) => {
                tracing::warn!("could not write {}: {e}", path.display());
                None
            }
        },
        None => None,
    };

    // Friendly startup banner (printed to stderr so it doesn't pollute stdout
    // for callers consuming JSON-RPC over a pipe). Includes the version so
    // users can confirm which binary is running when multiple are installed.
    eprintln!(
        "trusty-search v{} — HTTP admin panel: http://{}",
        env!("CARGO_PKG_VERSION"),
        addr,
    );

    // Why: The embedded UI needs to know the actual port at runtime so it
    // can call back to the daemon (window.__DAEMON_PORT__). Stamp it onto
    // the state right before building the router.
    let state = state.with_daemon_port(port);
    // Issue #85: capture a clone *before* moving `state` into `build_router`
    // so the post-shutdown flush can walk the registry. SearchAppState is
    // cheap to clone (all internal fields are Arc/handle-like).
    let flush_state = state.clone();
    let router = build_router(state);

    tracing::info!("daemon listening on {addr} (lock {})", lock_path.display());

    // Log active memory limits so operators can confirm the correct values
    // are in effect (especially important for launchd-managed restarts where
    // env vars come from daemon.env rather than the user's shell).
    {
        use crate::core::memguard::{index_memory_limit_mb, memory_limit_mb};
        let max_chunks = std::env::var("TRUSTY_MAX_CHUNKS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(200_000);
        let emb_cache = std::env::var("TRUSTY_EMBEDDING_CACHE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1_000);
        let fmt = |v: Option<u64>| match v {
            Some(mb) => format!("{mb}"),
            None => "unlimited".to_string(),
        };
        tracing::info!(
            "memory limits: max_chunks={max_chunks} embedding_cache={emb_cache} \
             memory_limit_mb={} index_memory_limit_mb={}",
            fmt(memory_limit_mb()),
            fmt(index_memory_limit_mb()),
        );
    }

    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    // Issue #85 — flush HNSW + chunk corpus for every registered index so
    // the next daemon boot warm-starts instead of paying a full re-index.
    // Best-effort: log on failure, don't abort cleanup.
    flush_all_indexes_on_shutdown(&flush_state).await;

    // Best-effort cleanup; ignore errors so the lockfile drop is what frees
    // the next daemon, not our cleanup.
    let _ = std::fs::remove_file(&port_path);
    if let Some(path) = http_addr_written {
        let _ = std::fs::remove_file(&path);
    }

    serve_result.map_err(|e| DaemonError::Server(e.to_string()))?;
    drop(lock_file);
    Ok(())
}

/// Walk every registered index and persist its HNSW snapshot + chunk corpus
/// to disk so the next daemon boot warm-starts (issue #85).
///
/// Why: called from `run_daemon` after the axum graceful-shutdown future
/// resolves. By this point no new requests can come in, but any in-flight
/// search handlers may still be holding read locks — we use the same
/// `save_to` / `save_chunks_to_disk` paths the incremental persister uses,
/// so they snapshot under read locks and never block writers indefinitely.
/// What: iterates `state.registry.list()`, persisting each index sequentially
/// (the daemon is exiting; we have no concurrency budget to protect).
/// Test: covered by the integration test that boots the daemon, indexes a
/// file, sends SIGTERM, then restarts and asserts the corpus survived.
pub async fn flush_all_indexes_on_shutdown(state: &SearchAppState) {
    let ids = state.registry.list();
    if ids.is_empty() {
        return;
    }
    tracing::info!(
        "shutdown: flushing {} index snapshot(s) before exit",
        ids.len()
    );
    for id in ids {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };
        // Issue #403: route HNSW + chunks paths to colocated or legacy storage
        // based on whether the index has a `.trusty-search/` dir in its root.
        let is_colocated =
            crate::service::colocated_storage::has_colocated_storage(&handle.root_path);
        let chunks_path = if is_colocated {
            // Colocated indexes write their corpus to redb only (no JSON fallback).
            // Provide a dummy path — `flush_corpus_to_disk` won't use it when a
            // `CorpusStore` is wired; the redb file lives in `.trusty-search/`.
            handle.root_path.join(".trusty-search").join("chunks.json")
        } else {
            match crate::service::persistence::chunks_path(&id.0) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("shutdown: chunks path unresolvable for '{}': {e}", id.0);
                    continue;
                }
            }
        };
        let hnsw_path = if is_colocated {
            match crate::service::colocated_storage::colocated_hnsw_path(&handle.root_path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        "shutdown: colocated hnsw path unresolvable for '{}': {e}",
                        id.0
                    );
                    continue;
                }
            }
        } else {
            match crate::service::persistence::hnsw_path(&id.0) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("shutdown: hnsw path unresolvable for '{}': {e}", id.0);
                    continue;
                }
            }
        };
        let indexer = handle.indexer.read().await;
        // Issue #28: `flush_corpus_to_disk` writes to redb when a `CorpusStore`
        // is wired (final consistency sweep, no full JSON rewrite) and falls
        // back to the legacy `chunks.json` snapshot otherwise.
        if let Err(e) = indexer.flush_corpus_to_disk(&chunks_path).await {
            tracing::warn!("shutdown: failed to flush chunk corpus for '{}': {e}", id.0);
        }
        match indexer.save_vector_store(&hnsw_path).await {
            Ok(true) => tracing::debug!("shutdown: saved HNSW for '{}'", id.0),
            Ok(false) => {} // no store wired (BM25-only mode)
            Err(e) => tracing::warn!("shutdown: failed to save HNSW for '{}': {e}", id.0),
        }
    }
}

/// Write the canonical `host:port` discovery line to `~/.trusty-search/http_addr`.
///
/// Why: separate from `write_port_file` because the format and location differ
/// — port file stores `12345`, http_addr stores `127.0.0.1:12345`. Both write
/// atomically via tmp-file + rename so partial reads are impossible.
/// What: creates parent directory if missing; writes via temp + rename.
/// Test: with a fresh tempdir, write addr → read back → matches `host:port`.
fn write_http_addr_file(path: &Path, addr: &SocketAddr) -> Result<(), DaemonError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("addr.tmp");
    {
        let mut f = File::create(&tmp)?;
        writeln!(f, "{addr}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_port_file(path: &PathBuf, port: u16) -> Result<(), DaemonError> {
    let tmp = path.with_extension("port.tmp");
    {
        let mut f = File::create(&tmp)?;
        writeln!(f, "{port}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener as StdTcpListener;

    #[test]
    fn http_addr_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("http_addr");
        let addr: SocketAddr = "127.0.0.1:54321".parse().unwrap();
        write_http_addr_file(&path, &addr).unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read.trim(), "127.0.0.1:54321");
    }

    #[test]
    fn port_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.port");
        write_port_file(&path, 12345).unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read.trim(), "12345");
    }

    #[test]
    fn pid_alive_current_process_is_alive() {
        // Why: smoke-test the PID-aliveness predicate so the launchd
        // crash-loop fix has explicit coverage. Our own PID must register
        // as alive; a clearly-invalid PID must not.
        assert!(pid_alive(std::process::id()));
        // Find a clearly-dead PID. macOS `pid_max` defaults to 99999 and
        // Linux to 4194304; on both, a value just under i32::MAX is well
        // beyond the legal range and `kill(pid, 0)` returns ESRCH.
        // (u32::MAX would narrow to -1 on i32 cast, which `kill` interprets
        // as "every process the caller can signal" — never ESRCH.)
        assert!(!pid_alive(2_000_000_000));
    }

    #[test]
    fn read_lockfile_pid_parses_pid() {
        // Why: `running_daemon_pid` depends on this parser. A malformed
        // file must return None rather than panic.
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.lock");
        std::fs::write(&good, "12345\n").unwrap();
        assert_eq!(read_lockfile_pid(&good), Some(12345));

        let bad = dir.path().join("bad.lock");
        std::fs::write(&bad, "not-a-pid").unwrap();
        assert_eq!(read_lockfile_pid(&bad), None);
    }

    #[test]
    fn lockfile_contention_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.lock");
        let _first = acquire_lock(&path).unwrap();
        let err = acquire_lock(&path).unwrap_err();
        assert!(matches!(err, DaemonError::AlreadyRunning(_)));
    }

    #[tokio::test]
    async fn auto_port_walks_forward() {
        // Bind a port, then ask the auto-port allocator to start there.
        let occupied = StdTcpListener::bind("127.0.0.1:0").unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();
        let next = bind_with_auto_port(occupied_port, 64).await.unwrap();
        assert_ne!(next.local_addr().unwrap().port(), occupied_port);
    }

    #[tokio::test]
    async fn auto_port_zero_uses_os() {
        // Note: port 0 is special — the shared helper delegates to the OS.
        let l = bind_with_auto_port(0, 1).await.unwrap();
        assert!(l.local_addr().unwrap().port() > 0);
    }

    /// Why: `daemon_dir()` must respect `TRUSTY_DATA_DIR` so an isolated daemon
    /// can run alongside the production daemon without lockfile conflicts (#281).
    /// What: set env var to a tempdir path; assert `daemon_dir()` returns it.
    /// Test: `daemon_dir_respects_trusty_data_dir_env_var` (this test).
    #[test]
    fn daemon_dir_respects_trusty_data_dir_env_var() {
        let tmp = tempfile::tempdir().unwrap();
        let override_path = tmp.path().to_path_buf();
        // SAFETY: test-only, single-threaded portion; no other thread reads
        // TRUSTY_DATA_DIR in this test binary at the same time.
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", &override_path);
        }
        let result = daemon_dir();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }
        let dir = result.expect("daemon_dir with TRUSTY_DATA_DIR should succeed");
        assert_eq!(dir, override_path, "daemon_dir should return the override");
        assert!(dir.exists(), "daemon_dir should create the directory");
    }

    /// Why: `daemon_lock_path()` and `daemon_port_path()` (service side) must
    /// land under the override directory, not the platform default.
    /// What: set env var, call both path functions, confirm they start with the
    /// override root rather than the default data-local dir.
    /// Test: `daemon_paths_under_data_dir_override` (this test).
    #[test]
    fn daemon_paths_under_data_dir_override() {
        let tmp = tempfile::tempdir().unwrap();
        let override_path = tmp.path().to_path_buf();
        unsafe {
            std::env::set_var("TRUSTY_DATA_DIR", &override_path);
        }
        let lock = daemon_lock_path();
        let port = daemon_port_path();
        unsafe {
            std::env::remove_var("TRUSTY_DATA_DIR");
        }
        let lock = lock.expect("lock path must resolve");
        let port = port.expect("port path must resolve");
        assert!(
            lock.starts_with(&override_path),
            "lock path {lock:?} should be under override {override_path:?}"
        );
        assert!(
            port.starts_with(&override_path),
            "port path {port:?} should be under override {override_path:?}"
        );
    }
}
