//! The launchd-aware branch of the daemon guard (#10413).
//!
//! Why: at boot, a CLI command that finds the socket down spawned its own
//! detached `start --foreground`. launchd's `com.trusty.search` job was
//! starting in the same seconds, so the two raced for the daemon lock and the
//! loser exited (#10358). The guard must hand the start to launchd when launchd
//! owns the socket, and spawn only when nothing supervises it.
//! What: [`current_state`] reads who owns the socket; [`decide`] maps that to a
//! [`Plan`]; [`ensure_with`] runs the plan with the kickstart, the owner
//! re-check and the spawn injected. [`is_production_socket`] keeps any isolated
//! daemon (a test, `TRUSTY_DATA_DIR`) on the old spawn path.
//! Test: `daemon_guard_launchd_tests` (decide truth table, isolation gate,
//! orchestration arms); `tests/auto_start_socket_9214.rs` guards the spawn.

use std::ffi::OsStr;
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use colored::Colorize;
use trusty_common::launchd_claim::SocketOwner;
use trusty_search::service::daemon_client::DaemonClient;

use super::daemon_guard::wait_for_socket;

/// The search unit's label. Not `service::LAUNCHD_LABEL`, which is macOS-only.
const LAUNCHD_LABEL: &str = trusty_common::launchd_labels::SEARCH;

/// Bound on each blocking launchd read and on the `kickstart` command.
pub(crate) const LAUNCHD_CALL_BOUND: Duration = Duration::from_secs(5);

/// What the guard knows about launchd's claim on the socket.
///
/// Why: [`SocketOwner::Launchd`] is true for a loaded unit AND for an installed
/// plist whose unit is booted out; the plan differs, so both signals travel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchdState {
    pub(crate) owner: SocketOwner,
    /// `launchctl print` answered for the unit.
    pub(crate) loaded: bool,
    /// The pid launchd reports for the running job, if any.
    pub(crate) pid: Option<u32>,
}

impl LaunchdState {
    /// Nothing supervises the socket: spawn as before.
    pub(crate) fn on_demand() -> Self {
        Self {
            owner: SocketOwner::OnDemand,
            loaded: false,
            pid: None,
        }
    }
}

/// What the guard does for a [`LaunchdState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Spawn `start --foreground` ourselves.
    Spawn,
    /// Loaded but idle: `launchctl kickstart` it, then wait.
    Kickstart,
    /// The job is running: wait for it to bind.
    Wait,
    /// The plist exists but the unit is not loaded: wait, then error.
    WaitUnloaded,
}

/// Choose the plan for the socket's owner.
///
/// Why: pure and separate from the `launchctl` reads, so the truth table is
/// testable without a registered unit.
/// What: `OnDemand` spawns; a loaded unit with no pid is kickstarted; a loaded
/// unit with a pid is waited on; an unloaded plist waits then errors. Nothing
/// under a launchd verdict maps to `Spawn`.
/// Test: `decide_truth_table`.
pub(crate) fn decide(owner: &SocketOwner, loaded: bool, pid: Option<u32>) -> Plan {
    match (owner, loaded, pid) {
        (SocketOwner::OnDemand, ..) => Plan::Spawn,
        (_, false, _) => Plan::WaitUnloaded,
        (_, true, None) => Plan::Kickstart,
        (_, true, Some(_)) => Plan::Wait,
    }
}

/// The process environment that isolates a daemon from the production socket.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct IsolationEnv<'a> {
    pub(crate) search_socket: Option<&'a OsStr>,
    pub(crate) search_data_dir: Option<&'a OsStr>,
    pub(crate) data_dir: Option<&'a OsStr>,
    pub(crate) data_dir_override: Option<&'a OsStr>,
}

/// Whether `client_socket` is the canonical production socket.
///
/// Why: conservative on purpose — a wrong `true` would keep an isolated daemon
/// (a test, a second instance) from spawning. Any doubt answers `false`, which
/// is the old behaviour.
/// What: true only when `TRUSTY_SEARCH_SOCKET` and `TRUSTY_DATA_DIR_OVERRIDE`
/// are unset, `TRUSTY_SEARCH_DATA_DIR` and `TRUSTY_DATA_DIR` are unset or empty,
/// and `client_socket` equals `canonical`.
/// Test: `production_socket_gate_cases`.
pub(crate) fn is_production_socket(
    env: &IsolationEnv<'_>,
    client_socket: &Path,
    canonical: Option<&Path>,
) -> bool {
    let unset = |v: Option<&OsStr>| v.is_none();
    let unset_or_empty = |v: Option<&OsStr>| v.is_none_or(OsStr::is_empty);
    unset(env.search_socket)
        && unset(env.data_dir_override)
        && unset_or_empty(env.search_data_dir)
        && unset_or_empty(env.data_dir)
        && canonical == Some(client_socket)
}

/// A stderr note when `--device` cannot reach a launchd-managed daemon.
///
/// Why: under launchd the unit starts the daemon, so the CLI's `--device cpu`
/// (the #24 fallback) is not passed to it; staying silent would hide that.
/// What: `Some(note)` when `device` is set and the plan does not spawn.
/// Test: `device_note_only_for_a_non_spawn_plan`.
pub(crate) fn device_note(device: Option<&str>, plan: Plan) -> Option<String> {
    let dev = device?;
    (plan != Plan::Spawn).then(|| {
        format!(
            "note: --device {dev} is ignored; the launchd-managed daemon starts with the unit's \
             own settings. Set TRUSTY_DEVICE={dev} in the plist's EnvironmentVariables or in \
             daemon.env, then restart the unit"
        )
    })
}

/// `launchctl kickstart` arguments for the search unit, without `-k`.
///
/// Why: `-k` kills a running job first; the guard only starts an idle one.
/// Test: `kickstart_argv_has_no_kill_flag`.
pub(crate) fn kickstart_argv(uid: u32, label: &str) -> Vec<String> {
    vec!["kickstart".to_owned(), format!("gui/{uid}/{label}")]
}

/// Run `program args` with a wall-clock bound; non-zero exit is an error.
///
/// Why: the kickstart must not hang a CLI command; `program` is a parameter so
/// tests run a stub and never touch the real launchd.
/// What: spawns with `kill_on_drop`, so a timeout reaps the child.
/// Test: `run_bounded_logs_argv_and_succeeds`, `run_bounded_reports_nonzero_exit`,
/// `run_bounded_times_out`.
pub(crate) async fn run_bounded(program: &Path, args: &[String], bound: Duration) -> Result<()> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(bound, cmd.output())
        .await
        .map_err(|_| {
            anyhow!(
                "{} {} did not finish within {}s",
                program.display(),
                args.join(" "),
                bound.as_secs()
            )
        })?
        .map_err(|e| anyhow!("run {}: {e}", program.display()))?;
    if out.status.success() {
        return Ok(());
    }
    Err(anyhow!(
        "{} {} exited {}: {}",
        program.display(),
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// The real user id; `trusty_common::launchd` exists on macOS only.
fn current_uid() -> u32 {
    #[cfg(target_os = "macos")]
    {
        trusty_common::launchd::current_uid()
    }
    #[cfg(not(target_os = "macos"))]
    {
        // SAFETY: getuid(2) has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }
}

/// The real `launchctl kickstart gui/<uid>/com.trusty.search`.
///
/// Test: side-effecting; argv by `kickstart_argv_has_no_kill_flag`, the runner
/// by `run_bounded_*`. Never called from a test.
pub(crate) async fn real_kickstart() -> Result<()> {
    let uid = current_uid();
    // Off macOS no state read ever yields a launchd verdict, so this is unreached.
    run_bounded(
        Path::new("launchctl"),
        &kickstart_argv(uid, LAUNCHD_LABEL),
        LAUNCHD_CALL_BOUND,
    )
    .await
}

/// Read launchd's claim on the search socket, bounded.
///
/// Why: the guard sits in front of every CLI command; a wedged `launchctl`
/// must not hang it.
/// What: [`is_production_socket`] first (not production: `OnDemand`, no
/// launchctl call), then the blocking reads in `spawn_blocking` under
/// [`LAUNCHD_CALL_BOUND`]. A timeout or a panicked read counts as `OnDemand`.
/// Test: the gate by `production_socket_gate_cases`; the reads are
/// side-effecting and never run in a test.
pub(crate) async fn current_state(client_socket: &Path) -> LaunchdState {
    let canonical = crate::service::socket::socket_path().ok();
    let (a, b, c, d) = (
        std::env::var_os("TRUSTY_SEARCH_SOCKET"),
        std::env::var_os("TRUSTY_SEARCH_DATA_DIR"),
        std::env::var_os("TRUSTY_DATA_DIR"),
        std::env::var_os("TRUSTY_DATA_DIR_OVERRIDE"),
    );
    let env = IsolationEnv {
        search_socket: a.as_deref(),
        search_data_dir: b.as_deref(),
        data_dir: c.as_deref(),
        data_dir_override: d.as_deref(),
    };
    if !is_production_socket(&env, client_socket, canonical.as_deref()) {
        return LaunchdState::on_demand();
    }
    state_with(read_state_blocking, LAUNCHD_CALL_BOUND, plist_present()).await
}

/// Whether the search unit's plist is installed (a cheap stat, off the blocking pool).
fn plist_present() -> bool {
    #[cfg(target_os = "macos")]
    {
        trusty_common::launchd::plist_path_for_label(LAUNCHD_LABEL).is_some_and(|p| p.exists())
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// The state to assume when the launchd read did not answer.
///
/// Why: a plist survives a bootout, so "could not read" with a plist present
/// must not read as "unsupervised" — that would spawn into the boot race.
/// What: plist present: a launchd verdict, not loaded (the guard waits, never
/// spawns). No plist: `OnDemand`.
/// Test: `unanswered_read_with_a_plist_waits_instead_of_spawning`.
pub(crate) fn unanswered_state(plist_present: bool) -> LaunchdState {
    if plist_present {
        LaunchdState {
            owner: SocketOwner::Launchd {
                label: LAUNCHD_LABEL.to_owned(),
            },
            loaded: false,
            pid: None,
        }
    } else {
        LaunchdState::on_demand()
    }
}

/// Run `read` on the blocking pool under `bound`; a timeout or panic yields
/// [`unanswered_state`].
///
/// Test: `a_timed_out_read_with_a_plist_is_not_loaded_launchd`,
/// `a_panicked_read_with_a_plist_is_not_loaded_launchd`,
/// `an_unanswered_read_without_a_plist_is_on_demand`.
pub(crate) async fn state_with<R>(read: R, bound: Duration, plist_present: bool) -> LaunchdState
where
    R: FnOnce() -> LaunchdState + Send + 'static,
{
    match tokio::time::timeout(bound, tokio::task::spawn_blocking(read)).await {
        Ok(Ok(state)) => state,
        Ok(Err(e)) => {
            eprintln!("launchd state read failed ({e}); assuming plist presence decides");
            unanswered_state(plist_present)
        }
        Err(_) => {
            eprintln!("launchd state read timed out; assuming plist presence decides");
            unanswered_state(plist_present)
        }
    }
}

/// The blocking half of [`current_state`]: owner, then the running pid.
fn read_state_blocking() -> LaunchdState {
    #[cfg(target_os = "macos")]
    {
        let owner = trusty_common::launchd_claim::launchd_socket_owner(LAUNCHD_LABEL, true);
        if !owner.is_launchd() {
            return LaunchdState::on_demand();
        }
        let printed = super::service::label_only_config().launchctl_print();
        LaunchdState {
            owner,
            loaded: printed.is_some(),
            pid: printed
                .as_deref()
                .and_then(trusty_common::launchd_grace::parse_launchctl_pid),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        LaunchdState::on_demand()
    }
}

/// Run the plan for `state`, with every side effect injected.
///
/// Why: the branch that must be unreachable under a launchd verdict is the
/// spawn; proving that means handing in a spawn that panics.
/// What: see [`decide`]. A failed kickstart re-reads the owner once through
/// `recheck`: `OnDemand` now spawns, anything else waits out `budget` and
/// errors. The spawn is never reached while the unit exists.
///
/// # Errors
///
/// The spawn fails, nothing answers within `budget`, or a launchd-owned socket
/// stays unserved (the error names the unit and the `launchctl print` command).
///
/// Test: `ensure_with_*` in `daemon_guard_launchd_tests`.
pub(crate) async fn ensure_with<K, KF, R, RF, S>(
    client: &DaemonClient,
    budget: Duration,
    state: LaunchdState,
    kickstart: K,
    recheck: R,
    spawn: S,
) -> Result<()>
where
    K: FnOnce() -> KF,
    KF: Future<Output = Result<()>>,
    R: FnOnce() -> RF,
    RF: Future<Output = LaunchdState>,
    S: FnOnce() -> Result<u32>,
{
    let label = state.owner.label().map(str::to_owned);
    let plan = decide(&state.owner, state.loaded, state.pid);
    match (plan, label) {
        (Plan::Wait | Plan::WaitUnloaded, Some(label)) => {
            wait_for_unit(client, budget, &label, plan == Plan::Wait).await
        }
        (Plan::Kickstart, Some(label)) => {
            eprintln!("{} Starting launchd unit {label}…", "◉".cyan());
            if let Err(e) = kickstart().await {
                eprintln!("launchctl kickstart failed: {e}");
                // Re-check once: only a unit that is gone lets us spawn.
                let again = recheck().await;
                if decide(&again.owner, again.loaded, again.pid) == Plan::Spawn {
                    return spawn_and_wait(client, budget, spawn).await;
                }
            }
            wait_for_unit(client, budget, &label, true).await
        }
        // `Spawn`, or a plan with no label (unreachable: only a launchd owner has one).
        _ => spawn_and_wait(client, budget, spawn).await,
    }
}

/// Spawn the daemon ourselves, then wait for the socket.
async fn spawn_and_wait(
    client: &DaemonClient,
    budget: Duration,
    spawn: impl FnOnce() -> Result<u32>,
) -> Result<()> {
    spawn()?;
    wait_for_socket(client, budget).await
}

/// Wait for the unit to serve the socket; the error names the unit.
///
/// What: `loaded` picks the error text. An unloaded unit says so and names the
/// command that loads it.
/// Test: `ensure_with_errors_for_an_unloaded_unit_without_spawning`,
/// `ensure_with_never_spawns_when_kickstart_fails_and_unit_remains`.
async fn wait_for_unit(
    client: &DaemonClient,
    budget: Duration,
    label: &str,
    loaded: bool,
) -> Result<()> {
    eprintln!(
        "{} Waiting for launchd unit {label} to serve trusty-search…",
        "◉".cyan()
    );
    wait_for_socket(client, budget).await.map_err(|_| {
        let socket = client.socket().display();
        let secs = budget.as_secs();
        if loaded {
            anyhow!(
                "launchd unit {label} owns {socket} but nothing answered within {secs}s. Not \
                 spawning a daemon onto that path — it would race the unit. Check \
                 `launchctl print gui/$(id -u)/{label}` and the daemon's launchd log"
            )
        } else {
            anyhow!(
                "launchd unit {label} has an installed plist but is not loaded, so nothing \
                 answered {socket} within {secs}s. Not spawning a daemon onto that path. Load \
                 it with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{label}.plist` \
                 or `trusty-search service install` (inspect: `launchctl print gui/$(id -u)/{label}`)"
            )
        }
    })
}
