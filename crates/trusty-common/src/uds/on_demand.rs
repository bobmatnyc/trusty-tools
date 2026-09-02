//! The one place a client starts `trusty-analyze` on demand (#6350, ADR-0032).
//!
//! Why here, rather than in `trusty-analyze`: three crates dial that socket —
//! `trusty-review`'s report adapter, `trusty-analyze`'s own `deep` subcommand,
//! and `tctl`'s boot stage — and only `trusty-analyze` has a Cargo edge on
//! itself. `trusty-installer` deliberately has none (see `probe_http`'s note on
//! why it duplicates method-name literals rather than depending on a daemon),
//! and `trusty-review` has none either. So the description of HOW to start the
//! service — binary name, arguments, socket path, spawn budget — has to live in
//! the crate all three already depend on, next to
//! [`crate::daemon_socket_path`], which is where they already agree about
//! WHERE it answers.
//!
//! What: [`OnDemandAnalyze`], a small owned handle wrapping a
//! [`UdsServiceSupervisor`] in [`SupervisorConfig::with_detached`] mode. One
//! call — [`OnDemandAnalyze::ensure_running`] — returns the socket path with
//! something answering on it, spawning `trusty-analyze serve` only when nothing
//! is.
//!
//! **Two callers, one server, at two levels.** Within a process, the handle's
//! spawn gate serialises concurrent callers and the second one finds the socket
//! already serving. Across processes there is no shared gate, so the arbiter is
//! `bind_singleton_hardened` in the child: it takes over only a socket the
//! kernel proves nobody is serving, so the loser of a two-process race exits on
//! its bind and the winner serves both clients.
//!
//! **This module never fails open.** Every failure — no binary on `PATH`, a
//! spawn the OS refused, a socket that never appeared inside the budget —
//! reaches the caller as a [`SupervisorError`]. A client that wants to degrade
//! rather than abort makes that choice itself, visibly; nothing here decides it
//! silently on the caller's behalf.
//!
//! Test: `on_demand_tests.rs` — `analyze_spawn_spec_runs_a_bare_serve`,
//! `analyze_timeouts_leave_room_for_the_flush`,
//! `idle_timeout_parses_its_three_meanings`,
//! `external_mode_returns_the_socket_without_spawning`,
//! `the_default_handle_uses_the_shared_socket_path`. The real spawn, the idle
//! exit and the two-concurrent-callers race are proven against the built binary
//! in `trusty-analyze`'s `tests/on_demand.rs`, since only that crate has one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::supervisor::{
    ServiceTimeouts, SpawnSpec, SupervisorConfig, SupervisorError, UdsServiceSupervisor,
};

/// Binary and member name of the analysis service.
pub const ANALYZE_SERVICE: &str = "trusty-analyze";

/// Environment variable that suppresses on-demand spawning.
///
/// Why: an operator running `trusty-analyze serve` in a terminal, or a
/// developer running one under a debugger, owns that process's lifecycle.
/// Setting this to exactly `1` makes every client dial whatever is there and
/// never start one of its own — the same opt-out `tctl`-managed services get
/// under ADR-0034 §1.
pub const ANALYZE_EXTERNAL_ENV: &str = "TRUSTY_ANALYZE_EXTERNAL";

/// Environment variable overriding the server's idle window, in seconds.
///
/// `0` disables idle exit, for an operator who wants a foreground server that
/// stays up. Read by the SERVER (`trusty-analyze serve`), not by clients — it is
/// declared here because [`analyze_idle_timeout`] is the single parser both a
/// client's documentation and the server's startup read.
pub const ANALYZE_IDLE_TIMEOUT_ENV: &str = "TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS";

/// Default idle window before an on-demand analyze server exits.
///
/// Ten minutes, chosen from the 5–15 minute band the #6350 ruling set. The two
/// costs it balances: a cold start re-opens the facts redb and the SCIP overlay
/// store and re-warms per-index chunk caches, so a window shorter than an
/// operator's edit-run loop makes every `trusty-review report --analyze` pay
/// that again; and a resident process that nobody has spoken to for ten minutes
/// is exactly the thing ADR-0032 retired the launchd unit to stop having.
pub const DEFAULT_ANALYZE_IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// The analyze server's own shutdown budget.
///
/// Why this exists rather than a literal at the [`ServiceTimeouts`] call site:
/// that type's sourcing rule says `shutdown_flush` must be the supervised
/// binary's real budget, and `trusty-common` cannot import `trusty-analyze` to
/// read it. So the constant lives here and `trusty-analyze`'s
/// `analyze_flush_budget_matches_the_supervisor_contract` pins its own value
/// against it — the equality is checked, rather than assumed to have stayed
/// true.
///
/// 🔴 **What this actually bounds, precisely (#6601 review).** It is the child's
/// half of the `sigterm_patience > shutdown_flush` relation, and that relation
/// governs the ONE path on which this supervisor signals an analyze child: the
/// spawn-probe timeout in `UdsServiceSupervisor::ensure_running`. A child that
/// BOUND is detached (see [`SupervisorConfig::with_detached`], #6350) — it is
/// never entered in the supervisor's population, and every `terminate_child`
/// call site reads only that population or the doomed queue — so no reap path
/// can reach a serving analyze server, and this number says nothing about how
/// long one lives after SIGTERM.
///
/// It is therefore NOT the serve loop's shutdown drain. `trusty-analyze`'s
/// `service::rpc::serve_options` drains on
/// [`crate::shutdown::plannable_grace`], because the only bounded terminator of
/// a SERVING analyze process is the OS at logout or shutdown — `trusty-analyze
/// stop` sends SIGTERM, polls 5 s and then merely reports. Setting the drain to
/// this budget instead gave up the #6595 guarantee three seconds into a
/// multi-minute `analyze.review` while averting no SIGKILL at all.
///
/// Three seconds is right for what it does bound. The server holds no write
/// buffer — `redb` commits inside each handler before it answers, so a SIGTERM
/// discards nothing that was acked — and what a spawn-failure kill must leave
/// room for is signal delivery, the socket unlink and exit. Three leaves 2 s of
/// [`ANALYZE_SIGTERM_PATIENCE`]'s 5 s for exactly that.
pub const ANALYZE_SHUTDOWN_FLUSH: Duration = Duration::from_secs(3);

/// How long to wait for a freshly-spawned analyze server to accept.
///
/// The server opens two redb files before it binds. On a warm page cache that is
/// milliseconds; on a cold one, or a machine under load, it is not, and a budget
/// that expires produces a `SpawnTimeout` the caller reports as a failure while
/// the server binds successfully a moment later.
const ANALYZE_SPAWN_PROBE: Duration = Duration::from_secs(20);

/// SIGTERM-to-SIGKILL patience. Must strictly exceed [`ANALYZE_SHUTDOWN_FLUSH`].
///
/// The 2 s margin over the flush budget is what the child spends after its drain
/// ends: unlinking the socket, dropping its redb stores, and exiting. Raising it
/// costs every reap — `enforce_limits` waits this out per victim — so it is
/// sized to that margin rather than to the process grace window.
const ANALYZE_SIGTERM_PATIENCE: Duration = Duration::from_secs(5);

/// The analyze service's timing budget.
///
/// `const`, so the `sigterm_patience > shutdown_flush` relation is a build
/// error rather than a runtime panic — see [`ServiceTimeouts::new`].
pub const ANALYZE_TIMEOUTS: ServiceTimeouts = ServiceTimeouts::new(
    ANALYZE_SPAWN_PROBE,
    ANALYZE_SHUTDOWN_FLUSH,
    ANALYZE_SIGTERM_PATIENCE,
);

/// Resolve the idle window a server should apply.
///
/// Why a parser rather than a bare `env::var`: the variable has three meanings —
/// unset is the default, `0` is "never exit", anything else is a second count —
/// and a caller that read it inline would have to re-derive all three. An
/// unparseable value is treated as unset rather than fatal: a typo in an
/// environment variable must not stop the service from starting.
///
/// What: `None` means never exit; `Some(d)` is the window.
/// Test: `idle_timeout_parses_its_three_meanings`.
pub fn analyze_idle_timeout(raw: Option<&str>) -> Option<Duration> {
    match raw.map(str::trim) {
        None | Some("") => Some(DEFAULT_ANALYZE_IDLE_TIMEOUT),
        Some(value) => match value.parse::<u64>() {
            Ok(0) => None,
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(_) => Some(DEFAULT_ANALYZE_IDLE_TIMEOUT),
        },
    }
}

/// Read [`ANALYZE_IDLE_TIMEOUT_ENV`] through [`analyze_idle_timeout`].
pub fn analyze_idle_timeout_from_env() -> Option<Duration> {
    analyze_idle_timeout(std::env::var(ANALYZE_IDLE_TIMEOUT_ENV).ok().as_deref())
}

/// A handle that starts `trusty-analyze` when nothing is serving its socket.
///
/// Why an owned handle rather than a free function: the in-process half of the
/// "two callers, one server" guarantee is the supervisor's spawn gate, and a
/// free function would build a fresh supervisor — and a fresh gate — per call,
/// letting two concurrent callers in one process each spawn a server. Sharing
/// one handle (behind an `Arc` where the caller is concurrent) is what closes
/// that. It is deliberately NOT a process-wide singleton: this workspace keeps
/// no global state, and a handle costs one small struct.
///
/// What: `ensure_running` is the whole surface. The socket path is resolved once
/// at construction through [`crate::daemon_socket_path`], the same call the
/// server itself binds, so there is nothing for the two to disagree about.
///
/// Test: `the_default_handle_uses_the_shared_socket_path`,
/// `external_mode_returns_the_socket_without_spawning`.
#[derive(Debug)]
pub struct OnDemandAnalyze {
    supervisor: UdsServiceSupervisor,
    socket: PathBuf,
}

impl OnDemandAnalyze {
    /// A handle for the socket every consumer dials.
    ///
    /// # Errors
    ///
    /// When the data directory cannot be resolved, which is what makes the
    /// socket path underivable.
    pub fn new() -> Result<Self, SupervisorError> {
        // `SpawnSpec` is the variant for "could not work out how or where to
        // run it", which an underivable socket path is: without it there is
        // nothing to pass the child as `--socket` and nothing for a client to
        // dial. `SocketPath` cannot carry this — its source is a
        // `UdsSecurityError`, and this failure is a data-directory one.
        let socket = crate::daemon_socket_path(ANALYZE_SERVICE).map_err(|source| {
            SupervisorError::SpawnSpec {
                service: ANALYZE_SERVICE.to_string(),
                key: ANALYZE_SERVICE.to_string(),
                source: source.into(),
            }
        })?;
        Ok(Self::at(socket))
    }

    /// A handle for an explicit socket path.
    ///
    /// Why public: a test needs a socket under its own tempdir rather than the
    /// developer's real data directory, and `trusty-analyze serve --socket`
    /// exists for exactly that.
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            // `max_live` is 1 and unused: a detached child never enters the
            // population map, so nothing is ever counted against the cap.
            supervisor: UdsServiceSupervisor::new(
                SupervisorConfig::new(ANALYZE_SERVICE, 1, ANALYZE_TIMEOUTS)
                    .with_external_env(ANALYZE_EXTERNAL_ENV)
                    .with_detached(true),
            ),
            socket: socket.into(),
        }
    }

    /// The socket this handle guarantees is being served.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Return the socket path with a server answering on it.
    ///
    /// What: delegates to [`UdsServiceSupervisor::ensure_running`], which
    /// returns immediately when the socket already answers and otherwise spawns
    /// `trusty-analyze serve` and waits for the bind. The binary is located
    /// through [`crate::bin_resolve::resolve_binary`] — the workspace's single
    /// answer to "find this executable", which also searches the well-known bin
    /// directories a launchd-minimal `PATH` omits.
    ///
    /// # Errors
    ///
    /// [`SupervisorError::SpawnSpec`] when `trusty-analyze` is not installed,
    /// [`SupervisorError::Spawn`] when the OS refused the spawn,
    /// [`SupervisorError::SpawnTimeout`] when no socket appeared inside
    /// [`ANALYZE_SPAWN_PROBE`], and [`SupervisorError::UntrustedSocket`] when
    /// something is serving the path but does not pass the ownership check.
    /// None of these is degraded into a success — see the module docs.
    pub async fn ensure_running(&self) -> Result<PathBuf, SupervisorError> {
        self.supervisor
            .ensure_running(ANALYZE_SERVICE, &self.socket, || {
                Ok(analyze_spawn_spec(&self.socket)?)
            })
            .await
    }
}

/// The command that starts one analyze server on `socket`.
///
/// Why the socket is passed explicitly rather than left to the child's own
/// derivation: `OnDemandAnalyze::at` exists so a test can use a tempdir path,
/// and a child that derived the default would bind somewhere the parent never
/// probes — a spawn that "succeeds" against a socket nobody dials.
///
/// # Errors
///
/// When `trusty-analyze` is not on `PATH` or in a well-known bin directory.
///
/// Test: `analyze_spawn_spec_runs_a_bare_serve`.
pub fn analyze_spawn_spec(socket: &Path) -> Result<SpawnSpec, MissingBinary> {
    let program = crate::bin_resolve::resolve_binary(ANALYZE_SERVICE).ok_or(MissingBinary {
        name: ANALYZE_SERVICE,
    })?;
    let mut spec = SpawnSpec::new(program)
        .arg("serve")
        .arg("--socket")
        .arg(socket);
    if let Some(dir) = socket.parent() {
        spec = spec.create_dir(dir);
    }
    Ok(spec)
}

/// `trusty-analyze` is not installed anywhere this process can see.
///
/// Why its own type rather than a bare string: this is the failure an operator
/// fixes with one action (install the binary), and it reaches them through
/// [`SupervisorError::SpawnSpec`]'s source chain, where a `&'static str` would
/// have arrived as an unattributed sentence.
#[derive(Debug, thiserror::Error)]
#[error(
    "{name} is not installed — no such executable on PATH or in the standard \
     bin directories. Install it with `tctl install {name}`."
)]
pub struct MissingBinary {
    /// The binary that could not be located.
    pub name: &'static str,
}

#[cfg(test)]
#[path = "on_demand_tests.rs"]
mod tests;
