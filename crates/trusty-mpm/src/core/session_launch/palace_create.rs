//! Create the project's memory palace at session launch (#11823).
//!
//! Why: a fresh clone's first session had no palace, so startup memories came
//! back empty and every write failed with `PalaceNotFound` until someone
//! created the palace by hand. Both launch paths that run the palace-alias step
//! now ask trusty-memory to create the resolved palace through its normal
//! create path.
//! What: [`ensure_launch_palace`] names the palace with the env-free resolver
//! (the name the daemon checks against `cwd`), probes the daemon, sends
//! `palace_create` with `{name, cwd}` and never `force`, and logs one line per
//! outcome. It never fails the launch.
//! Test: `palace_create_tests.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use trusty_common::memory_rpc::{
    MemoryRpcError, TRUSTY_MEMORY_SOCKET_ENV, call_memory_tool_at_with_timeout,
    memory_daemon_is_serving, resolve_memory_dial_socket,
};
use trusty_common::palace_resolve::{
    PalaceCreatePlanError, PalaceResolveError, resolve_palace_for_project,
};

/// The daemon method that creates a palace (trusty-memory `palace_create`).
pub(crate) const CREATE_METHOD: &str = "palace_create";

/// How long the launch waits to learn whether anything serves the socket.
const SERVING_PROBE: Duration = Duration::from_millis(500);

/// The whole create's budget: handshake, call and any palace-opening wait.
///
/// Why 3 s: a healthy local daemon answers a create in milliseconds, and a
/// first create (palace open plus KG bootstrap) well under a second; 3 s
/// covers a loaded host while capping what a hung daemon adds to a launch.
pub(crate) const CREATE_BUDGET: Duration = Duration::from_secs(3);

/// A second launch step for the same project inside this window is skipped.
///
/// Why: a daemon spawn runs both launch paths, so without this a failure would
/// warn twice for one launch.
const REPEAT_WINDOW: Duration = Duration::from_secs(30);

/// What the launch-time create did.
///
/// Test: one test per variant in `palace_create_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaunchPalace {
    /// Nothing was asked of the daemon, for the stated reason.
    Skipped(&'static str),
    /// The daemon created the palace.
    Created { palace: String },
    /// The palace, or a live alias, already existed.
    Exists { palace: String },
    /// The daemon linked the palace to an existing folder-named palace.
    Aliased { palace: String, target: String },
    /// The project's pin cannot be trusted; nothing was sent.
    PinBlocked { reason: String },
    /// Nothing serves the trusty-memory socket.
    NotServing { palace: String, socket: PathBuf },
    /// The daemon could not be reached, or did not answer within the budget.
    Unreachable { palace: String, reason: String },
    /// The daemon answered with an error.
    Refused {
        palace: String,
        code: i64,
        message: String,
    },
    /// The daemon answered with a reply this client does not read.
    BadReply { palace: String, reply: String },
}

impl LaunchPalace {
    /// The warning this outcome owes the operator, if any.
    ///
    /// What: names the palace (or the pin path) and the reason; unreachable
    /// and refused read differently. `None` for success and skips.
    /// Test: `a_refusal_warns_naming_the_palace_and_the_reason`,
    /// `an_unreachable_daemon_warns_and_returns_within_the_budget`.
    pub(crate) fn warning(&self) -> Option<String> {
        match self {
            Self::Skipped(_) | Self::Created { .. } | Self::Exists { .. } => None,
            Self::Aliased { .. } => None,
            Self::PinBlocked { reason } => Some(format!(
                "project palace not created: {reason}; fix or remove the pin \
                 (no fallback name is used)"
            )),
            Self::NotServing { palace, socket } => Some(format!(
                "palace `{palace}` not created: trusty-memory is not running \
                 (nothing serves {})",
                socket.display()
            )),
            Self::Unreachable { palace, reason } => Some(format!(
                "palace `{palace}` not created: trusty-memory could not be reached: {reason}"
            )),
            Self::Refused {
                palace,
                code,
                message,
            } => Some(format!(
                "palace `{palace}` not created: trusty-memory refused the create: \
                 {message} ({code})"
            )),
            Self::BadReply { palace, reply } => Some(format!(
                "palace `{palace}`: trusty-memory answered palace_create with an \
                 unrecognised reply: {reply}"
            )),
        }
    }

    /// Log the outcome: one warning on failure, info on a change, else debug.
    fn log(&self) {
        if let Some(warning) = self.warning() {
            tracing::warn!("{warning}");
            return;
        }
        match self {
            Self::Created { palace } => {
                tracing::info!(palace = %palace, "created the project palace")
            }
            Self::Aliased { palace, target } => tracing::info!(
                palace = %palace,
                target = %target,
                "linked the project palace to the existing folder-named palace"
            ),
            Self::Exists { palace } => tracing::debug!(palace = %palace, "project palace exists"),
            other => tracing::debug!("project palace create skipped: {other:?}"),
        }
    }
}

/// The launch's palace steps: the #1939 alias, then the #11823 create.
///
/// Why (#4181, #11823): the standalone `prepare_session_inner` and the daemon
/// spawn/resume choke point `ensure_managed_config_dir_with_root` both need
/// the same two steps in the same order; one call keeps them from drifting.
/// The alias step (`owner-repo` → an existing bare-repo palace) runs first so
/// the create then answers `exists` for an aliased name.
/// What: [`super::palace_alias::maybe_register_palace_alias`], then
/// [`ensure_launch_palace`] when `create` (the manifest's trusty-memory
/// toggle). Never errors.
/// Test: `standalone_launch_sends_the_create`,
/// `ensure_managed_config_dir_sends_the_palace_create`,
/// `ensure_managed_config_dir_heals_a_bare_repo_palace_alias`.
pub(crate) fn launch_palace_steps(
    project_dir: &Path,
    git_remote: Option<&str>,
    registry_dir: Option<&Path>,
    create: bool,
) {
    super::palace_alias::maybe_register_palace_alias(project_dir, git_remote, registry_dir);
    if create {
        ensure_launch_palace(project_dir);
    }
}

/// Create the project's palace if it is missing, at session launch (#11823).
///
/// Why: see the module docs. The alias step before it handles the older
/// `owner-repo` → bare-repo case; the daemon's create handles the folder-name
/// case.
/// What: skips (debug) when `TRUSTY_MEMORY_PALACE` is set — the operator chose
/// the session palace, and the daemon would refuse it as a name mismatch — or
/// when `project_dir` is no project. A pin that cannot be trusted is
/// [`LaunchPalace::PinBlocked`] with no daemon call. Under a test harness it
/// runs only against an explicit `TRUSTY_MEMORY_SOCKET`. The same project is
/// asked at most once per [`REPEAT_WINDOW`]. Never errors; logs its outcome.
/// Test: `standalone_launch_sends_the_create`,
/// `ensure_managed_config_dir_sends_the_palace_create` (managed_config_tests),
/// `a_malformed_pin_blocks_the_create_and_sends_nothing`.
pub(crate) fn ensure_launch_palace(project_dir: &Path) -> LaunchPalace {
    let outcome = launch_palace_outcome(project_dir);
    outcome.log();
    outcome
}

fn launch_palace_outcome(project_dir: &Path) -> LaunchPalace {
    if trusty_common::palace_override_from_env().is_some() {
        return LaunchPalace::Skipped("TRUSTY_MEMORY_PALACE names the session palace");
    }
    let palace = match resolve_palace_for_project(project_dir) {
        Ok(resolution) => resolution.id,
        Err(
            PalaceCreatePlanError::NoProjectDir { .. }
            | PalaceCreatePlanError::Resolve(PalaceResolveError::NoIdentity { .. }),
        ) => return LaunchPalace::Skipped("not inside a project directory"),
        // #11823: a malformed pin blocks the create; no fallback, no force.
        Err(e) => {
            return LaunchPalace::PinBlocked {
                reason: e.to_string(),
            };
        }
    };
    // Unit tests never reach the operator's daemon.
    if trusty_common::running_under_test_harness()
        && std::env::var_os(TRUSTY_MEMORY_SOCKET_ENV).is_none()
    {
        return LaunchPalace::Skipped("test harness without a named trusty-memory socket");
    }
    if !first_ask_in_window(project_dir, &palace, Instant::now()) {
        return LaunchPalace::Skipped("already asked for this project in this launch");
    }
    match resolve_memory_dial_socket() {
        Ok(socket) => create_launch_palace_at(&socket, project_dir, &palace, CREATE_BUDGET),
        Err(e) => LaunchPalace::Unreachable {
            palace,
            reason: format!("{e:#}"),
        },
    }
}

/// Record an ask for `(project_dir, palace)`; `false` when one is in window.
fn first_ask_in_window(project_dir: &Path, palace: &str, now: Instant) -> bool {
    static ASKED: Mutex<Option<HashMap<(PathBuf, String), Instant>>> = Mutex::new(None);
    let mut guard = ASKED.lock().unwrap_or_else(|e| e.into_inner());
    let asked = guard.get_or_insert_with(HashMap::new);
    asked.retain(|_, at| now.duration_since(*at) < REPEAT_WINDOW);
    let key = (project_dir.to_path_buf(), palace.to_string());
    if asked.contains_key(&key) {
        return false;
    }
    asked.insert(key, now);
    true
}

/// Send the create to the daemon at `socket`, bounded by `budget`.
///
/// What: runs on its own thread and runtime, so a caller inside a tokio
/// runtime can block on it. Probes the socket first, then sends
/// `palace_create` with `{name, cwd}`; the whole call is bounded by `budget`.
/// Test: `a_missing_palace_is_created_with_cwd_and_no_force`,
/// `an_existing_palace_is_not_a_warning`,
/// `a_hung_daemon_is_unreachable_within_the_budget`.
pub(crate) fn create_launch_palace_at(
    socket: &Path,
    project_dir: &Path,
    palace: &str,
    budget: Duration,
) -> LaunchPalace {
    let Some(cwd) = project_dir.to_str().map(str::to_string) else {
        return LaunchPalace::Skipped("project directory is not UTF-8");
    };
    let socket = socket.to_path_buf();
    let name = palace.to_string();
    let joined = std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(create_at(&socket, &cwd, &name, budget)),
            Err(e) => LaunchPalace::Unreachable {
                palace: name,
                reason: format!("could not build a runtime for the call: {e}"),
            },
        }
    })
    .join();
    joined.unwrap_or_else(|_| LaunchPalace::Unreachable {
        palace: palace.to_string(),
        reason: "the create thread panicked".to_string(),
    })
}

async fn create_at(socket: &Path, cwd: &str, palace: &str, budget: Duration) -> LaunchPalace {
    let palace = palace.to_string();
    if !memory_daemon_is_serving(socket, SERVING_PROBE.min(budget)).await {
        return LaunchPalace::NotServing {
            palace,
            socket: socket.to_path_buf(),
        };
    }
    // #11823: `cwd` is required by the daemon; `force` is never sent.
    let params = json!({ "name": palace, "cwd": cwd });
    let call = call_memory_tool_at_with_timeout(socket, CREATE_METHOD, params, budget);
    match tokio::time::timeout(budget, call).await {
        Err(_) => LaunchPalace::Unreachable {
            palace,
            reason: format!("no answer within {} ms", budget.as_millis()),
        },
        Ok(Ok(reply)) => from_reply(palace, &reply),
        Ok(Err(e)) => match e.downcast_ref::<MemoryRpcError>() {
            Some(refusal) => LaunchPalace::Refused {
                palace,
                code: refusal.code,
                message: refusal.message.clone(),
            },
            None => LaunchPalace::Unreachable {
                palace,
                reason: format!("{e:#}"),
            },
        },
    }
}

/// Read `{status, alias_of?}` from a `palace_create` reply.
fn from_reply(palace: String, reply: &Value) -> LaunchPalace {
    let alias_of = reply.get("alias_of").and_then(Value::as_str);
    match (reply.get("status").and_then(Value::as_str), alias_of) {
        (Some("created"), _) => LaunchPalace::Created { palace },
        (Some("exists"), _) => LaunchPalace::Exists { palace },
        (Some("aliased"), Some(target)) => LaunchPalace::Aliased {
            palace,
            target: target.to_string(),
        },
        _ => LaunchPalace::BadReply {
            palace,
            reply: reply.to_string(),
        },
    }
}

#[cfg(test)]
#[path = "palace_create_tests.rs"]
pub(crate) mod tests;
