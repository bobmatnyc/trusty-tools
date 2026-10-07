//! The router, the bind, and the serve loop of the on-demand socket.
//!
//! Why: owner rulings 28 and 31 — the socket spawns on first call, exits when
//! idle, and is no daemon. Ruling 32 — the bind, the peer check and the idle
//! loop are trusty-common's, not hand-rolled here.
//! What: [`State`] (settings, index, backend factory), [`build_router`]
//! (each `secrets.*` method on the blocking pool, errors folded to fixed
//! text), and [`serve`]: `prepare_socket_dir` (0700) →
//! `bind_singleton_hardened` (0600, refuses a live owner) →
//! `serve_until_idle` (every connection uid-checked by `handle_connection`
//! before a byte is read) → unlink the socket → drop the listener.
//! Test: `server_tests.rs` — round trips, `server_exits_when_idle_and_removes_its_socket`,
//! `server_second_instance_is_refused_and_the_first_keeps_serving`,
//! `server_bind_failure_is_reported_and_the_occupant_kept`.

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use trusty_common::uds::server::{
    IdleTracker, RpcRouter, RpcServeOptions, ServeExit as UdsServeExit, serve_until_idle,
};
use trusty_common::uds::{bind_singleton_hardened, prepare_socket_dir};

use super::audit::AuditSink;
use super::errors::ErrorKind;
use super::methods::{self, MethodFn};
use super::settings::ServerSettings;
use crate::api::methods::method;
use crate::api::{BackendId, SecretValue, SecretsError};
use crate::store::config::MACHINE_CONFIG_SUBPATH;
use crate::store::{
    KEYCHAIN_COMPILED, NamesIndex, SecretBackend, open_backend, open_backend_at, platform,
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
/// Why: a CLI backend must read the machine config the server was told to
/// use, write its template files where the startup sweep looks, and get the
/// service-account token the binary took out of its own environment.
/// What: [`open_backend_at`] with [`ServerSettings::machine_config`],
/// [`ServerSettings::template_root`] and `onepassword_token`. The machine
/// config is read on each open, so a change is seen on the next request.
/// Test: `server_backends_for_opens_onepassword_only_when_enabled`.
pub fn backends_for(
    settings: &ServerSettings,
    onepassword_token: Option<SecretValue>,
) -> BackendFactory {
    let machine_config = settings.machine_config.clone();
    let template_root = settings.template_root.clone();
    Arc::new(move |id: &BackendId| {
        open_backend_at(
            id,
            &machine_config,
            &template_root,
            onepassword_token.clone(),
        )
    })
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
    /// writes into `file` on a Keychain build; `None` refuses them.
    // #7524 H1: from the password database, never `--machine-config` or
    // `$HOME`; a crate-private field so only tests can aim it elsewhere.
    pub(crate) file_consent_config: Option<PathBuf>,
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
            file_consent_config: platform::account_home_dir()
                .ok()
                .map(|home| home.join(MACHINE_CONFIG_SUBPATH)),
        }
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

/// Every method this socket serves, with its body.
pub(crate) const METHODS: [(&str, MethodFn); 6] = [
    (method::SCOPES, methods::scopes),
    (method::LIST, methods::list),
    (method::SET, methods::set),
    (method::DELETE, methods::delete),
    (method::COPY, methods::copy),
    (methods::DOCTOR, methods::doctor),
];

/// The `secrets.*` router over `state`.
///
/// Why: registering each method as `typed::<Value, Value>` means the router's
/// own decode — `params do not decode: {e}` — can never fail; decoding
/// happens in the method body, which drops the serde message.
/// What: each call runs its body on tokio's blocking pool; an `Err` becomes
/// [`ErrorKind::to_rpc`] for that method, and a body that panics becomes
/// [`ErrorKind::Internal`].
/// Test: `server_error_text_is_fixed_per_method_and_kind`.
pub(crate) fn build_router(state: Arc<State>) -> RpcRouter {
    METHODS
        .into_iter()
        .fold(RpcRouter::new(), |router, (name, body)| {
            let state = Arc::clone(&state);
            router.typed::<Value, Value, _, _>(name, move |params| {
                let state = Arc::clone(&state);
                async move {
                    tokio::task::spawn_blocking(move || body(&state, params))
                        .await
                        .unwrap_or(Err(ErrorKind::Internal))
                        .map_err(|kind| kind.to_rpc(name))
                }
            })
        })
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
/// server left under [`ServerSettings::template_root`] (#7519); a sweep
/// failure is reported on stderr and serving goes on, because the next
/// template write refuses the same directory. Then `prepare_socket_dir` on
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
    serve_state(State::new(settings, backends), shutdown).await
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

/// Remove stale template directories under `root`, reporting on stderr.
///
/// What: [`crate::store::cli::sweep_stale_templates`]; the report names the
/// directory and a count, or the error, which names a path, never content.
#[cfg(all(unix, feature = "cli-backends"))]
fn sweep_templates(root: &Path) {
    match crate::store::cli::sweep_stale_templates(root) {
        Ok(0) => {}
        Ok(removed) => eprintln!(
            "trusty-secrets: removed {removed} stale template director{} under {}",
            if removed == 1 { "y" } else { "ies" },
            root.display()
        ),
        Err(e) => eprintln!("trusty-secrets: template sweep skipped: {e}"),
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
