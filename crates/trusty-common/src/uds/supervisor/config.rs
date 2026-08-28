//! Per-service configuration for [`super::UdsServiceSupervisor`] (#5089).
//!
//! Why: the supervisor this generalises carried two timing constants bound to
//! `trusty-bm25-daemon` specifically. `SPAWN_PROBE_TIMEOUT` was 3 s, justified
//! in its own doc comment by "BM25 has no model-loading step" against the
//! embedder's 30 s. `SIGTERM_PATIENCE_SECS` was 5, tied to the daemon's real
//! flush budget by a `const _: () = assert!(SIGTERM_PATIENCE_SECS >
//! trusty_bm25_daemon::SHUTDOWN_FLUSH_TIMEOUT.as_secs(), …)`. Carried across as
//! bare constants both break silently for the first service with a model to
//! load or a longer flush, and each failure wears a misleading costume — a
//! spawn timeout that looks like a broken binary, and a SIGKILL landing inside
//! a flush that discards acked writes with no error anywhere. So they are
//! per-service values here.
//!
//! What: [`ServiceTimeouts`] carries the three numbers that must move together
//! and re-derives the compile-time guard as a precondition of a `const fn`
//! constructor — see [`ServiceTimeouts::new`]. [`SupervisorConfig`] carries the
//! population limits, the log label, and the external-mode opt-out.
//! [`SpawnSpec`] is what the supervisor executes, resolved lazily so a service
//! that is already running never pays for locating its binary.
//!
//! Test: `tests.rs` — `service_timeouts_reject_patience_equal_to_the_flush`,
//! `service_timeouts_carry_probe_defaults_and_honour_overrides`,
//! `supervisor_config_clamps_max_live_to_one`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use super::SupervisorError;

/// Initial socket-probe interval, doubled on each miss.
///
/// 20 ms gives sub-50 ms detection on a fast bind without busy-waiting.
pub const DEFAULT_INITIAL_PROBE_INTERVAL: Duration = Duration::from_millis(20);

/// Ceiling on the exponential probe backoff.
pub const DEFAULT_MAX_PROBE_INTERVAL: Duration = Duration::from_millis(250);

/// Per-attempt timeout on the liveness connect.
///
/// Short on purpose: an unresponsive-but-bound socket must not stall the probe
/// loop, and a connect that does not settle inside it is
/// [`super::SocketVerdict::Inconclusive`] rather than a reason to kill anything.
pub const DEFAULT_CONNECT_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Const-evaluable `a > b` for [`Duration`].
///
/// Why: [`ServiceTimeouts::new`] must compare two durations inside a `const fn`,
/// and this pair of accessors has been `const` since long before this crate's
/// MSRV — unlike the whole-value comparison operators, which are not `const`.
const fn duration_gt(a: Duration, b: Duration) -> bool {
    a.as_secs() > b.as_secs() || (a.as_secs() == b.as_secs() && a.subsec_nanos() > b.subsec_nanos())
}

/// The timing budget of ONE supervised service.
///
/// Why: see the module docs. Every field here is a statement about the
/// supervised child, not about supervision in general.
///
/// What: `spawn_probe` is how long [`super::UdsServiceSupervisor::ensure_running`]
/// waits for a freshly-spawned child to bind and accept. `shutdown_flush` is the
/// child's OWN shutdown budget, declared by the service so the relationship
/// below is checkable. `sigterm_patience` is how long the supervisor waits after
/// SIGTERM before escalating to SIGKILL, and it must strictly exceed
/// `shutdown_flush` — the child still needs signal delivery, the flush itself,
/// socket cleanup and exit inside that window.
///
/// 🔴 **Sourcing rule for `shutdown_flush`, and what the assert cannot check.**
/// `shutdown_flush` MUST be the supervised binary's own flush constant,
/// imported — `trusty_bm25_daemon::SHUTDOWN_FLUSH_TIMEOUT`, not a literal `2 s`
/// that happens to match it today. [`ServiceTimeouts::new`] enforces the
/// RELATION (`sigterm_patience > shutdown_flush`) and nothing else: it has no
/// way to know what your child's real budget is, so a literal that understates
/// it compiles, passes the assert, and SIGKILLs mid-flush — the exact failure
/// this type exists to prevent. A hardcoded copy also stays equal to itself
/// while the daemon's real value drifts, so it can never detect the drift.
/// Import the constant, or add a test pinning your value against it the way
/// `sigterm_patience_exceeds_the_daemon_flush_budget` does. If your service's
/// flush budget is not a constant you can name, that is the thing to fix first.
///
/// `#[non_exhaustive]`: free while `trusty-common` sits unpublished at 0.30.0
/// against a published 0.28.1, and never free again. On a STRUCT the attribute
/// bars construction from outside the crate, including functional-update syntax
/// (E0639) — which is the intent: the constructors are the only way in, and
/// they are where the guard lives. Use [`ServiceTimeouts::new`] for a `const`
/// declaration (compile-time check) and [`ServiceTimeouts::try_new`] when the
/// values are computed at runtime.
///
/// Test: `service_timeouts_reject_patience_equal_to_the_flush`,
/// `service_timeouts_carry_probe_defaults_and_honour_overrides`,
/// `try_new_rejects_an_inverted_pair_without_panicking`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServiceTimeouts {
    /// Total budget waiting for a freshly-spawned child to accept a connection.
    pub spawn_probe: Duration,
    /// The child's own shutdown-flush budget, as declared by its service.
    pub shutdown_flush: Duration,
    /// SIGTERM-to-SIGKILL patience. Strictly greater than `shutdown_flush`.
    pub sigterm_patience: Duration,
    /// First probe interval after spawn.
    pub initial_probe_interval: Duration,
    /// Ceiling on the doubling probe interval.
    pub max_probe_interval: Duration,
    /// Per-attempt timeout on a liveness connect.
    pub connect_probe: Duration,
}

impl ServiceTimeouts {
    /// Declare a service's timing budget, checking the one relationship that
    /// loses data when it is wrong.
    ///
    /// Why: this is what replaces `bm25_supervisor.rs`'s
    /// `const _: () = assert!(SIGTERM_PATIENCE_SECS > …SHUTDOWN_FLUSH_TIMEOUT…)`.
    /// That assertion could not cross the abstraction — `trusty-common` cannot
    /// name any particular daemon's flush budget without depending on it — so
    /// the check moved to where both numbers are in scope: the service's own
    /// declaration. Because this is a `const fn`, a service that writes
    /// `const T: ServiceTimeouts = ServiceTimeouts::new(…)` gets the identical
    /// compile-time failure it had before, now bound to the value actually used
    /// rather than to a free-standing constant that a refactor could leave
    /// behind. A service that builds its timeouts at runtime instead gets a
    /// panic at construction — startup, before any child exists — which is the
    /// programmer-error case `expect` is reserved for, not a runtime condition
    /// a correct caller can reach.
    ///
    /// What: `sigterm_patience` must be strictly greater than `shutdown_flush`.
    /// At an equal budget the SIGKILL lands inside the very flush the child's
    /// shutdown handler exists to perform, and every write inside its open
    /// window is discarded with nothing left to recover from. Probe intervals
    /// default to [`DEFAULT_INITIAL_PROBE_INTERVAL`],
    /// [`DEFAULT_MAX_PROBE_INTERVAL`] and [`DEFAULT_CONNECT_PROBE_TIMEOUT`];
    /// override them with [`Self::with_probe_intervals`] /
    /// [`Self::with_connect_probe`].
    ///
    /// Test: `service_timeouts_reject_patience_equal_to_the_flush` (runtime
    /// half) and `trusty-memory`'s `BM25_TIMEOUTS` const item (compile-time
    /// half — flip its patience below the daemon's flush and the build fails).
    pub const fn new(
        spawn_probe: Duration,
        shutdown_flush: Duration,
        sigterm_patience: Duration,
    ) -> Self {
        assert!(
            duration_gt(sigterm_patience, shutdown_flush),
            "SIGTERM patience must strictly exceed the supervised child's own \
             shutdown-flush budget, or the SIGKILL lands mid-flush and discards \
             acked writes"
        );
        Self {
            spawn_probe,
            shutdown_flush,
            sigterm_patience,
            initial_probe_interval: DEFAULT_INITIAL_PROBE_INTERVAL,
            max_probe_interval: DEFAULT_MAX_PROBE_INTERVAL,
            connect_probe: DEFAULT_CONNECT_PROBE_TIMEOUT,
        }
    }

    /// [`Self::new`] for values that are not known at compile time.
    ///
    /// Why: `new` is a `const fn`, which is what makes the guard a build error
    /// at a `const` call site — but it also means the only thing it can do about
    /// a bad pair at runtime is panic. A service deriving its timeouts from
    /// config or an environment variable needs to report that as an error, not
    /// take the process down. `#[non_exhaustive]` leaves no other way to build
    /// the value, so without this the runtime case is unserviceable.
    /// What: identical to [`Self::new`] except the relation failure is returned
    /// as [`SupervisorError::InvalidTimeouts`]. The sourcing rule on the type
    /// still applies — this checks the relation, never whether `shutdown_flush`
    /// is your child's real budget.
    /// Test: `try_new_rejects_an_inverted_pair_without_panicking`,
    /// `try_new_matches_new_for_a_valid_pair`.
    pub fn try_new(
        spawn_probe: Duration,
        shutdown_flush: Duration,
        sigterm_patience: Duration,
    ) -> Result<Self, SupervisorError> {
        if !duration_gt(sigterm_patience, shutdown_flush) {
            return Err(SupervisorError::InvalidTimeouts {
                sigterm_patience,
                shutdown_flush,
            });
        }
        Ok(Self::new(spawn_probe, shutdown_flush, sigterm_patience))
    }

    /// Override the exponential-backoff probe intervals.
    pub const fn with_probe_intervals(self, initial: Duration, max: Duration) -> Self {
        Self {
            initial_probe_interval: initial,
            max_probe_interval: max,
            ..self
        }
    }

    /// Override the per-attempt liveness-connect timeout.
    pub const fn with_connect_probe(self, connect_probe: Duration) -> Self {
        Self {
            connect_probe,
            ..self
        }
    }
}

/// Everything a [`super::UdsServiceSupervisor`] needs that is fixed for its
/// lifetime.
///
/// Why: resolving the limits once at construction — rather than on each
/// `ensure_running` — means a mid-flight environment mutation cannot make the
/// cap wobble between two concurrent calls.
///
/// What: `service` is the label every log line carries. `max_live` caps
/// concurrently-live children, with least-recently-used reaping.
/// `rss_limit_mb` is a per-child ceiling compared against a real measurement;
/// `None` disables enforcement, which is a different instruction from a ceiling
/// of zero and so cannot share an encoding with it. `external_env` names an
/// environment variable that, when set to exactly `"1"`, suppresses spawning
/// entirely so an operator running the target under `tctl` (ADR-0011,
/// ADR-0034 §1) keeps ownership of its lifecycle.
///
/// Test: `supervisor_config_clamps_max_live_to_one`,
/// `external_env_only_honours_exactly_one`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SupervisorConfig {
    /// Label for log fields; typically the supervised binary's name.
    pub service: String,
    /// Cap on concurrently-live children. Clamped to at least 1.
    pub max_live: usize,
    /// Per-child RSS ceiling in MB. `None` disables enforcement.
    pub rss_limit_mb: Option<u64>,
    /// Timing budget of the supervised service.
    pub timeouts: ServiceTimeouts,
    /// Environment variable that opts spawn supervision out when set to `"1"`.
    pub external_env: Option<String>,
    /// Whether spawned children outlive this supervisor (#6350).
    pub detached: bool,
}

impl SupervisorConfig {
    /// Configure a supervisor for one service.
    ///
    /// `max_live` is clamped to at least 1: a cap of zero would reap every
    /// child the instant it spawned, which is a wedge rather than a limit.
    pub fn new(service: impl Into<String>, max_live: usize, timeouts: ServiceTimeouts) -> Self {
        Self {
            service: service.into(),
            max_live: max_live.max(1),
            rss_limit_mb: None,
            timeouts,
            external_env: None,
            detached: false,
        }
    }

    /// Set the per-child RSS ceiling in MB; `None` disables enforcement.
    pub fn with_rss_limit_mb(mut self, rss_limit_mb: Option<u64>) -> Self {
        self.rss_limit_mb = rss_limit_mb;
        self
    }

    /// Let spawned children outlive this supervisor (#6350).
    ///
    /// Why: the default answers "keep this child alive for as long as I need
    /// it", which is what `trusty-console` wants — it is resident, it owns the
    /// child, and `kill_on_drop` guarantees no orphan survives it. An on-demand
    /// service under ADR-0032 is the opposite arrangement: the client is a CLI
    /// that exits in seconds, the SERVICE decides when to end (its own idle
    /// window), and a second client is expected to reuse the same process. With
    /// the default, the first client's exit would SIGKILL a server a concurrent
    /// client was mid-request against.
    ///
    /// What: the child is spawned without `kill_on_drop`, is not entered into
    /// the supervisor's population map, and is reaped by a detached task that
    /// waits on it. Everything the map drives — the `max_live` cap, LRU
    /// eviction, RSS reaping, [`super::UdsServiceSupervisor::shutdown`] — has
    /// nothing to act on in this mode, deliberately: a child that owns its own
    /// lifetime is not this supervisor's to reclaim. What DOES still apply is
    /// the part an on-demand caller needs — the spawn gate, the
    /// already-serving check, socket verification, and the post-spawn probe.
    ///
    /// Test: `detached_children_are_not_retained_in_the_population`.
    pub fn with_detached(mut self, detached: bool) -> Self {
        self.detached = detached;
        self
    }

    /// Name the environment variable that suppresses spawning when set to `"1"`.
    pub fn with_external_env(mut self, var: impl Into<String>) -> Self {
        self.external_env = Some(var.into());
        self
    }

    /// Whether spawn supervision is currently opted out.
    ///
    /// Why: read per call rather than cached at construction, because an
    /// operator flipping the variable expects the next request to honour it,
    /// and because the supervisor's own tests set it after construction.
    /// What: true iff `external_env` is set and the variable holds exactly
    /// `"1"`. Any other value is treated as unset, matching how the
    /// `TRUSTY_BM25_DAEMON=1` client-side gate reads.
    /// Test: `external_env_only_honours_exactly_one`.
    pub fn external_mode_enabled(&self) -> bool {
        self.external_env
            .as_deref()
            .is_some_and(|key| std::env::var(key).as_deref() == Ok("1"))
    }
}

/// The command that starts one instance of a supervised service.
///
/// Why: resolved by a closure the supervisor calls only when it has decided to
/// spawn, so a service that is already running — or externally managed, or
/// adoptable from a socket someone else bound — never pays for locating its
/// binary, and a missing binary is not an error on any of those paths.
///
/// What: the program, its arguments, and directories to create first. `stdin`
/// and `stdout` are closed and `stderr` is inherited so the child's tracing
/// output reaches the parent's log stream; `kill_on_drop` is always set so an
/// unsupervised drop still reaps the child rather than leaking it.
///
/// Test: `spawn_spec_builder_accumulates_args_and_dirs`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SpawnSpec {
    /// Binary to execute.
    pub program: PathBuf,
    /// Arguments, in order.
    pub args: Vec<OsString>,
    /// Directories created (recursively) before the spawn.
    pub create_dirs: Vec<PathBuf>,
}

impl SpawnSpec {
    /// Start a spec for `program`.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            create_dirs: Vec::new(),
        }
    }

    /// Append one argument.
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Create `dir` (and its parents) before spawning.
    pub fn create_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.create_dirs.push(dir.into());
        self
    }
}
