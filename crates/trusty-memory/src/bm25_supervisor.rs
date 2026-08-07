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
//! The daemon population is BOUNDED (#2845 / #2846). Until this module grew a
//! cap, the only thing that ever removed an entry from the map was `shutdown`,
//! so one cross-palace `memory_recall_all` over ~99 palaces would leave 99
//! child processes resident for the trusty-memory daemon's whole life — and
//! each one's memory scales with its palace's drawer text, because
//! `PalaceBm25Index` retains every document's full text in a `BTreeMap`. Two
//! limits now apply on every `ensure_running`: a cap on concurrently-live
//! daemons with least-recently-used reaping, and a per-daemon RSS ceiling that
//! is compared against a real measurement rather than merely declared. Spawns
//! are serialised so a burst fan-out cannot satisfy the cap per-caller and
//! violate it in aggregate.
//!
//! Test: unit tests in `bm25_supervisor_tests.rs` cover the external-mode
//! opt-out, the "already running" probe, the `shutdown` reaping path with no
//! daemon ever spawned, and the two limits. An `#[ignore]`-tagged integration
//! test in `tests/bm25_supervisor_e2e.rs` drives real `trusty-bm25-daemon`
//! children end-to-end (index + search + cap eviction + shutdown) when the
//! binary is on PATH or `TRUSTY_BM25_DAEMON_BIN` is set.

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

/// Environment variable that overrides the cap on concurrently-live daemons.
///
/// Why (#2845): `ensure_running` is called once per palace, and one
/// `memory_recall_all` touches every palace on disk — ~99 on this host. Without
/// a cap the supervisor would hold 99 child processes for the rest of the
/// trusty-memory daemon's life, because nothing but `shutdown` ever removed an
/// entry from the map. Drawer distribution is heavily skewed (one palace holds
/// 57% of the corpus, ~80 hold nothing), so the working set is a handful of
/// palaces and a small cap costs almost nothing while bounding the worst case.
/// What: env var name `TRUSTY_BM25_MAX_DAEMONS`. Parsed as `usize`; values
/// below 1 and unparseable values fall back to [`DEFAULT_MAX_LIVE_DAEMONS`].
/// Test: `max_live_daemons_honours_env_override`.
pub const ENV_MAX_DAEMONS: &str = "TRUSTY_BM25_MAX_DAEMONS";

/// Environment variable that overrides the per-daemon RSS ceiling, in MB.
///
/// Why (#2846): trusty-search declared an `rss_limit_mb` and never compared it
/// against anything; the process grew to 2.2x that limit and was OOM-killed.
/// A BM25 daemon's memory scales linearly with its palace's drawer text
/// because `PalaceBm25Index` retains every document's full text in a
/// `BTreeMap`, so an unbounded palace is an unbounded daemon. This limit is
/// enforced on every `ensure_running`, not merely reported.
/// What: env var name `TRUSTY_BM25_RSS_LIMIT_MB`. `0` disables enforcement;
/// unparseable values fall back to [`DEFAULT_RSS_LIMIT_MB`].
/// Test: `rss_limit_honours_env_override`, `over_rss_limit_children_are_reaped`.
pub const ENV_RSS_LIMIT_MB: &str = "TRUSTY_BM25_RSS_LIMIT_MB";

/// Default cap on concurrently-live BM25 daemons.
///
/// Why: three covers the realistic working set — the palace you are in, the
/// one you just cross-referenced, and one in flight — while keeping the
/// worst case at three subprocesses instead of ninety-nine. Raising it is a
/// one-env-var decision for operators who genuinely fan out wider.
/// What: `3`.
/// Test: `default_cap_is_three`.
pub const DEFAULT_MAX_LIVE_DAEMONS: usize = 3;

/// Default per-daemon RSS ceiling in megabytes.
///
/// Why: the whole drawer corpus across ~99 palaces is single-digit MB of text,
/// so a single daemon holding more than 512 MB is not holding drawers — it is
/// leaking, and #2846 is the record of what happens when nobody notices.
/// Generous enough never to reap a healthy daemon; tight enough that a runaway
/// one is reaped in seconds rather than at OOM time.
/// What: `512`.
/// Test: `rss_limit_honours_env_override` pins the parse; the reap path is
/// covered by `over_rss_limit_children_are_reaped`.
pub const DEFAULT_RSS_LIMIT_MB: u64 = 512;

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

/// How long the supervisor waits after SIGTERM before escalating to SIGKILL.
///
/// Why: strictly greater than the daemon's own `SHUTDOWN_FLUSH_TIMEOUT` (2 s),
/// because the daemon needs signal delivery, the flush itself, socket cleanup
/// and exit inside this window. An equal budget means the SIGKILL can land
/// mid-flush and lose the write window the flush exists to save.
/// What: 5 seconds.
/// Test: `sigterm_patience_exceeds_the_daemon_flush_budget`.
const SIGTERM_PATIENCE: Duration = Duration::from_secs(5);

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
    /// Monotonic tick of the last `ensure_running` that resolved to this
    /// child. The LRU victim is the entry with the smallest value.
    ///
    /// Why a counter and not an `Instant`: two `ensure_running` calls inside
    /// the same clock tick would compare equal and make the victim choice
    /// arbitrary, which is exactly the case a burst fan-out produces. A
    /// strictly-increasing counter gives a total order regardless of clock
    /// resolution.
    last_used: u64,
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
    /// Serialises the spawn sequence (re-check → enforce limits → spawn →
    /// probe → insert).
    ///
    /// Why (#2845): the map mutex is released before spawning so slow startups
    /// for one palace don't block lookups for another, but that left two gaps.
    /// First, two concurrent calls for the SAME palace could both reach the
    /// spawn step and the second insert would silently drop the first child.
    /// Second, a cross-palace fan-out could have every palace in flight at
    /// once, so a cap checked per-call would be satisfied by each caller
    /// individually and violated in aggregate — the same shape as the
    /// unbounded fan-out that 503-stormed trusty-search. Serialising spawns
    /// closes both: the cap is evaluated against a map nobody else is
    /// mutating, and a double spawn is impossible because the second caller
    /// re-checks the map after acquiring.
    /// The cost is that a cold N-palace fan-out spawns serially. That is the
    /// intended trade — with a cap of three, a parallel fan-out would spend
    /// its time evicting daemons it just spawned.
    spawn_gate: Mutex<()>,
    /// Cap on concurrently-live daemons. Resolved once at construction.
    max_live: usize,
    /// Per-daemon RSS ceiling in MB. `None` disables enforcement.
    ///
    /// Why `Option` and not a sentinel `0`: "no ceiling" and "a ceiling of
    /// zero" are opposite instructions, and a single `u64` cannot say both.
    /// Encoding disabled-ness in the type keeps the operator's `=0` opt-out
    /// from colliding with the tightest possible limit, which is the value the
    /// enforcement tests need in order to be deterministic.
    rss_limit_mb: Option<u64>,
    /// Monotonic tick source for `ChildHandle::last_used`.
    clock: std::sync::atomic::AtomicU64,
    /// Count of children reaped by the cap or the RSS limit (never by
    /// `shutdown`). Observability + test assertion surface.
    reaped: std::sync::atomic::AtomicU64,
    /// Count of `trusty-bm25-daemon` child processes this supervisor has
    /// launched.
    ///
    /// Why: without it, a double spawn is invisible from outside. Two racing
    /// callers for one palace both launch a daemon; the loser's bind fails
    /// with EADDRINUSE and its process dies, and the map still ends up holding
    /// exactly one entry — so `supervised_count()` reports the same number
    /// whether the serialisation worked or not. Counting launches is what
    /// makes `spawn_gate` a claim a test can falsify.
    /// What: monotonic, incremented once per successful `Command::spawn`
    /// regardless of whether the socket probe later succeeds.
    /// Test: `concurrent_callers_for_one_palace_spawn_exactly_one_daemon`.
    spawned: std::sync::atomic::AtomicU64,
}

impl Bm25Supervisor {
    /// Construct an empty supervisor with limits read from the environment.
    ///
    /// Why: cheap, allocation-light constructor so the supervisor can be
    /// built unconditionally at startup and only allocate when a palace
    /// first asks for a daemon. Resolving the limits here (rather than on
    /// each `ensure_running`) means a mid-flight env mutation cannot make the
    /// cap wobble between two concurrent calls.
    /// What: returns a supervisor with an empty per-palace map and the
    /// caps from [`ENV_MAX_DAEMONS`] / [`ENV_RSS_LIMIT_MB`], falling back to
    /// [`DEFAULT_MAX_LIVE_DAEMONS`] / [`DEFAULT_RSS_LIMIT_MB`].
    /// Test: `default_cap_is_three`, `max_live_daemons_honours_env_override`.
    pub fn new() -> Self {
        Self::with_limits(max_live_from_env(), rss_limit_from_env())
    }

    /// Construct a supervisor with explicit limits, bypassing the environment.
    ///
    /// Why: the limit-enforcement tests must pin a cap of 1 or an RSS ceiling
    /// of 1 MB without mutating process-global env vars that sibling tests
    /// race against. Production code should use [`Self::new`].
    /// What: `max_live` is clamped to at least 1 — a cap of zero would reap
    /// every daemon the instant it spawned, which is a wedge, not a limit.
    /// `rss_limit_mb: None` disables RSS enforcement; `Some(n)` reaps any child
    /// measuring at or above `n` MB.
    /// Test: `cap_of_one_keeps_a_single_daemon_live`.
    pub fn with_limits(max_live: usize, rss_limit_mb: Option<u64>) -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
            spawn_gate: Mutex::new(()),
            max_live: max_live.max(1),
            rss_limit_mb,
            clock: std::sync::atomic::AtomicU64::new(0),
            reaped: std::sync::atomic::AtomicU64::new(0),
            spawned: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Cap on concurrently-live daemons this supervisor enforces.
    pub fn max_live(&self) -> usize {
        self.max_live
    }

    /// Per-daemon RSS ceiling in MB; `None` means enforcement is off.
    pub fn rss_limit_mb(&self) -> Option<u64> {
        self.rss_limit_mb
    }

    /// How many children have been reaped by the cap or the RSS limit.
    ///
    /// Why: "the cap is configured" and "the cap did something" are different
    /// claims, and #2846 is the record of a limit that only ever made the
    /// first one. This counter makes the second checkable — by a test, and by
    /// an operator reading it out of the daemon.
    /// What: monotonic count; `shutdown` does not increment it.
    /// Test: `exceeding_the_cap_reaps_the_least_recently_used`.
    pub fn reaped_count(&self) -> u64 {
        self.reaped.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many daemon processes this supervisor has launched.
    ///
    /// Why: this is the only externally-visible difference between "spawns are
    /// serialised" and "spawns race". A double spawn leaves the map holding
    /// one entry either way — the loser's daemon fails its bind and dies — so
    /// `supervised_count()` cannot tell the two apart and a test written
    /// against it passes with `spawn_gate` deleted.
    /// What: monotonic count of successful `Command::spawn` calls.
    /// Test: `concurrent_callers_for_one_palace_spawn_exactly_one_daemon`.
    pub fn spawned_count(&self) -> u64 {
        self.spawned.load(std::sync::atomic::Ordering::Relaxed)
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

        // ── (2) Already-supervised fast path ───────────────────────────────
        // Take the lock briefly to inspect the stored child. We drop the
        // guard before spawning so concurrent calls for OTHER palaces don't
        // queue behind a slow startup probe.
        if let Some(path) = self.lookup_live(palace).await {
            return Ok(path);
        }

        // ── (3) Spawn, serialised ──────────────────────────────────────────
        // Everything from here down mutates the daemon population, so it runs
        // under the spawn gate — see the field's doc comment for why the map
        // mutex alone is not enough.
        let _spawn = self.spawn_gate.lock().await;

        // Re-check under the gate: a concurrent caller for this same palace
        // may have spawned it while we waited. Without this, the cap could be
        // exceeded by exactly the number of racing callers, and the loser's
        // child would be silently dropped by the map insert.
        if let Some(path) = self.lookup_live(palace).await {
            return Ok(path);
        }

        // ── (3a) Socket-already-bound check ────────────────────────────────
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

        // ── (3b) Enforce the limits BEFORE adding to the population ────────
        // Order matters: reaping after the spawn would let the population
        // momentarily exceed the cap, and under a fan-out that moment is when
        // every other caller is also spawning.
        self.enforce_limits(1).await;

        // ── (4) Spawn ──────────────────────────────────────────────────────
        let binary =
            locate_bm25_daemon_binary().context("locate trusty-bm25-daemon binary for spawn")?;
        let child = spawn_child(&binary, palace, data_dir)
            .await
            .inspect(|_| {
                self.spawned
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })
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
                last_used: self.tick(),
            },
        );
        let live = guard.len();
        drop(guard);
        tracing::debug!(live, cap = self.max_live, "bm25 supervisor: daemon count");
        Ok(socket_path)
    }

    /// Next value of the monotonic LRU clock.
    fn tick(&self) -> u64 {
        self.clock
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1)
    }

    /// Resolve `palace` to a live child's socket, refreshing its LRU stamp.
    ///
    /// Why: `ensure_running` needs this answer twice — once on the fast path
    /// and once under the spawn gate — and the two must agree exactly,
    /// including the dead-child eviction. Two copies would be two chances to
    /// drift.
    ///
    /// Liveness is decided by TWO questions, not one (#5085). `try_wait()`
    /// answers "has this child been reaped", which is not the same as "is it
    /// serving": a SIGKILLed daemon closes its listening socket during
    /// `do_exit` and only becomes reapable once the kernel finishes tearing it
    /// down, so for that window `try_wait()` reports `Ok(None)` for a process
    /// that is already refusing connections. Trusting it alone made this the
    /// fast path's fail-open — the caller was handed a socket path nothing was
    /// listening on and nothing reported a problem. The socket the caller is
    /// about to use is the authority, so we ask it too.
    ///
    /// What: returns `Some(socket_path)` when the stored child is both
    /// un-reaped AND its socket is not definitively unserved, and stamps it as
    /// most-recently-used so the LRU victim choice reflects real traffic. A
    /// child that has exited, whose status cannot be read, or whose socket
    /// answers [`SocketVerdict::NotServing`] is removed from the map and `None`
    /// is returned, so the caller falls through to a fresh spawn. An
    /// [`SocketVerdict::Inconclusive`] probe keeps the child: a saturated
    /// accept backlog is a busy daemon, not a dead one, and reaping on it would
    /// turn load into a respawn storm. Removing a corpse does NOT count as a
    /// reap — nothing was reclaimed.
    ///
    /// The probe runs with the map lock released, so a slow connect never
    /// blocks another palace's lookup. Eviction then re-acquires and fires only
    /// if the map still holds the same child, so a replacement spawned
    /// concurrently is never evicted by this call's stale verdict.
    /// Test: `a_dead_child_is_evicted_and_respawned` and
    /// `a_live_child_that_stopped_serving_is_evicted_and_respawned`.
    async fn lookup_live(&self, palace: &str) -> Option<PathBuf> {
        // Phase 1 — under the map lock: reap check, LRU stamp, snapshot.
        let (pid, path) = {
            let stamp = self.tick();
            let mut guard = self.children.lock().await;
            let entry = guard.get_mut(palace)?;
            match entry.child.try_wait() {
                Ok(None) => {
                    entry.last_used = stamp;
                    (entry.child.id(), entry.socket_path.clone())
                }
                Ok(Some(status)) => {
                    tracing::warn!(
                        palace = %palace,
                        ?status,
                        "bm25 daemon exited unexpectedly — attempting one restart"
                    );
                    guard.remove(palace);
                    return None;
                }
                Err(e) => {
                    tracing::warn!(
                        palace = %palace,
                        "bm25 supervisor: try_wait failed: {e:#} — evicting and retrying"
                    );
                    guard.remove(palace);
                    return None;
                }
            }
        };

        // Phase 2 — lock released. #5085: an un-reaped child is not the same
        // claim as a serving one; ask the socket the caller will actually use.
        if probe_socket_verdict(&path).await != SocketVerdict::NotServing {
            tracing::trace!(
                palace = %palace,
                socket = %path.display(),
                "bm25 supervisor: child already running"
            );
            return Some(path);
        }

        // Phase 3 — evict, but only if the map still holds THIS child.
        tracing::warn!(
            palace = %palace,
            socket = %path.display(),
            pid = ?pid,
            "bm25 daemon is not serving its socket — evicting and respawning"
        );
        let mut guard = self.children.lock().await;
        if guard.get(palace).map(|h| h.child.id()) == Some(pid) {
            guard.remove(palace);
        }
        None
    }

    /// Bring the live-daemon population within both limits, leaving room for
    /// `headroom` more.
    ///
    /// Why (#2845 + #2846): these are the two failures trusty-search shipped —
    /// an unbounded fan-out and a memory limit that was declared but never
    /// compared against anything. Enforcing both in one place, on the path
    /// that grows the population, is what makes them limits rather than
    /// documentation.
    /// What: two passes. First, every child whose measured RSS exceeds
    /// `rss_limit_mb` is selected regardless of recency — a leaking daemon is
    /// the wrong thing to keep no matter how recently it was used. A child
    /// whose RSS cannot be measured is left alone: `None` means "no reading",
    /// and reaping on a failed measurement would kill healthy daemons on any
    /// platform where the read is unavailable. Second, while the survivors
    /// plus `headroom` would exceed `max_live`, the least-recently-used
    /// survivor is selected. Victims are removed from the map under the lock,
    /// then terminated with the lock released so the SIGTERM wait never blocks
    /// another palace's lookup.
    /// Test: `exceeding_the_cap_reaps_the_least_recently_used`,
    /// `over_rss_limit_children_are_reaped`,
    /// `over_rss_limit_is_false_without_a_measurement`.
    async fn enforce_limits(&self, headroom: usize) {
        let mut victims: Vec<(String, ChildHandle, &'static str)> = Vec::new();
        {
            let mut guard = self.children.lock().await;

            let over: Vec<String> = guard
                .iter()
                .filter(|(_, h)| over_rss_limit(h.child.id(), self.rss_limit_mb))
                .map(|(p, _)| p.clone())
                .collect();
            for palace in over {
                if let Some(h) = guard.remove(&palace) {
                    victims.push((palace, h, "rss"));
                }
            }

            while guard.len() + headroom > self.max_live {
                let Some(lru) = guard
                    .iter()
                    .min_by_key(|(_, h)| h.last_used)
                    .map(|(p, _)| p.clone())
                else {
                    break;
                };
                if let Some(h) = guard.remove(&lru) {
                    victims.push((lru, h, "cap"));
                }
            }
        }

        for (palace, mut handle, reason) in victims {
            tracing::info!(
                palace = %palace,
                reason,
                cap = self.max_live,
                rss_limit_mb = ?self.rss_limit_mb,
                pid = ?handle.child.id(),
                "bm25 supervisor: reaping daemon to stay within limits"
            );
            if let Err(e) = terminate_child(&mut handle.child).await {
                tracing::warn!(palace = %palace, "bm25 daemon reap error: {e:#}");
            }
            remove_socket_file(&palace, &handle.socket_path).await;
            self.reaped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
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
            remove_socket_file(&palace, &entry.socket_path).await;
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

/// Best-effort unlink of a daemon's socket file.
///
/// Why: the daemon's own SIGTERM handler unlinks the socket on a clean exit,
/// but a SIGKILLed daemon leaves the file behind and the next spawn for that
/// palace then fails to bind with EADDRINUSE. Both the shutdown path and the
/// limit-enforcement reap path need this, and a reaped palace is exactly the
/// one most likely to be spawned again shortly.
/// What: `remove_file`; `NotFound` is the expected clean-exit case and is not
/// logged. Any other error is logged at `debug!` and ignored — a stale socket
/// costs one failed spawn, not correctness.
/// Test: covered by `bm25_supervisor_e2e`'s shutdown assertion.
async fn remove_socket_file(palace: &str, socket_path: &Path) {
    if let Err(e) = tokio::fs::remove_file(socket_path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(
                palace = %palace,
                socket = %socket_path.display(),
                "could not remove bm25 daemon socket (likely already cleaned up): {e}"
            );
        }
    }
}

/// Decide whether a child has breached the RSS ceiling.
///
/// Why (#2846): the whole point of this limit is that it is compared against a
/// real measurement, so the two ways a measurement can be missing need an
/// explicit, tested answer rather than whatever `unwrap_or` happens to do. A
/// child with no pid has already exited — there is nothing to reclaim. A pid
/// whose RSS cannot be read yields `None`, which means "no reading", NOT
/// "zero"; reaping on it would kill every healthy daemon on any platform where
/// the read is unavailable, turning a memory guardrail into an outage.
/// What: `false` when enforcement is off, when the pid is gone, or when the
/// reading is unavailable. Otherwise `true` iff the measured MB is at or above
/// the ceiling. Pure — takes the pid rather than the handle so it is testable
/// without a live process.
/// Test: `over_rss_limit_is_false_without_a_measurement`,
/// `over_rss_limit_is_false_when_disabled`.
fn over_rss_limit(pid: Option<u32>, limit_mb: Option<u64>) -> bool {
    let Some(limit) = limit_mb else {
        return false;
    };
    let Some(pid) = pid else {
        return false;
    };
    trusty_common::sys_metrics::process_rss_mb(pid).is_some_and(|mb| mb >= limit)
}

/// Resolve the live-daemon cap from [`ENV_MAX_DAEMONS`].
///
/// Why: an operator who fans out wider than the default working set needs a
/// knob, and a knob that silently ignores a typo is worse than no knob — hence
/// the explicit warn on an unparseable value rather than a quiet default.
/// What: parses the env var as `usize`; `0` and unparseable values fall back
/// to [`DEFAULT_MAX_LIVE_DAEMONS`].
/// Test: `max_live_daemons_honours_env_override`.
fn max_live_from_env() -> usize {
    match std::env::var(ENV_MAX_DAEMONS) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => {
                tracing::warn!(
                    "{ENV_MAX_DAEMONS}={raw:?} is not a positive integer — \
                     using default {DEFAULT_MAX_LIVE_DAEMONS}"
                );
                DEFAULT_MAX_LIVE_DAEMONS
            }
        },
        Err(_) => DEFAULT_MAX_LIVE_DAEMONS,
    }
}

/// Resolve the per-daemon RSS ceiling from [`ENV_RSS_LIMIT_MB`].
///
/// Why: same knob-with-a-typo argument as [`max_live_from_env`], plus one
/// specific to this limit — `0` must be an explicit, documented way to turn
/// enforcement off, not an accident of parsing.
/// What: parses as `u64`; `0` maps to `None` (enforcement off); unparseable
/// values fall back to [`DEFAULT_RSS_LIMIT_MB`].
/// Test: `rss_limit_honours_env_override`.
fn rss_limit_from_env() -> Option<u64> {
    match std::env::var(ENV_RSS_LIMIT_MB) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => {
                tracing::warn!(
                    "{ENV_RSS_LIMIT_MB}={raw:?} is not an integer — \
                     using default {DEFAULT_RSS_LIMIT_MB}"
                );
                Some(DEFAULT_RSS_LIMIT_MB)
            }
        },
        Err(_) => Some(DEFAULT_RSS_LIMIT_MB),
    }
}

/// What a connect attempt says about whether anything is serving a socket.
///
/// Why (#5085): the spawn path only ever needed "did it answer, yes or no",
/// but eviction needs a third answer. Evicting a live daemon because one
/// connect did not complete would turn a load spike into a respawn storm —
/// the daemon is killed, a replacement is spawned into the same contention,
/// and the cycle repeats. Separating "nothing is listening" from "I could not
/// tell" keeps eviction to the case the kernel is unambiguous about.
/// What: `NotServing` is reserved for errors that prove no listener is bound
/// to the path — ENOENT and ECONNREFUSED. A timeout or any other errno
/// (EAGAIN from a saturated backlog, EPERM) is `Inconclusive` and leaves the
/// child alone.
/// Test: `probe_verdict_reports_not_serving_for_a_missing_socket`,
/// `probe_verdict_reports_serving_for_a_bound_socket`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketVerdict {
    /// A connection was accepted — something is serving this path.
    Serving,
    /// The kernel proved no listener is bound (ENOENT / ECONNREFUSED).
    NotServing,
    /// The probe could not settle the question; assume the daemon is fine.
    Inconclusive,
}

/// Quick non-blocking probe — opens a `UnixStream`, immediately closes it.
///
/// Why: we want a single answer to "is something listening on this socket
/// right now?" without depending on `Bm25Client` (which would couple us to the
/// BM25 wire protocol) and without spending more than a few ms.
/// What: attempts `UnixStream::connect` with a short timeout and classifies the
/// outcome per [`SocketVerdict`].
/// Test: `probe_verdict_reports_not_serving_for_a_missing_socket`,
/// `probe_verdict_reports_serving_for_a_bound_socket`.
async fn probe_socket_verdict(path: &Path) -> SocketVerdict {
    // Use a short timeout so an unresponsive (but still bound) socket
    // doesn't stall the probe loop.
    let connect = UnixStream::connect(path);
    match tokio::time::timeout(Duration::from_millis(200), connect).await {
        Ok(Ok(_)) => SocketVerdict::Serving,
        Ok(Err(e)) => match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                SocketVerdict::NotServing
            }
            _ => SocketVerdict::Inconclusive,
        },
        Err(_elapsed) => SocketVerdict::Inconclusive,
    }
}

/// Whether a socket is definitely accepting connections right now.
///
/// Why: the spawn and adoption paths only care about the affirmative case —
/// "may I skip the spawn?" — for which anything short of a completed connect
/// must read as no.
/// What: true iff [`probe_socket_verdict`] returns [`SocketVerdict::Serving`].
/// Test: covered indirectly — every spawn path goes through this probe
/// in a tight loop.
async fn probe_socket(path: &Path) -> bool {
    probe_socket_verdict(path).await == SocketVerdict::Serving
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
/// remove its socket; a SIGKILL is the fallback if the daemon hangs. The wait
/// must exceed the daemon's OWN shutdown budget with margin, not merely match
/// it: `trusty-bm25-daemon` allows itself 2 s for the shutdown flush
/// (`lib.rs`'s `SHUTDOWN_FLUSH_TIMEOUT`) and still needs signal delivery,
/// socket cleanup, and process exit on top. At an equal 2 s the SIGKILL landed
/// inside the very flush the durability fix added — no corruption, since the
/// flush is tmp+rename, but the window's writes were lost. 5 s leaves 3 s of
/// margin over the daemon's worst case.
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

    // Give the child longer than its own flush budget to exit on its own.
    let wait_result = tokio::time::timeout(SIGTERM_PATIENCE, child.wait()).await;
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
            tracing::warn!(
                "bm25 daemon ignored SIGTERM after {SIGTERM_PATIENCE:?} — sending SIGKILL"
            );
            child
                .kill()
                .await
                .context("SIGKILL bm25 daemon after SIGTERM timeout")?;
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "bm25_supervisor_tests.rs"]
mod tests;
