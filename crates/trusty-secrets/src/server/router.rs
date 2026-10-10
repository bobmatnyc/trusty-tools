//! The router, the bind, and the serve loop of the on-demand socket.
//!
//! Why: owner rulings 28 and 31 — the socket spawns on first call, exits when
//! idle, and is no daemon. Ruling 32 — the bind, the peer check and the idle
//! loop are trusty-common's, not hand-rolled here.
//! What: [`State`] (settings, index, backend factory), [`build_router`]
//! (each `secrets.*` method on the blocking pool under its deadline and an
//! admission cap, errors folded to fixed text), and [`serve`]: `prepare_socket_dir` (0700) →
//! `bind_singleton_hardened` (0600, refuses a live owner) →
//! `serve_until_idle` (every connection uid-checked by `handle_connection`
//! before a byte is read) → unlink the socket → drop the listener.
//! Test: `server_tests.rs` — round trips, `server_exits_when_idle_and_removes_its_socket`,
//! `server_second_instance_is_refused_and_the_first_keeps_serving`,
//! `server_bind_failure_is_reported_and_the_occupant_kept`.

use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Semaphore;
use trusty_common::uds::server::{
    IdleTracker, RpcRouter, RpcServeOptions, ServeExit as UdsServeExit, serve_until_idle,
};
use trusty_common::uds::{bind_singleton_hardened, prepare_socket_dir};

use super::audit::AuditSink;
use super::deadline::{BODY_GRACE, request_deadline};
use super::doctor;
use super::errors::ErrorKind;
use super::methods::{self, MethodFn};
use super::settings::ServerSettings;
use crate::api::methods::method;
use crate::api::{BackendId, SecretValue, SecretsError};
use crate::store::config::{MACHINE_CONFIG_SUBPATH, MachineSecretsConfig, load_machine_at};
use crate::store::{
    KEYCHAIN_COMPILED, NamesIndex, SecretBackend, cli_backends, open_backend, open_backend_at_in,
    platform,
};

/// Maps a configured backend id to an implementation.
///
/// Why: production opens the real Keychain through S1's `open_backend`;
/// tests inject S1's `MemoryBackend` so no test touches the OS keychain.
pub type BackendFactory =
    Arc<dyn Fn(&BackendId) -> Result<Arc<dyn SecretBackend>, SecretsError> + Send + Sync>;

/// S1's [`open_backend`]: CLI backends read the machine config under
/// `$HOME` and get no token overlay. The binary uses [`backends_for`].
pub fn default_backends() -> BackendFactory {
    Arc::new(open_backend)
}

/// The production factory for a server with `settings` (#7519).
///
/// Why: a CLI backend must write its template files where the startup
/// sweep looks and get the service-account token the binary took out of its
/// own environment. Ruling 74: whether it opens at all, and which `op` runs,
/// is the account's own machine config's to say, never a file the spawner
/// chose through `--machine-config` or `$HOME`. #7524 P2-M2: nor the
/// spawner's `PATH`; `op` comes from the pin or the fixed system directories.
/// What: `backends_with` on `account_machine_config`, the file #7524
/// reads `file` consent from, and the production 1Password directories; with
/// no account home, every CLI backend is off.
/// Test: `server_onepassword_enablement_ignores_a_spawner_chosen_machine_config`,
/// `server_onepassword_is_off_when_the_account_config_is_unreadable`.
pub fn backends_for(
    settings: &ServerSettings,
    onepassword_token: Option<SecretValue>,
) -> BackendFactory {
    backends_with(
        account_machine_config(),
        settings,
        onepassword_token,
        crate::store::program::onepassword_dirs(),
    )
}

/// [`backends_for`] with the account's machine config given.
///
/// What: a CLI backend opens through `open_backend_at_in` with
/// `account_config`, [`ServerSettings::template_root`], `onepassword_token`
/// and `op_dirs`, the directories searched for `op` when the account
/// config pins no `program` (#7524 P2-M2), only when [`account_machine`]
/// enables it; else [`SecretsError::BackendNotEnabled`].
/// The file is read on each open, so a change is seen on the next request.
/// `keychain` and `file` open through [`open_local`].
/// Test: `server_backends_for_opens_onepassword_only_when_enabled`,
/// `server_onepassword_enablement_ignores_a_spawner_chosen_machine_config`,
/// `server_onepassword_is_off_when_the_account_config_is_unreadable`.
pub(crate) fn backends_with(
    account_config: Option<PathBuf>,
    settings: &ServerSettings,
    onepassword_token: Option<SecretValue>,
    op_dirs: Vec<PathBuf>,
) -> BackendFactory {
    let template_root = settings.template_root.clone();
    Arc::new(move |id: &BackendId| {
        if !cli_backends().contains(id) {
            return open_local(id, KEYCHAIN_COMPILED);
        }
        // #7519: ruling 74 — `settings.machine_config` is the spawner's to
        // choose, so only the account's own file enables a CLI backend or
        // pins its `program`; a missing or unreadable one leaves it off.
        let config = account_config
            .as_deref()
            .filter(|path| account_machine(Some(path)).is_some_and(|machine| machine.enables(id)));
        let Some(config) = config else {
            return Err(SecretsError::BackendNotEnabled {
                backend: id.to_string(),
            });
        };
        open_backend_at_in(
            id,
            config,
            &template_root,
            onepassword_token.clone(),
            &op_dirs,
        )
    })
}

/// [`open_backend`] for a backend that is not CLI-backed, refusing
/// `keychain` on a build that links no Keychain (#7519 P4).
///
/// Why: A4 — off macOS `KeychainBackend` opens but fails every call, so
/// doctor read a healthy Keychain row on a Linux host with no Keychain.
/// What: `keychain` without `keychain_compiled` is
/// [`SecretsError::UnknownBackend`], the error each of its calls returned;
/// anything else is [`open_backend`].
/// Test: `doctor_keychain_row_is_not_compiled_without_a_keychain`.
pub(crate) fn open_local(
    id: &BackendId,
    keychain_compiled: bool,
) -> Result<Arc<dyn SecretBackend>, SecretsError> {
    if id.as_str() == BackendId::KEYCHAIN && !keychain_compiled {
        return Err(SecretsError::UnknownBackend {
            backend: id.to_string(),
        });
    }
    open_backend(id)
}

/// What the binary read from its own environment at start (#7519 P4).
///
/// Why: A10 — the binary takes the 1Password service-account token out of
/// its environment before any thread starts and hands it to the factory;
/// doctor may report only that it was there (owner ruling Q1).
/// What: the token's presence, never its value, and the `PATH` doctor's
/// tool detection searches (DOC-74 §7), so `Debug` carries no secret.
/// Test: `doctor_reports_token_presence_and_never_the_token`,
/// `doctor_detects_unsupported_tools_on_the_start_path_without_running_them`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StartEnv {
    /// Whether `OP_SERVICE_ACCOUNT_TOKEN` was set and non-empty at start.
    pub onepassword_token: bool,
    /// The `PATH` the binary read at start; only its absolute entries are
    /// searched.
    pub search_path: Option<OsString>,
}

impl StartEnv {
    /// This environment with the 1Password token's presence set.
    pub fn with_onepassword_token(mut self, present: bool) -> Self {
        self.onepassword_token = present;
        self
    }

    /// This environment with the `PATH` read at start.
    pub fn with_search_path(mut self, path: Option<OsString>) -> Self {
        self.search_path = path;
        self
    }
}

/// The account's own machine config: [`MACHINE_CONFIG_SUBPATH`] under
/// `platform::account_home_dir`, or `None` when that home is unknown.
///
/// Why: #7524 H1 and ruling 74 — `--machine-config` and `$HOME` are the
/// spawner's to set; this path is not.
/// Test: `server_file_consent_defaults_to_the_account_home_config`.
fn account_machine_config() -> Option<PathBuf> {
    platform::account_home_dir()
        .ok()
        .map(|home| home.join(MACHINE_CONFIG_SUBPATH))
}

/// The account machine config at `path`, if it is given and loads.
///
/// What: a missing path, a missing or unreadable file, or a parse failure
/// is `None`, so a caller that reads enablement from it fails closed.
pub(crate) fn account_machine(path: Option<&Path>) -> Option<MachineSecretsConfig> {
    path.and_then(|path| load_machine_at(path).ok().flatten())
}

/// What every handler shares.
///
/// What: the settings, the names-only index rooted at
/// [`ServerSettings::index_root`], the backend factory, the audit log at
/// [`ServerSettings::audit_log`] (#4567), whether this server acts as a
/// Keychain build (#7524), and the account's own machine config, the one
/// file that may consent to `file` writes there (#7524 H1). `Debug` shows
/// settings and the index root only.
// #9073: S8's grant registry (DOC-74 §15.8) joins this; build it with `new`.
#[non_exhaustive]
pub struct State {
    /// Paths and the idle window.
    pub settings: ServerSettings,
    /// The names-only index.
    pub index: NamesIndex,
    /// Backend id → implementation.
    pub backends: BackendFactory,
    /// The credential access audit log.
    pub(crate) audit: AuditSink,
    /// Whether this build links a Keychain, for the `file` posture checks.
    // #7524: a field, not the constant, so tests can act as either build.
    pub(crate) keychain_compiled: bool,
    /// The machine config whose `default_backend: file` consents to value
    /// writes into `file` on a Keychain build; `None` refuses them. #7519:
    /// also the one whose enabled CLI backends the delete sweep reaches.
    // #7524 H1: from the password database, never `--machine-config` or
    // `$HOME`; a crate-private field so only tests can aim it elsewhere.
    pub(crate) file_consent_config: Option<PathBuf>,
    /// What the binary read from its environment at start (#7519 P4).
    pub(crate) start: StartEnv,
    /// Replaces every method's deadline; tests only (#7524 P2-M1).
    pub(crate) deadline_override: Option<Duration>,
    /// Replaces [`MAX_BLOCKING_CALLS`]; tests only (#9572).
    pub(crate) admission_cap_override: Option<usize>,
}

impl State {
    /// State for `settings`, opening backends through `backends`.
    ///
    /// What: the file consent config is [`MACHINE_CONFIG_SUBPATH`] under
    /// `platform::account_home_dir`; `None` when that home is unknown.
    /// Test: `server_file_consent_defaults_to_the_account_home_config`.
    pub fn new(settings: ServerSettings, backends: BackendFactory) -> Self {
        let index = NamesIndex::at(&settings.index_root);
        let audit = AuditSink::new(settings.audit_log.clone(), settings.audit_max_bytes);
        Self {
            settings,
            index,
            backends,
            audit,
            keychain_compiled: KEYCHAIN_COMPILED,
            // #7524 H1: resolved once; a lookup failure refuses `file` writes.
            file_consent_config: account_machine_config(),
            start: StartEnv::default(),
            deadline_override: None,
            admission_cap_override: None,
        }
    }

    /// The whole-operation deadline for one request of `name`.
    fn deadline_for(&self, name: &str) -> Duration {
        self.deadline_override
            .unwrap_or_else(|| request_deadline(name))
    }

    /// How many method bodies may run at once (#9572).
    fn admission_cap(&self) -> usize {
        self.admission_cap_override.unwrap_or(MAX_BLOCKING_CALLS)
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("settings", &self.settings)
            .field("index_root", &self.index.root())
            .finish_non_exhaustive()
    }
}

/// How many method bodies may run on the blocking pool at once (#9572).
///
/// Why: a backend call that never returns (a file backend on a hung mount)
/// keeps its blocking-pool thread forever; no thread can be cancelled. With
/// no cap, each such request adds a thread, up to tokio's default pool of
/// 512, and later requests wait for the pool.
/// What: the router takes one permit before it spawns a body and moves the
/// permit into the body's closure, so a permit comes back only when its
/// thread finishes. At most this many threads are ever stuck; a request
/// that waits for a permit past its deadline answers
/// [`ErrorKind::DeadlineExceeded`].
/// Test: `server_stuck_backend_calls_do_not_starve_a_later_request`,
/// `server_timed_out_request_keeps_its_permit_until_its_call_returns`.
pub(crate) const MAX_BLOCKING_CALLS: usize = 64;

/// Every method this socket serves, with its body.
pub(crate) const METHODS: [(&str, MethodFn); 6] = [
    (method::SCOPES, methods::scopes),
    (method::LIST, methods::list),
    (method::SET, methods::set),
    (method::DELETE, methods::delete),
    (method::COPY, methods::copy),
    (doctor::DOCTOR, doctor::doctor),
];

/// The `secrets.*` router over `state`.
///
/// Why: registering each method as `typed::<Value, Value>` means the router's
/// own decode — `params do not decode: {e}` — can never fail; decoding
/// happens in the method body, which drops the serde message.
/// What: each call runs its body on tokio's blocking pool; an `Err` becomes
/// [`ErrorKind::to_rpc`] for that method, and a body that panics becomes
/// [`ErrorKind::Internal`]. #7524 P2-M1: the request's deadline starts when
/// the call arrives and is set on the body's thread, so every CLI call the
/// body makes is bounded by it (`store::deadline`). #9572: every method
/// shares one admission semaphore of [`State::admission_cap`] permits; see
/// [`run_blocking`].
/// Test: `server_error_text_is_fixed_per_method_and_kind`,
/// `server_request_past_its_deadline_is_a_definite_error_and_commits_nothing`,
/// `server_stuck_backend_calls_do_not_starve_a_later_request`.
pub(crate) fn build_router(state: Arc<State>) -> RpcRouter {
    let admission = Arc::new(Semaphore::new(state.admission_cap()));
    METHODS
        .into_iter()
        .fold(RpcRouter::new(), |router, (name, body)| {
            let state = Arc::clone(&state);
            let admission = Arc::clone(&admission);
            router.typed::<Value, Value, _, _>(name, move |params| {
                let state = Arc::clone(&state);
                let admission = Arc::clone(&admission);
                async move {
                    // #7524 P2-M1: one deadline for the whole request.
                    let deadline = Instant::now() + state.deadline_for(name);
                    run_blocking(state, admission, deadline, body, params)
                        .await
                        .map_err(|kind| kind.to_rpc(name))
                }
            })
        })
}

/// Run `body` on the blocking pool under `deadline` and one admission permit.
///
/// Why: #9572 — the router awaited the body with no bound, so a backend call
/// that never returns held its request forever, and nothing capped how many
/// threads such calls could hold.
/// What: waits for a permit from `admission` until `deadline`, then spawns
/// the body with the permit moved into its closure, and waits for it until
/// `deadline` plus [`BODY_GRACE`], so a body that stops at the deadline still
/// gives its own answer. Past either wait the answer is
/// [`ErrorKind::DeadlineExceeded`]; a closed semaphore or a panicking body
/// is [`ErrorKind::Internal`]. Neither is ever a success.
/// Test: `server_stuck_backend_calls_do_not_starve_a_later_request`,
/// `server_timed_out_request_keeps_its_permit_until_its_call_returns`,
/// `server_closed_admission_is_internal_and_never_runs_the_body`.
pub(crate) async fn run_blocking(
    state: Arc<State>,
    admission: Arc<Semaphore>,
    deadline: Instant,
    body: MethodFn,
    params: Value,
) -> Result<Value, ErrorKind> {
    let until = tokio::time::Instant::from_std(deadline);
    // #9572: the wait for a permit counts against the request's deadline.
    let permit = match tokio::time::timeout_at(until, admission.acquire_owned()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(_closed)) => return Err(ErrorKind::Internal),
        Err(_elapsed) => return Err(ErrorKind::DeadlineExceeded),
    };
    let task = tokio::task::spawn_blocking(move || {
        // #9572: the permit is the thread's, released only when it finishes.
        let _permit = permit;
        crate::store::deadline::within(deadline, || body(&state, params))
    });
    // See #9572: past the deadline and its grace the request answers
    // `DeadlineExceeded`, but the body's thread cannot be cancelled. It keeps
    // running, and keeps its permit, until the backend call returns; the cap
    // bounds such threads.
    match tokio::time::timeout_at(until + BODY_GRACE, task).await {
        Ok(joined) => joined.unwrap_or(Err(ErrorKind::Internal)),
        Err(_elapsed) => Err(ErrorKind::DeadlineExceeded),
    }
}

/// Why [`serve`] returned.
///
/// What: an idle exit is the normal end of an on-demand server's life; a
/// shutdown is the caller's future resolving (a signal, or a test).
// #9073: this crate's own enum, so a trusty-common bump never changes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServeExit {
    /// The shutdown future resolved and in-flight connections drained.
    Shutdown,
    /// No connection arrived for the whole idle window.
    Idle,
}

impl ServeExit {
    fn from_uds(exit: UdsServeExit) -> Self {
        match exit {
            UdsServeExit::Shutdown => Self::Shutdown,
            UdsServeExit::Idle => Self::Idle,
        }
    }
}

/// The socket could not be prepared or bound.
///
/// What: the path and the bind layer's reason. Carries nothing a client sent.
/// The reason is opaque (#9073): its type is trusty-common's and not part of
/// this crate's API; read it through `Display` or `downcast_ref`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServeError {
    /// The socket path has no parent directory.
    #[error("socket path {path} has no parent directory")]
    NoParent {
        /// The socket path.
        path: PathBuf,
    },
    /// The 0700 socket directory could not be created or verified.
    #[error("cannot prepare socket directory {path}: {source}")]
    SocketDir {
        /// The directory.
        path: PathBuf,
        /// The bind layer's reason.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The bind was refused — including a live instance already serving.
    #[error("cannot bind {path}: {source}")]
    Bind {
        /// The socket path.
        path: PathBuf,
        /// The bind layer's reason, including a live owner already serving.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Bind the socket and serve until idle or `shutdown`, then unlink it.
///
/// Why: see the module docs.
/// What: first, on a `cli-backends` build, sweeps template files a crashed
/// server left under [`ServerSettings::template_root`] (#7519). An entry the
/// sweep cannot remove does not stop it: every other stale directory is
/// still removed, and the first failure is reported on stderr. A refused
/// root (symlink, too wide, another user's) stops the sweep before any
/// entry. Either way serving goes on; the failed entry stays on disk until
/// a later start removes it, and a refused root also refuses every template
/// write. Then `prepare_socket_dir` on
/// the socket's parent, then
/// `bind_singleton_hardened`, which takes over only a socket the kernel
/// proves nobody serves and refuses a live one, so a second instance never
/// clobbers the first. Serves with an [`IdleTracker`] of
/// [`ServerSettings::idle_timeout`]. On return, removes the socket file
/// before the listener drops (the order trusty-common's `RpcServer` uses).
///
/// # Errors
///
/// [`ServeError`] when the directory or the bind is refused; nothing is
/// unlinked in that case.
///
/// Test: `server_exits_when_idle_and_removes_its_socket`,
/// `server_second_instance_is_refused_and_the_first_keeps_serving`,
/// `server_bind_failure_is_reported_and_the_occupant_kept`,
/// `server_startup_sweeps_stale_template_dirs`.
pub async fn serve(
    settings: ServerSettings,
    backends: BackendFactory,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<ServeExit, ServeError> {
    serve_with(settings, backends, StartEnv::default(), shutdown).await
}

/// [`serve`], reporting `start` in `secrets.doctor` (#7519 P4).
///
/// What: the binary's entry point; `start` is what it read from its own
/// environment before the runtime started.
/// Test: `binary_doctor_reports_token_presence_and_never_the_token`.
pub async fn serve_with(
    settings: ServerSettings,
    backends: BackendFactory,
    start: StartEnv,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<ServeExit, ServeError> {
    let mut state = State::new(settings, backends);
    state.start = start;
    serve_state(state, shutdown).await
}

/// [`serve`] over a prepared [`State`].
// #7524: tests set `State::keychain_compiled` to act as either build.
pub(crate) async fn serve_state(
    state: State,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<ServeExit, ServeError> {
    // #7519: owner ruling — a crash skips the template guard's drop, so the
    // leftover is removed here, before any request can write a new one.
    #[cfg(all(unix, feature = "cli-backends"))]
    sweep_templates(&state.settings.template_root);
    let socket = state.settings.socket.clone();
    let dir = socket.parent().ok_or_else(|| ServeError::NoParent {
        path: socket.clone(),
    })?;
    prepare_socket_dir(dir).map_err(|source| ServeError::SocketDir {
        path: dir.to_path_buf(),
        source: Box::new(source),
    })?;
    let listener = bind_singleton_hardened(&socket)
        .await
        .map_err(|source| ServeError::Bind {
            path: socket.clone(),
            source: Box::new(source),
        })?;
    let idle = IdleTracker::new(state.settings.idle_timeout);
    let router = Arc::new(build_router(Arc::new(state)));
    let exit = serve_until_idle(
        &listener,
        router,
        RpcServeOptions::default(),
        shutdown,
        Some(idle),
    )
    .await;
    remove_socket(&socket);
    drop(listener);
    Ok(ServeExit::from_uds(exit))
}

/// Run `future` on `runtime` to completion, then shut the runtime down.
///
/// Why: #9572 — the binary's exit path, in the crate so a test can drive it.
/// What: `block_on`, then the runtime is dropped.
/// Test: `server_process_exit_is_bounded_while_a_call_is_stuck`.
pub fn run_to_exit<T>(runtime: tokio::runtime::Runtime, future: impl Future<Output = T>) -> T {
    runtime.block_on(future)
}

/// Remove stale template directories under `root`, reporting on stderr.
///
/// What: [`crate::store::cli::sweep_stale_templates`]; the report names the
/// directory and a count, or the first error, which names a path, never
/// content. The sweep has already visited every other entry by then.
#[cfg(all(unix, feature = "cli-backends"))]
fn sweep_templates(root: &Path) {
    match crate::store::cli::sweep_stale_templates(root) {
        Ok(0) => {}
        Ok(removed) => eprintln!(
            "trusty-secrets: removed {removed} stale template director{} under {}",
            if removed == 1 { "y" } else { "ies" },
            root.display()
        ),
        Err(e) => eprintln!("trusty-secrets: template sweep incomplete: {e}"),
    }
}

/// Unlink the socket file; already gone is fine.
fn remove_socket(socket: &Path) {
    if let Err(e) = std::fs::remove_file(socket)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("trusty-secrets: could not remove {}: {e}", socket.display());
    }
}
