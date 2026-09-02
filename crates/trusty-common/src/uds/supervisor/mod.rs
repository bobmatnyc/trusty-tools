//! On-demand supervision of a UDS-serving child process (#5089 step 2).
//!
//! Why: ADR-0034 §1 puts `trusty-console` in charge of starting
//! `trusty-review` / `trusty-analyze` at the moment a webhook arrives, so
//! neither has to be resident — which is milestone `tm 1.3.5` criterion (c).
//! ADR-0032 had said that spawn-on-delivery mechanism "does not yet exist";
//! it did, as `trusty-memory`'s `Bm25Supervisor`, carrying the scars of #2845,
//! #2846 and #5085. This module is that supervisor with its BM25-specific parts
//! lifted out, so the next service inherits the hardening instead of
//! re-earning it.
//!
//! What: [`UdsServiceSupervisor`] owns a keyed population of children, each
//! serving one Unix socket. [`UdsServiceSupervisor::ensure_running`] resolves an
//! instance key to a socket path, spawning if it has to, and is the whole public
//! surface a caller needs. Configuration is per-service — see `config.rs` for why
//! that is not a nicety.
//!
//! **Two invariants a reader must not "simplify" away.**
//!
//! 1. Liveness is decided by the SOCKET, not by `try_wait()`. See `probe.rs`.
//!    #6600 does not weaken this: the spawn probe asks the socket first on
//!    every iteration and consults `try_wait` only to answer "has this child
//!    already died", which is a reason to STOP waiting, never a reason to call
//!    a still-running child dead.
//! 2. The timeouts belong to the supervised service, not to supervision. See
//!    `config.rs`.
//!
//! Test: `tests.rs` for the state machine and the limits; `trusty-memory`'s
//! `tests/bm25_supervisor_concurrency.rs` drives the same code with real
//! children for the double-spawn, aggregate-cap, dead-child, unserved-socket
//! and evicted-live-child-flush cases.

mod child;
mod config;
mod error;
mod probe;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use crate::uds::probe::{SocketVerdict, probe_socket_verdict, socket_is_serving};
pub use child::over_rss_limit;
pub use config::{
    DEFAULT_CONNECT_PROBE_TIMEOUT, DEFAULT_INITIAL_PROBE_INTERVAL, DEFAULT_MAX_PROBE_INTERVAL,
    ServiceTimeouts, SpawnSpec, SupervisorConfig,
};
pub use error::SupervisorError;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::Mutex;

use child::{ChildHandle, remove_socket_file, spawn_child, terminate_child};

// #6600: `wait_for_spawn` watches the child as well as the socket, so a child
// that dies before binding is reported as `ChildExited` within one probe
// interval instead of as a `SpawnTimeout` at the end of the whole budget.

/// Supervisor that owns a keyed population of UDS-serving child processes.
///
/// Why: a caller wants one function that handles the four states a child can be
/// in — externally managed, already-spawned, dead-and-needs-restart,
/// never-spawned — without each call site reimplementing the probe-and-spawn
/// logic, and without any of them getting the fail-open cases wrong.
///
/// What: a `Mutex<HashMap<String, ChildHandle>>` keyed by instance. All public
/// methods take `&self` so the supervisor lives behind an `Arc` and is shared
/// across handlers. The map mutex is fine-grained — it protects only the map;
/// spawns and socket probes run with it released so a slow startup for one
/// instance never blocks another's lookup.
///
/// Test: `tests.rs`, plus `trusty-memory`'s concurrency suite.
pub struct UdsServiceSupervisor {
    config: SupervisorConfig,
    children: Mutex<HashMap<String, ChildHandle>>,
    /// Serialises the spawn sequence (re-check → drain doomed → adopt-check →
    /// enforce limits → spawn → probe → insert).
    ///
    /// Why (#2845): the map mutex is released before spawning, which left two
    /// gaps. Two concurrent calls for the SAME key could both reach the spawn
    /// step and the second insert would silently drop the first child. And a
    /// cross-key fan-out could have every instance in flight at once, so a cap
    /// checked per-call is satisfied by each caller individually and violated in
    /// aggregate — the same shape as the unbounded fan-out that 503-stormed
    /// trusty-search. Serialising spawns closes both: the cap is evaluated
    /// against a map nobody else is mutating, and a double spawn is impossible
    /// because the second caller re-checks after acquiring.
    spawn_gate: Mutex<()>,
    /// Monotonic tick source for `ChildHandle::last_used`.
    clock: AtomicU64,
    /// Children reaped by the cap or the RSS limit — never by `shutdown`.
    reaped: AtomicU64,
    /// Child processes this supervisor has launched.
    ///
    /// Why: without it a double spawn is invisible from outside. Two racing
    /// callers for one key both launch a child; the loser's bind fails with
    /// EADDRINUSE and its process dies, so the map still holds exactly one entry
    /// and `supervised_count()` reports the same number whether the
    /// serialisation worked or not. Counting launches is what makes `spawn_gate`
    /// a claim a test can falsify.
    spawned: AtomicU64,
    /// Children evicted by [`Self::lookup_live`] that are still ALIVE and owe a
    /// graceful shutdown.
    ///
    /// Why (#5085/#5119): every other eviction here removes a corpse, so
    /// dropping the handle — and letting `kill_on_drop` SIGKILL it — is free.
    /// The unserved-socket arm is the first that evicts a LIVE child, and a live
    /// child may hold acked-but-unflushed work that only its own SIGTERM handler
    /// writes out. A bare drop discards it with nothing left to recover from.
    ///
    /// Why a queue rather than a detached kill task: the doomed child unlinks
    /// the socket path as the last step of its own shutdown. Terminating it
    /// concurrently with the respawn would let that unlink land AFTER the
    /// replacement bound the same path, leaving the replacement alive and
    /// permanently unreachable — #5085 reintroduced by its own fix. Draining the
    /// queue UNDER THE SPAWN GATE, before the replacement is spawned, is what
    /// orders the two so that cannot happen.
    doomed: Mutex<Vec<(String, ChildHandle)>>,
}

impl UdsServiceSupervisor {
    /// Construct an empty supervisor for one service.
    pub fn new(config: SupervisorConfig) -> Self {
        Self {
            config,
            children: Mutex::new(HashMap::new()),
            spawn_gate: Mutex::new(()),
            clock: AtomicU64::new(0),
            reaped: AtomicU64::new(0),
            spawned: AtomicU64::new(0),
            doomed: Mutex::new(Vec::new()),
        }
    }

    /// The configuration this supervisor was built with.
    pub fn config(&self) -> &SupervisorConfig {
        &self.config
    }

    /// Cap on concurrently-live children.
    pub fn max_live(&self) -> usize {
        self.config.max_live
    }

    /// Per-child RSS ceiling in MB; `None` means enforcement is off.
    pub fn rss_limit_mb(&self) -> Option<u64> {
        self.config.rss_limit_mb
    }

    /// How many children have been reaped by the cap or the RSS limit.
    ///
    /// Why: "the cap is configured" and "the cap did something" are different
    /// claims, and #2846 is the record of a limit that only ever made the first
    /// one. `shutdown` does not increment this.
    pub fn reaped_count(&self) -> u64 {
        self.reaped.load(Ordering::Relaxed)
    }

    /// How many child processes this supervisor has launched.
    pub fn spawned_count(&self) -> u64 {
        self.spawned.load(Ordering::Relaxed)
    }

    /// Number of instances currently supervised.
    pub async fn supervised_count(&self) -> usize {
        self.children.lock().await.len()
    }

    /// Ensure a child is serving `socket_path` for `key`, and return that path.
    ///
    /// Why: returning the socket path rather than a connected client keeps the
    /// supervisor free of any dependency on a service's wire protocol and lets
    /// the caller decide whether to build one client per call or cache.
    ///
    /// What, in order: (1) external mode returns the path untouched, so an
    /// operator running the service under `tctl` keeps its lifecycle. (2) The
    /// already-supervised fast path — a stored child that is both un-reaped and
    /// serving. (3) Under the spawn gate: re-check, then drain the doomed queue,
    /// then adopt a socket some other process already bound (verified, see
    /// below), then enforce the limits BEFORE growing the population, then spawn
    /// and probe. `spec` is called only at (3) — a service that is already
    /// running never pays for locating its binary, and a missing binary is not
    /// an error on any earlier path.
    ///
    /// Adoption is verified, not assumed. ADR-0034 §3 makes the socket's
    /// filesystem permissions the credential the relay hop rests on, and this is
    /// the one path where the service's own `bind_hardened` never runs — so an
    /// unhardened socket would otherwise be adopted silently. A socket that is
    /// serving but fails [`crate::uds::verify_socket_for_connect`] produces
    /// [`SupervisorError::UntrustedSocket`] rather than a spawn that dies on
    /// EADDRINUSE.
    ///
    /// A spawned child that EXITS before the socket answers is reported as
    /// [`SupervisorError::ChildExited`], carrying its status and the tail of
    /// its stderr, within one probe interval (#6600). Before that, such a child
    /// spent the whole `spawn_probe` budget and then blamed the budget.
    ///
    /// Test: `external_mode_skips_spawn`,
    /// `adoption_refuses_a_world_writable_socket`,
    /// `a_child_that_exits_before_binding_reports_its_status_and_stderr`,
    /// `adoption_accepts_a_hardened_socket_without_spawning`,
    /// `a_serving_child_is_reused_without_a_spawn`, plus `trusty-memory`'s
    /// concurrency suite for the racing cases.
    pub async fn ensure_running<F>(
        &self,
        key: &str,
        socket_path: &Path,
        spec: F,
    ) -> Result<PathBuf, SupervisorError>
    where
        F: FnOnce() -> Result<SpawnSpec, Box<dyn std::error::Error + Send + Sync + 'static>>,
    {
        let service = self.config.service.as_str();

        if self.config.external_mode_enabled() {
            tracing::debug!(
                service = %service,
                instance = %key,
                socket = %socket_path.display(),
                "external mode — skipping spawn supervision"
            );
            return Ok(socket_path.to_path_buf());
        }

        if let Some(path) = self.lookup_live(key).await {
            return Ok(path);
        }

        // Everything below mutates the population, so it runs under the gate.
        let _spawn = self.spawn_gate.lock().await;

        // A concurrent caller for this same key may have spawned it while we
        // waited. Without this re-check the cap could be exceeded by exactly the
        // number of racing callers.
        if let Some(path) = self.lookup_live(key).await {
            return Ok(path);
        }

        // #5085: settle any live child the lookups above evicted BEFORE the
        // socket is probed or rebound. The doomed child flushes and unlinks this
        // very path on its way out, so letting it overlap the respawn would
        // strand the replacement on an unlinked socket.
        self.reap_doomed().await;

        if socket_is_serving(socket_path, self.config.timeouts.connect_probe).await {
            crate::uds::verify_socket_for_connect(socket_path).map_err(|source| {
                SupervisorError::UntrustedSocket {
                    service: service.to_string(),
                    key: key.to_string(),
                    socket: socket_path.to_path_buf(),
                    source: Box::new(source),
                }
            })?;
            tracing::info!(
                service = %service,
                instance = %key,
                socket = %socket_path.display(),
                "socket already responding — not spawning a new child"
            );
            return Ok(socket_path.to_path_buf());
        }

        // Reaping AFTER the spawn would let the population momentarily exceed
        // the cap, and under a fan-out that moment is when every other caller is
        // also spawning.
        self.enforce_limits(1).await;

        crate::uds::check_sun_path_budget(socket_path).map_err(|source| {
            SupervisorError::SocketPath {
                service: service.to_string(),
                key: key.to_string(),
                source: Box::new(source),
            }
        })?;

        let spec = spec().map_err(|source| SupervisorError::SpawnSpec {
            service: service.to_string(),
            key: key.to_string(),
            source,
        })?;
        let detached = self.config.detached;
        let mut spawned = spawn_child(service, key, &spec, detached).await?;
        self.spawned.fetch_add(1, Ordering::Relaxed);

        match probe::wait_for_spawn(socket_path, &self.config.timeouts, &mut spawned.child).await {
            probe::SpawnWait::Bound => {}
            // #6600: the child is already gone. Nothing to terminate, and the
            // diagnosis the caller needs is its status and stderr — not the
            // probe budget, which had nothing to do with it.
            probe::SpawnWait::Exited(status) => {
                let stderr = spawned.stderr_tail().await;
                tracing::warn!(
                    service = %service,
                    instance = %key,
                    socket = %socket_path.display(),
                    ?status,
                    "spawned child exited before binding its socket"
                );
                drop(spawned);
                return Err(SupervisorError::ChildExited {
                    service: service.to_string(),
                    key: key.to_string(),
                    socket: socket_path.to_path_buf(),
                    status,
                    stderr,
                });
            }
            probe::SpawnWait::TimedOut => {
                // The child that never bound is killed here rather than by
                // `kill_on_drop`, because the stderr tail below can only be read
                // once the write end of the pipe is closed — which happens when
                // the process dies. Nothing was acked to it, so there is nothing
                // to flush.
                // #6350: a DETACHED child was spawned without `kill_on_drop`, so
                // dropping it would leave a process that never bound running with
                // nothing left holding a handle to it. Terminate it here instead.
                if detached {
                    let _ =
                        terminate_child(&mut spawned.child, self.config.timeouts.sigterm_patience)
                            .await;
                } else {
                    let _ = spawned.child.kill().await;
                }
                // #6600 review: the child's own last lines say whether the
                // budget was too small or the child was never going to bind.
                let stderr = spawned.stderr_tail().await;
                drop(spawned);
                return Err(SupervisorError::SpawnTimeout {
                    service: service.to_string(),
                    key: key.to_string(),
                    socket: socket_path.to_path_buf(),
                    budget: self.config.timeouts.spawn_probe,
                    stderr,
                });
            }
        }

        let mut child = spawned.child;

        tracing::info!(
            service = %service,
            instance = %key,
            socket = %socket_path.display(),
            program = %spec.program.display(),
            detached,
            "spawned supervised child"
        );

        // #6350: a detached child owns its own lifetime (its idle window), so it
        // is deliberately NOT entered into the population map — nothing in this
        // supervisor may reap it, and the next caller adopts it through the
        // already-serving check above. The waiter exists only so the process is
        // not left a zombie when this supervisor outlives it; it never kills,
        // and if this process exits first the child keeps serving.
        if detached {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(socket_path.to_path_buf());
        }

        let mut guard = self.children.lock().await;
        guard.insert(
            key.to_string(),
            ChildHandle {
                child,
                socket_path: socket_path.to_path_buf(),
                last_used: self.tick(),
            },
        );
        let live = guard.len();
        drop(guard);
        tracing::debug!(service = %service, live, cap = self.config.max_live, "child count");
        Ok(socket_path.to_path_buf())
    }

    /// Next value of the monotonic LRU clock.
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }

    /// Resolve `key` to a live child's socket, refreshing its LRU stamp.
    ///
    /// Why: `ensure_running` needs this answer twice — once on the fast path and
    /// once under the spawn gate — and the two must agree exactly, including the
    /// eviction. Two copies would be two chances to drift.
    ///
    /// Liveness is decided by TWO questions (#5085). `try_wait()` answers "has
    /// this child been reaped", which is not "is it serving" — see `probe.rs` for
    /// the measurement. Trusting it alone made this the fast path's fail-open.
    ///
    /// What: returns `Some(socket_path)` when the stored child is un-reaped AND
    /// its socket is not definitively unserved, stamping it most-recently-used
    /// so the LRU victim choice reflects real traffic. A child that has exited,
    /// whose status cannot be read, or whose socket answers
    /// [`SocketVerdict::NotServing`] is removed and `None` returned so the caller
    /// falls through to a fresh spawn. [`SocketVerdict::Inconclusive`] keeps the
    /// child, so an unanswerable connect never turns load into a respawn storm.
    /// A child evicted while still alive goes to [`Self::doomed`] rather than
    /// being dropped, because it owes a flush. Removing a corpse does NOT count
    /// as a reap — nothing was reclaimed.
    ///
    /// The probe runs with the map lock released, so a slow connect never blocks
    /// another instance's lookup. Eviction then re-acquires and fires only if the
    /// map still holds the same child, so a replacement spawned concurrently is
    /// never evicted by this call's stale verdict.
    /// Test: `a_dead_child_is_evicted_without_counting_as_a_reap`,
    /// `an_unserved_live_child_is_queued_for_graceful_termination`.
    async fn lookup_live(&self, key: &str) -> Option<PathBuf> {
        let (pid, path) = {
            let stamp = self.tick();
            let mut guard = self.children.lock().await;
            let entry = guard.get_mut(key)?;
            match entry.child.try_wait() {
                Ok(None) => {
                    entry.last_used = stamp;
                    (entry.child.id(), entry.socket_path.clone())
                }
                Ok(Some(status)) => {
                    tracing::warn!(
                        service = %self.config.service,
                        instance = %key,
                        ?status,
                        "supervised child exited unexpectedly — attempting one restart"
                    );
                    guard.remove(key);
                    return None;
                }
                Err(e) => {
                    tracing::warn!(
                        service = %self.config.service,
                        instance = %key,
                        "try_wait failed: {e:#} — evicting and retrying"
                    );
                    guard.remove(key);
                    return None;
                }
            }
        };

        // Lock released. #5085: an un-reaped child is not the same claim as a
        // serving one; ask the socket the caller will actually use.
        if probe_socket_verdict(&path, self.config.timeouts.connect_probe).await
            != SocketVerdict::NotServing
        {
            return Some(path);
        }

        tracing::warn!(
            service = %self.config.service,
            instance = %key,
            socket = %path.display(),
            pid = ?pid,
            "supervised child is not serving its socket — evicting and respawning"
        );
        let evicted = {
            let mut guard = self.children.lock().await;
            // #5085: identify the child by pid so a replacement spawned while the
            // probe ran is never evicted on this call's stale verdict. Two
            // unreadable pids must not alias into "same child", so an absent pid
            // on either side is never a match.
            let same_child = matches!(
                (pid, guard.get(key).and_then(|h| h.child.id())),
                (Some(ours), Some(current)) if ours == current
            );
            if same_child { guard.remove(key) } else { None }
        };
        if let Some(handle) = evicted {
            self.doomed.lock().await.push((key.to_string(), handle));
        }
        None
    }

    /// SIGTERM every child [`Self::lookup_live`] evicted while it was still
    /// alive, and wait for each to exit.
    ///
    /// Why (#5085/#5119): see the [`Self::doomed`] field — a live child may hold
    /// acked work only its own SIGTERM handler writes, and it unlinks the shared
    /// socket path on its way out, so it must be fully gone before a replacement
    /// binds that path.
    /// What: drains the queue and runs the same terminate + unlink sequence the
    /// reap and shutdown paths use. Callers MUST hold the spawn gate — that is
    /// what orders this before the respawn. Does NOT increment `reaped`: this is
    /// a restart, not a reclamation. A no-op (one uncontended lock) when nothing
    /// has been evicted, which is the overwhelmingly common case.
    /// Test: `trusty-memory`'s `an_evicted_live_child_is_given_a_chance_to_flush`.
    async fn reap_doomed(&self) {
        let doomed: Vec<(String, ChildHandle)> = {
            let mut guard = self.doomed.lock().await;
            if guard.is_empty() {
                return;
            }
            std::mem::take(&mut *guard)
        };
        for (key, mut handle) in doomed {
            tracing::info!(
                service = %self.config.service,
                instance = %key,
                pid = ?handle.child.id(),
                "gracefully terminating an evicted child so it can flush"
            );
            if let Err(e) =
                terminate_child(&mut handle.child, self.config.timeouts.sigterm_patience).await
            {
                tracing::warn!(instance = %key, "graceful termination error: {e:#}");
            }
            remove_socket_file(&self.config.service, &key, &handle.socket_path).await;
        }
    }

    /// Bring the live population within both limits, leaving room for `headroom`
    /// more.
    ///
    /// Why (#2845 + #2846): these are the two failures trusty-search shipped — an
    /// unbounded fan-out, and a memory limit that was declared but never compared
    /// against anything. Enforcing both in one place, on the path that grows the
    /// population, is what makes them limits rather than documentation.
    /// What: two passes. Every child whose MEASURED RSS is at or above the
    /// ceiling is selected regardless of recency — a leaking child is the wrong
    /// thing to keep no matter how recently it was used — while a child whose RSS
    /// cannot be measured is left alone. Then, while the survivors plus
    /// `headroom` would exceed `max_live`, the least-recently-used survivor is
    /// selected. Victims are removed under the lock and terminated with it
    /// released, so the SIGTERM wait never blocks another instance's lookup.
    /// Test: `exceeding_the_cap_reaps_the_least_recently_used`,
    /// `over_rss_limit_children_are_reaped`.
    async fn enforce_limits(&self, headroom: usize) {
        let mut victims: Vec<(String, ChildHandle, &'static str)> = Vec::new();
        {
            let mut guard = self.children.lock().await;

            let over: Vec<String> = guard
                .iter()
                .filter(|(_, h)| over_rss_limit(h.child.id(), self.config.rss_limit_mb))
                .map(|(k, _)| k.clone())
                .collect();
            for key in over {
                if let Some(h) = guard.remove(&key) {
                    victims.push((key, h, "rss"));
                }
            }

            while guard.len() + headroom > self.config.max_live {
                let Some(lru) = guard
                    .iter()
                    .min_by_key(|(_, h)| h.last_used)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                if let Some(h) = guard.remove(&lru) {
                    victims.push((lru, h, "cap"));
                }
            }
        }

        for (key, mut handle, reason) in victims {
            tracing::info!(
                service = %self.config.service,
                instance = %key,
                reason,
                cap = self.config.max_live,
                rss_limit_mb = ?self.config.rss_limit_mb,
                pid = ?handle.child.id(),
                "reaping supervised child to stay within limits"
            );
            if let Err(e) =
                terminate_child(&mut handle.child, self.config.timeouts.sigterm_patience).await
            {
                tracing::warn!(instance = %key, "reap error: {e:#}");
            }
            remove_socket_file(&self.config.service, &key, &handle.socket_path).await;
            self.reaped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Graceful shutdown: SIGTERM all owned children, reap them, clean up their
    /// sockets.
    ///
    /// Why: the parent's normal exit is a SIGTERM from launchd or a ctrl-c.
    /// Relying on `kill_on_drop` instead would skip each child's own cleanup and
    /// leave any acked-but-unflushed work discarded.
    /// What: drains the doomed queue first (a child evicted but never respawned
    /// still owes a flush), then the map. Idempotent.
    /// Test: `shutdown_with_no_children_is_noop`,
    /// `shutdown_does_not_count_as_a_limit_reap`.
    pub async fn shutdown(&self) {
        self.reap_doomed().await;
        let handles: Vec<(String, ChildHandle)> = self.children.lock().await.drain().collect();

        for (key, mut entry) in handles {
            tracing::info!(
                service = %self.config.service,
                instance = %key,
                pid = ?entry.child.id(),
                "shutting down supervised child"
            );
            if let Err(e) =
                terminate_child(&mut entry.child, self.config.timeouts.sigterm_patience).await
            {
                tracing::warn!(instance = %key, "shutdown encountered an error: {e:#}");
            }
            remove_socket_file(&self.config.service, &key, &entry.socket_path).await;
        }
    }
}

impl std::fmt::Debug for UdsServiceSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately does not lock: `Debug` may be invoked while another task
        // holds the guard, and a placeholder beats a deadlock.
        f.debug_struct("UdsServiceSupervisor")
            .field("service", &self.config.service)
            .field("children", &"<locked>")
            .finish()
    }
}
