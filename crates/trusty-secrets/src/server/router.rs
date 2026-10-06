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
    IdleTracker, RpcRouter, RpcServeOptions, ServeExit, serve_until_idle,
};
use trusty_common::uds::{UdsSecurityError, bind_singleton_hardened, prepare_socket_dir};

use super::errors::ErrorKind;
use super::methods::{self, MethodFn};
use super::settings::ServerSettings;
use crate::api::methods::method;
use crate::api::{BackendId, SecretsError};
use crate::store::{NamesIndex, SecretBackend, open_backend};

/// Maps a configured backend id to an implementation.
///
/// Why: production opens the real Keychain through S1's `open_backend`;
/// tests inject S1's `MemoryBackend` so no test touches the OS keychain.
pub type BackendFactory =
    Arc<dyn Fn(&BackendId) -> Result<Arc<dyn SecretBackend>, SecretsError> + Send + Sync>;

/// The production factory: S1's [`open_backend`].
pub fn default_backends() -> BackendFactory {
    Arc::new(open_backend)
}

/// What every handler shares.
///
/// What: the settings, the names-only index rooted at
/// [`ServerSettings::index_root`], and the backend factory. `Debug` shows
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
}

impl State {
    /// State for `settings`, opening backends through `backends`.
    pub fn new(settings: ServerSettings, backends: BackendFactory) -> Self {
        let index = NamesIndex::at(&settings.index_root);
        Self {
            settings,
            index,
            backends,
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
pub const METHODS: [(&str, MethodFn); 6] = [
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
pub fn build_router(state: Arc<State>) -> RpcRouter {
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

/// The socket could not be prepared or bound.
///
/// What: the path and trusty-common's reason. Carries nothing a client sent.
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
        /// trusty-common's reason.
        #[source]
        source: UdsSecurityError,
    },
    /// The bind was refused — including a live instance already serving.
    #[error("cannot bind {path}: {source}")]
    Bind {
        /// The socket path.
        path: PathBuf,
        /// trusty-common's reason; `AlreadyServing` for a live owner.
        #[source]
        source: UdsSecurityError,
    },
}

/// Bind the socket and serve until idle or `shutdown`, then unlink it.
///
/// Why: see the module docs.
/// What: `prepare_socket_dir` on the socket's parent, then
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
/// `server_bind_failure_is_reported_and_the_occupant_kept`.
pub async fn serve(
    settings: ServerSettings,
    backends: BackendFactory,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<ServeExit, ServeError> {
    let socket = settings.socket.clone();
    let dir = socket.parent().ok_or_else(|| ServeError::NoParent {
        path: socket.clone(),
    })?;
    prepare_socket_dir(dir).map_err(|source| ServeError::SocketDir {
        path: dir.to_path_buf(),
        source,
    })?;
    let listener = bind_singleton_hardened(&socket)
        .await
        .map_err(|source| ServeError::Bind {
            path: socket.clone(),
            source,
        })?;
    let idle = IdleTracker::new(settings.idle_timeout);
    let router = Arc::new(build_router(Arc::new(State::new(settings, backends))));
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
    Ok(exit)
}

/// Unlink the socket file; already gone is fine.
fn remove_socket(socket: &Path) {
    if let Err(e) = std::fs::remove_file(socket)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("trusty-secrets: could not remove {}: {e}", socket.display());
    }
}
