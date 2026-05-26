//! Per-palace `trusty-bm25-daemon` spawn supervisor (issue #193).
//!
//! Why: trusty-memory ships the `trusty-bm25-daemon` binary alongside its
//! own (`cargo install trusty-memory` produces all three binaries) but never
//! actually spawns it. Operators who set `TRUSTY_BM25_DAEMON=1` then have to
//! manually `launchctl bootstrap` (or otherwise babysit) one daemon per
//! palace, which is the exact UX trap PR #190 closed for trusty-embedderd.
//! This module makes BM25 a single-process concern: on first BM25 use for a
//! palace, we discover the binary via `locate_bm25_daemon_binary()`, spawn
//! a child with the right `--palace` + `--data-dir`, poll the socket until
//! it appears, and own the `tokio::process::Child` for the rest of the
//! daemon's life. Operators who run their own daemon out-of-band (launchd,
//! systemd, manual) set `TRUSTY_BM25_EXTERNAL=1` to opt out of spawn.
//!
//! What: a single `Bm25Supervisor` value keyed by palace id, internally
//! using a `tokio::sync::Mutex<HashMap<String, ChildHandle>>` so concurrent
//! callers don't race a double-spawn. `ensure_running` returns the socket
//! path the caller should connect to; it skips spawn entirely when
//! `TRUSTY_BM25_EXTERNAL=1` is set or when the socket already accepts a
//! connection. Shutdown SIGTERMs every owned child, waits on each, and
//! best-effort cleans up its socket file.
//!
//! Test: unit tests in this module cover the external-mode opt-out, the
//! "already running" probe, and the `shutdown` reaping path with no
//! daemon ever spawned. An `#[ignore]`-tagged integration test in
//! `tests/bm25_supervisor_e2e.rs` drives a real `trusty-bm25-daemon` child
//! end-to-end (index + search + shutdown) when the binary is on PATH or
//! `TRUSTY_BM25_DAEMON_BIN` is set.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use trusty_common::bm25_client::{locate_bm25_daemon_binary, socket_path_for_palace};

/// Environment variable that disables spawn supervision entirely.
///
/// Why: operators who manage `trusty-bm25-daemon` themselves (launchd plist,
/// systemd unit, docker sidecar) must be able to opt the in-process
/// supervisor out so we don't end up with two daemons fighting over the
/// same socket. Setting this to `1` makes `ensure_running` skip the spawn
/// step and just return the socket path the caller would have connected to
/// anyway.
/// What: the env var name `TRUSTY_BM25_EXTERNAL`. Any value other than `"1"`
/// is treated as unset.
/// Test: `external_mode_skips_spawn` exercises the opt-out branch.
pub const ENV_EXTERNAL_BM25: &str = "TRUSTY_BM25_EXTERNAL";

/// Upper bound on how long `ensure_running` waits for the freshly-spawned
/// daemon's UDS socket to appear and accept a connection.
///
/// Why: the daemon's bind step is fast (the BM25 snapshot load is the
/// slowest part and runs on a tempdir-sized fixture in tests), so 3 s is
/// comfortably more than the observed worst case while still failing
/// fast on a misconfigured spawn. Mirrors the trusty-search supervisor's
/// `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS=30` default but scaled down
/// because BM25 has no model-loading step.
/// What: 3 seconds (3000 ms).
/// Test: covered indirectly by the integration test's spawn path.
const SPAWN_PROBE_TIMEOUT: Duration = Duration::from_millis(3000);

/// Initial polling interval used to probe the socket after spawn; doubled
/// on each miss up to a ceiling so we don't busy-wait but also don't sleep
/// the full 3 s budget when the daemon is ready in ~20 ms.
const INITIAL_PROBE_INTERVAL: Duration = Duration::from_millis(20);

/// Ceiling on the exponential backoff so we never sleep longer than 250 ms
/// between probes — at the 3 s budget this gives ~16 probes which is more
/// than enough to catch any reasonable startup latency.
const MAX_PROBE_INTERVAL: Duration = Duration::from_millis(250);

/// Per-palace child handle stored in the supervisor's map.
///
/// Why: keeping the `Child` plus the resolved socket path together lets
/// `shutdown` reap each daemon and clean up its socket file in one pass
/// without re-resolving the path. Mirrors the trusty-search embedder
/// supervisor's per-slot bookkeeping.
/// What: holds the `tokio::process::Child` and the socket path that the
/// daemon was instructed to bind. The `Child` is moved out on shutdown
/// so we can call `.wait()`.
/// Test: covered indirectly by `shutdown_with_no_children_is_noop` and
/// the end-to-end integration test.
struct ChildHandle {
    child: Child,
    socket_path: PathBuf,
}

/// Supervisor that owns BM25 daemon subprocesses, one per palace.
///
/// Why: trusty-memory wants the BM25 lane to be a zero-touch feature — set
/// `TRUSTY_BM25_DAEMON=1` and recall just gets a lexical boost without any
/// extra process management. Owning the children here means the trusty-memory
/// daemon's lifetime IS the BM25 daemons' lifetime, which is the same
/// guarantee `tokio::process::Child` gives us via `kill_on_drop` (set on
/// every spawn). Restart-on-crash is handled lazily: the next
/// `ensure_running` call observes the dead child via `try_wait()` and
/// re-spawns once.
/// What: a `Mutex<HashMap<String, ChildHandle>>` keyed by palace id. All
/// public methods are `&self` so the supervisor can live behind an `Arc`
/// and be shared across handlers without `Mutex<Supervisor>` plumbing at
/// the call site. The map mutex is fine-grained: it only protects the
/// HashMap; the actual spawn + socket probe happen with the lock released
/// so concurrent palaces don't queue behind each other.
/// Test: `external_mode_skips_spawn`, `already_running_skips_spawn`,
/// `shutdown_with_no_children_is_noop`, and the integration test.
pub struct Bm25Supervisor {
    children: Mutex<HashMap<String, ChildHandle>>,
}

impl Bm25Supervisor {
    /// Construct an empty supervisor.
    ///
    /// Why: cheap, allocation-light constructor so the supervisor can be
    /// built unconditionally at startup and only allocate when a palace
    /// first asks for a daemon.
    /// What: returns a supervisor with an empty per-palace map.
    /// Test: trivially exercised by every other test in this module.
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }

    /// Ensure a `trusty-bm25-daemon` is running for `palace` and return the
    /// socket path the caller should connect to.
    ///
    /// Why: callers want a single function that handles the four states a
    /// per-palace daemon can be in — externally managed, already-spawned,
    /// dead-and-needs-restart, never-spawned — without each call site
    /// reimplementing the probe + spawn logic. Returning the socket path
    /// (rather than the `Bm25Client`) keeps the supervisor free of any
    /// dependency on the client type and lets the caller decide whether
    /// to construct one new client per call or cache.
    /// What: when `TRUSTY_BM25_EXTERNAL=1`, returns the socket path without
    /// touching anything. Otherwise: (1) checks the in-memory map; if the
    /// stored child is alive, returns its socket path. If the child has
    /// exited unexpectedly, evicts it and falls through to a fresh spawn
    /// (one restart attempt per call — if THAT spawn also fails the error
    /// propagates and the caller degrades). (2) Probes the socket — if some
    /// out-of-band process already runs the daemon for this palace, we adopt
    /// the socket and skip the spawn so we don't EADDRINUSE. (3) Spawns
    /// `trusty-bm25-daemon --palace <name> --data-dir <data_dir>` via
    /// `tokio::process::Command`, polls the socket with exponential backoff
    /// until the bound listener accepts a connection (or the timeout
    /// elapses), then stores the child and returns the path.
    /// Test: `external_mode_skips_spawn`, `already_running_skips_spawn`,
    /// and the integration test cover all four states.
    pub async fn ensure_running(&self, palace: &str, data_dir: &Path) -> Result<PathBuf> {
        let socket_path = socket_path_for_palace(palace);

        // ── (1) External-mode opt-out ──────────────────────────────────────
        if external_mode_enabled() {
            tracing::debug!(
                palace = %palace,
                socket = %socket_path.display(),
                "{ENV_EXTERNAL_BM25}=1 — skipping spawn supervision"
            );
            return Ok(socket_path);
        }

        // ── (2) Already-supervised path ────────────────────────────────────
        // Take the lock briefly to inspect / evict the stored child. We
        // drop the guard before spawning so concurrent calls for OTHER
        // palaces don't queue behind a slow startup probe.
        {
            let mut guard = self.children.lock().await;
            if let Some(entry) = guard.get_mut(palace) {
                match entry.child.try_wait() {
                    Ok(None) => {
                        // Still alive — happy path.
                        tracing::trace!(
                            palace = %palace,
                            socket = %entry.socket_path.display(),
                            "bm25 supervisor: child already running"
                        );
                        return Ok(entry.socket_path.clone());
                    }
                    Ok(Some(status)) => {
                        // Exited — log and evict so we can re-spawn below.
                        tracing::warn!(
                            palace = %palace,
                            ?status,
                            "bm25 daemon exited unexpectedly — attempting one restart"
                        );
                        guard.remove(palace);
                    }
                    Err(e) => {
                        tracing::warn!(
                            palace = %palace,
                            "bm25 supervisor: try_wait failed: {e:#} — evicting and retrying"
                        );
                        guard.remove(palace);
                    }
                }
            }
        }

        // ── (3) Socket-already-bound check ─────────────────────────────────
        // If some other process (a previously-spawned daemon we lost the
        // handle to, or an operator-managed launchd job that forgot to set
        // TRUSTY_BM25_EXTERNAL) is already serving on this socket, adopt
        // it. Spawning a second daemon would EADDRINUSE the bind.
        if probe_socket(&socket_path).await {
            tracing::info!(
                palace = %palace,
                socket = %socket_path.display(),
                "bm25 daemon socket already responding — not spawning a new child"
            );
            return Ok(socket_path);
        }

        // ── (4) Spawn ──────────────────────────────────────────────────────
        let binary =
            locate_bm25_daemon_binary().context("locate trusty-bm25-daemon binary for spawn")?;
        let child = spawn_child(&binary, palace, data_dir)
            .await
            .with_context(|| {
                format!(
                    "spawn trusty-bm25-daemon {} for palace {palace}",
                    binary.display()
                )
            })?;

        // Probe the socket until it accepts a connection (or we time out).
        // If the probe fails we still hold the `Child` — drop it so
        // `kill_on_drop` SIGKILLs the doomed daemon before we propagate.
        if let Err(e) = wait_for_socket(&socket_path).await {
            // Explicit drop — clarifies intent for the next reader.
            drop(child);
            return Err(e.context(format!(
                "bm25 daemon for palace {palace} did not bind {} within {:?}",
                socket_path.display(),
                SPAWN_PROBE_TIMEOUT
            )));
        }

        tracing::info!(
            palace = %palace,
            socket = %socket_path.display(),
            binary = %binary.display(),
            "spawned trusty-bm25-daemon"
        );

        // Store the handle so the next `ensure_running` sees it.
        let mut guard = self.children.lock().await;
        guard.insert(
            palace.to_string(),
            ChildHandle {
                child,
                socket_path: socket_path.clone(),
            },
        );
        Ok(socket_path)
    }

    /// Graceful shutdown: SIGTERM all owned daemons, reap them, and clean
    /// up their sockets.
    ///
    /// Why: trusty-memory's normal exit path is a SIGTERM from launchd or a
    /// ctrl-c at the foreground. Without this we'd rely on `kill_on_drop`
    /// to send SIGKILL on each child, which (a) skips the daemon's own
    /// cleanup of the socket file and (b) leaves the BM25 snapshot
    /// half-flushed if the daemon was mid-batch. Sending SIGTERM lets the
    /// daemon run its own shutdown sequence (drain queue, flush snapshot,
    /// unlink socket) before we move on.
    /// What: drains the per-palace map; for each child sends SIGTERM, waits
    /// up to ~2 s for it to exit, then `kill()`s (SIGKILL) if it's still
    /// alive. Best-effort `remove_file` on each socket path because the
    /// daemon's own cleanup may have already done it. Idempotent — calling
    /// `shutdown` twice is harmless.
    /// Test: `shutdown_with_no_children_is_noop` covers the empty-map case;
    /// the integration test asserts the child is reaped and the socket
    /// file removed.
    pub async fn shutdown(&self) {
        let mut guard = self.children.lock().await;
        let handles: Vec<(String, ChildHandle)> = guard.drain().collect();
        drop(guard);

        for (palace, mut entry) in handles {
            tracing::info!(
                palace = %palace,
                pid = ?entry.child.id(),
                "shutting down bm25 daemon"
            );
            if let Err(e) = terminate_child(&mut entry.child).await {
                tracing::warn!(
                    palace = %palace,
                    "bm25 daemon shutdown encountered an error: {e:#}"
                );
            }
            // Best-effort socket cleanup. The daemon's own SIGTERM handler
            // unlinks the socket as part of clean exit, but if we had to
            // SIGKILL it the file is still on disk and will EADDRINUSE
            // the next spawn.
            if let Err(e) = tokio::fs::remove_file(&entry.socket_path).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::debug!(
                        palace = %palace,
                        socket = %entry.socket_path.display(),
                        "could not remove bm25 daemon socket (likely already cleaned up): {e}"
                    );
                }
            }
        }
    }

    /// Number of palaces currently being supervised — primarily for tests
    /// and observability.
    ///
    /// Why: lets `shutdown_with_no_children_is_noop` and similar checks
    /// avoid reaching into the private map.
    /// What: returns the size of the per-palace handle map.
    /// Test: `supervisor_starts_empty`.
    pub async fn supervised_count(&self) -> usize {
        self.children.lock().await.len()
    }
}

impl Default for Bm25Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Bm25Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // We deliberately don't lock the mutex here — `Debug` may be
        // invoked while another task already holds the guard, and we'd
        // rather print a placeholder than deadlock or panic.
        f.debug_struct("Bm25Supervisor")
            .field("children", &"<locked>")
            .finish()
    }
}

/// Returns true when `TRUSTY_BM25_EXTERNAL=1`.
///
/// Why: tiny helper so the env-var check is testable and not duplicated
/// at each call site.
/// What: reads `TRUSTY_BM25_EXTERNAL`; treats anything other than the
/// exact string `"1"` as unset, matching how `TRUSTY_BM25_DAEMON=1`
/// gates the client side.
/// Test: `external_mode_skips_spawn` flips the env var to verify the
/// branch.
fn external_mode_enabled() -> bool {
    std::env::var(ENV_EXTERNAL_BM25).as_deref() == Ok("1")
}

/// Quick non-blocking probe — opens a `UnixStream`, immediately closes it.
///
/// Why: we want a single yes/no answer to "is something listening on this
/// socket right now?" without depending on `Bm25Client` (which would couple
/// us to the BM25 wire protocol) and without spending more than a few ms.
/// What: attempts `UnixStream::connect` with a short timeout; returns
/// true on success. Any error (ENOENT, ECONNREFUSED, ETIMEDOUT) is
/// interpreted as "no daemon".
/// Test: covered indirectly — every spawn path goes through this probe
/// in a tight loop.
async fn probe_socket(path: &Path) -> bool {
    // Use a short timeout so an unresponsive (but still bound) socket
    // doesn't stall the probe loop.
    let connect = UnixStream::connect(path);
    matches!(
        tokio::time::timeout(Duration::from_millis(200), connect).await,
        Ok(Ok(_))
    )
}

/// Poll the socket with exponential backoff until it accepts a connection
/// or the spawn timeout elapses.
///
/// Why: a freshly-spawned daemon takes a few ms to load its snapshot and
/// bind the listener. We don't know exactly how long, so polling with a
/// short initial interval (and doubling on each miss) gives sub-50 ms
/// detection on the happy path without hammering the kernel.
/// What: loops `probe_socket` until success or until the cumulative wait
/// exceeds `SPAWN_PROBE_TIMEOUT`. Returns `Err` with the timeout duration
/// in the message so the caller can surface it.
/// Test: covered indirectly by the integration test.
async fn wait_for_socket(path: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + SPAWN_PROBE_TIMEOUT;
    let mut interval = INITIAL_PROBE_INTERVAL;
    loop {
        if probe_socket(path).await {
            return Ok(());
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            anyhow::bail!(
                "socket {} did not become connectable within {:?}",
                path.display(),
                SPAWN_PROBE_TIMEOUT
            );
        }
        // Don't sleep past the deadline.
        let remaining = deadline.saturating_duration_since(now);
        let sleep_for = interval.min(remaining);
        tokio::time::sleep(sleep_for).await;
        // Exponential backoff with a cap so we never sleep too long.
        interval = (interval * 2).min(MAX_PROBE_INTERVAL);
    }
}

/// Spawn a single `trusty-bm25-daemon` child for `palace`.
///
/// Why: keeps the `tokio::process::Command` builder isolated so tests can
/// focus on the supervisor's higher-level state machine.
/// What: builds the command with `--palace <name> --data-dir <dir>`,
/// inherits stderr so the daemon's tracing log appears in the parent's
/// log stream, ignores stdin/stdout (the daemon speaks UDS only), and
/// sets `kill_on_drop` so an unsupervised drop (e.g. panic propagation)
/// still SIGKILLs the child rather than leaking it.
/// Test: covered indirectly by the integration test.
async fn spawn_child(binary: &Path, palace: &str, data_dir: &Path) -> Result<Child> {
    // Ensure the data dir exists before launching — the daemon would
    // create it itself but failing here gives a cleaner error message
    // and avoids spawning a child that immediately exits.
    if !data_dir.exists() {
        tokio::fs::create_dir_all(data_dir)
            .await
            .with_context(|| format!("create bm25 data dir {}", data_dir.display()))?;
    }

    let child = Command::new(binary)
        .arg("--palace")
        .arg(palace)
        .arg("--data-dir")
        .arg(data_dir)
        // The daemon never reads stdin; closing it cleanly is the only
        // reasonable behaviour. Stdout is also unused — the daemon logs
        // to stderr.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;

    Ok(child)
}

/// Send SIGTERM, wait briefly, then SIGKILL if still alive.
///
/// Why: a clean SIGTERM lets the daemon flush its BM25 snapshot and
/// remove its socket; a SIGKILL is the fallback if the daemon hangs.
/// 2 s is comfortably larger than the daemon's flush window (the batch
/// queue's coalescing window is 100 ms by default).
/// What: on Unix sends SIGTERM via `nix::sys::signal::kill` would add a
/// dep, so we use `tokio::process::Child::start_kill` on the doomsday
/// path and `Child::kill` (which sends SIGKILL) only as a fallback.
/// Since tokio's API doesn't expose SIGTERM directly, we send SIGTERM
/// via libc when the platform exposes it, otherwise fall back to the
/// stdlib `kill` (SIGKILL). On test/non-unix builds we just kill.
/// Test: integration test verifies the process is gone after shutdown.
async fn terminate_child(child: &mut Child) -> Result<()> {
    // Attempt a graceful SIGTERM first. tokio::process::Child doesn't
    // expose a direct SIGTERM method, so on Unix we reach into the raw
    // PID and use libc::kill. Failures here just fall through to the
    // SIGKILL path below.
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: libc::kill is safe to call with any pid; the kernel
        // returns -1/EINVAL/ESRCH rather than UB on bad input. We
        // intentionally ignore the return value because either the
        // signal landed (child will exit) or it didn't (we'll SIGKILL).
        unsafe {
            let _ = libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }

    // Give the child up to 2 s to exit on its own.
    let wait_result = tokio::time::timeout(Duration::from_millis(2000), child.wait()).await;
    match wait_result {
        Ok(Ok(status)) => {
            tracing::debug!(?status, "bm25 daemon exited after SIGTERM");
            Ok(())
        }
        Ok(Err(e)) => Err(e).context("wait on bm25 daemon child after SIGTERM"),
        Err(_elapsed) => {
            // Still alive — force-kill. `kill()` here is tokio's
            // method which sends SIGKILL and waits, so by the time it
            // returns the process is definitely gone.
            tracing::warn!("bm25 daemon ignored SIGTERM after 2s — sending SIGKILL");
            child
                .kill()
                .await
                .context("SIGKILL bm25 daemon after SIGTERM timeout")?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex as TokioMutex;

    /// Process-wide lock that serialises every test in this module which
    /// touches the `TRUSTY_BM25_EXTERNAL` or `TRUSTY_BM25_DAEMON_BIN` env
    /// vars.
    ///
    /// Why: cargo runs tests in parallel inside the same process, and any
    /// two tests that mutate the same env var race each other. The lock
    /// keeps the supervisor's env mutation isolated from sibling tests
    /// without slowing the whole suite via `--test-threads=1`. We use
    /// `tokio::sync::Mutex` here (not `std::sync::Mutex`) because the
    /// guard is held across `.await` calls in `ensure_running`; holding a
    /// std-sync guard across an await would block the runtime and is
    /// flagged by `clippy::await_holding_lock`.
    /// What: a static `OnceLock<Arc<TokioMutex<()>>>` initialised on first
    /// access so we don't need a runtime to construct it. Each call
    /// returns a clone of the `Arc` — cheap and lockable.
    /// Test: used by every env-mutating test below.
    fn env_lock() -> std::sync::Arc<TokioMutex<()>> {
        static LOCK: std::sync::OnceLock<std::sync::Arc<TokioMutex<()>>> =
            std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Arc::new(TokioMutex::new(())))
            .clone()
    }

    /// Why: the supervisor must start with an empty map so the first
    /// `ensure_running` call always takes the cold-path branch.
    /// What: constructs a supervisor and asserts no children are tracked.
    /// Test: this test itself.
    #[tokio::test]
    async fn supervisor_starts_empty() {
        let sup = Bm25Supervisor::new();
        assert_eq!(sup.supervised_count().await, 0);
    }

    /// Why: `Default::default()` must behave like `new()`. Catches a
    /// regression where someone adds state to `new` and forgets to mirror
    /// it on `Default`.
    /// Test: this test itself.
    #[tokio::test]
    async fn supervisor_default_matches_new() {
        let sup: Bm25Supervisor = Default::default();
        assert_eq!(sup.supervised_count().await, 0);
    }

    /// Why: in external-management mode `ensure_running` must NOT spawn
    /// anything; it must just hand back the socket path the caller would
    /// have ended up at if it had spawned. Pinning this guards against a
    /// future regression that accidentally fires off a child even with the
    /// env var set.
    /// What: set `TRUSTY_BM25_EXTERNAL=1`, call `ensure_running` against a
    /// definitely-unused palace + a nonexistent data dir, assert no child
    /// is tracked and the returned path matches the canonical resolver.
    /// Test: this test itself. The env mutation is serialised by the
    /// guard because Rust tests run in parallel inside the same process.
    #[tokio::test]
    async fn external_mode_skips_spawn() {
        let lock = env_lock();
        let _env = lock.lock().await;
        let _guard = EnvGuard::set(ENV_EXTERNAL_BM25, "1");
        let tmp = tempfile::tempdir().expect("tempdir");
        let sup = Bm25Supervisor::new();
        // Short palace name so the resolved socket path stays well under
        // the kernel's `sun_path` limit (~104 bytes on macOS).
        let palace = "ext-skip";
        let path = sup
            .ensure_running(palace, tmp.path())
            .await
            .expect("external mode must return socket path without spawning");
        assert_eq!(path, socket_path_for_palace(palace));
        assert_eq!(
            sup.supervised_count().await,
            0,
            "external mode must not register a child"
        );
    }

    /// Why: if some other process is already serving on the canonical
    /// socket path (think: stale daemon from a previous run, or an
    /// operator-managed launchd job that forgot the `TRUSTY_BM25_EXTERNAL`
    /// env var), spawning a second daemon would EADDRINUSE. The supervisor
    /// must adopt the existing socket and return without spawning.
    /// What: bind a dummy `UnixListener` at the canonical socket path for
    /// a test palace, call `ensure_running`, assert no child is tracked
    /// and the returned path matches.
    /// Test: this test itself.
    #[tokio::test]
    async fn already_running_skips_spawn() {
        let lock = env_lock();
        let _env = lock.lock().await;
        // Ensure we don't accidentally pick up an external-mode flag from
        // a sibling test that ran first.
        let _g = EnvGuard::remove(ENV_EXTERNAL_BM25);
        // Use a very short palace name. The canonical socket path is
        // `$TMPDIR/trusty-bm25-<palace>.sock`, and macOS' `$TMPDIR`
        // (`/var/folders/.../T/`) is already long, so we keep the palace
        // fragment to a handful of characters to avoid SUN_LEN errors.
        // Use the low bits of process PID to disambiguate concurrent
        // test runs.
        let palace = format!("a{:x}", std::process::id() & 0xffff);
        let socket = socket_path_for_palace(&palace);
        // Clean up any leftover socket from a previous failed test.
        let _ = std::fs::remove_file(&socket);
        let listener =
            tokio::net::UnixListener::bind(&socket).expect("bind dummy listener at canonical path");

        let tmp = tempfile::tempdir().expect("tempdir");
        let sup = Bm25Supervisor::new();
        let path = sup
            .ensure_running(&palace, tmp.path())
            .await
            .expect("ensure_running must adopt existing socket");
        assert_eq!(path, socket);
        assert_eq!(
            sup.supervised_count().await,
            0,
            "adoption path must not register a child"
        );

        drop(listener);
        let _ = std::fs::remove_file(&socket);
    }

    /// Why: `shutdown` on a fresh supervisor must not panic, error, or
    /// log anything alarming. Operators will inevitably call it at exit
    /// even when no palace has touched BM25 yet.
    /// Test: this test itself.
    #[tokio::test]
    async fn shutdown_with_no_children_is_noop() {
        let sup = Bm25Supervisor::new();
        sup.shutdown().await;
        assert_eq!(sup.supervised_count().await, 0);
    }

    /// Why: `Bm25Supervisor` is shared via `Arc` and must be `Send + Sync`
    /// so it can be cloned into background tasks and async handlers.
    /// Compile-fail of this test means the type bounds regressed.
    /// What: a static assertion via a const fn that requires `Send + Sync`.
    /// Test: this test itself.
    #[test]
    fn supervisor_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Bm25Supervisor>();
    }

    /// Why: the probe must report `false` for a path with nothing bound,
    /// not panic or block forever.
    /// Test: this test itself.
    #[tokio::test]
    async fn probe_returns_false_for_missing_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("nonexistent.sock");
        assert!(!probe_socket(&missing).await);
    }

    /// Why: the probe must report `true` immediately when something is
    /// already accepting connections at that path. Pins the happy path.
    /// Test: this test itself.
    #[tokio::test]
    async fn probe_returns_true_for_bound_socket() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("listen.sock");
        let _listener =
            tokio::net::UnixListener::bind(&sock).expect("bind listener for probe test");
        assert!(probe_socket(&sock).await);
    }

    /// RAII guard for serialised env-var mutation in tests.
    ///
    /// Why: cargo test runs tests in the same process by default, so
    /// `std::env::set_var` mutations leak between tests unless restored
    /// on drop. This is the same pattern the supervisor unit tests in
    /// `trusty-search/src/service/embedder_supervisor.rs` use.
    /// What: captures the prior value on construction, restores or
    /// removes on drop. SAFETY notes inlined at each unsafe block.
    /// Test: used by every env-touching test in this module.
    struct EnvGuard {
        key: String,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: test-only env mutation; serialised by the fact that
            // each test takes the guard before mutating, and the Drop
            // impl restores on scope exit.
            unsafe { std::env::set_var(key, value) }
            Self {
                key: key.to_string(),
                prev,
            }
        }

        fn remove(key: &str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: same invariant as `set`.
            unsafe { std::env::remove_var(key) }
            Self {
                key: key.to_string(),
                prev,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: test teardown; restoring the captured prior value
            // is the inverse of the unsafe mutation done at construction.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(&self.key, v),
                    None => std::env::remove_var(&self.key),
                }
            }
        }
    }
}
