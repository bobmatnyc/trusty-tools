//! The broker: four private Unix sockets in the state directory (`service.py` `Broker`).
//!
//! Why: the extension worker (`service.sock`), the popup through the native
//! host (`enrollment.sock`), local agents (`agent.sock`) and the operator CLI
//! (`operator.sock`) each get their own socket so each can carry its own
//! peer check. Paths, modes and the singleton lock must match Python so the
//! launchers, the CLI and a rollback keep working.
//! What: [`Broker::start`] opens the registry (0700 dir, 0600 file), takes a
//! non-blocking exclusive `flock` on `service.lock`, replaces a stale socket
//! (refusing a non-socket at the path), binds each socket and `chmod`s it
//! 0600, then serves each accept loop. [`Broker::close`] stops the sweeper,
//! the listeners and every connection, and unlinks the sockets.
//! Test: `broker_sockets_have_python_paths_and_modes`,
//! `a_second_broker_is_refused_by_the_lock`, `a_non_socket_at_a_socket_path_is_refused`
//! in `tests/parity_broker.rs`.

use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use rusqlite::Connection;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinSet};

use crate::audit::{Audit, Clock, SystemClock};
use crate::entropy::{Entropy, Monotonic, OsEntropy, SystemMonotonic};
use crate::grants::Grant;
use crate::peer_conn::Peer;
use crate::peer_darwin::HostRefused;
use crate::registry::{Registry, RegistryError};
use crate::vault::{KeyStore, SERVICE, Vault, VaultError};

/// The four sockets, in Python's bind order.
pub const SOCKETS: [&str; 4] = [
    "service.sock",
    "agent.sock",
    "enrollment.sock",
    "operator.sock",
];

// See review af942b9e95 #4: the seam is nameable outside the crate only with
// the `test-seams` feature, so a release build cannot swap in an open check.
mod host_seam {
    use super::{HostRefused, Path};

    /// The enrollment socket's peer identity check (`verify_chrome_host`).
    pub trait HostCheck: Send + Sync {
        /// `Ok` when the connected peer is our native host under Chrome Stable.
        fn verify(
            &self,
            sock: &std::os::unix::net::UnixStream,
            state: &Path,
        ) -> Result<(), HostRefused>;
    }
}
#[cfg(feature = "test-seams")]
pub use host_seam::HostCheck;
#[cfg(not(feature = "test-seams"))]
pub(crate) use host_seam::HostCheck;

/// The real Darwin check from [`crate::peer_darwin::verify_chrome_host`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ChromeHostCheck;

impl HostCheck for ChromeHostCheck {
    fn verify(
        &self,
        sock: &std::os::unix::net::UnixStream,
        state: &Path,
    ) -> Result<(), HostRefused> {
        crate::peer_darwin::verify_chrome_host(sock, state)
    }
}

/// Why the broker could not start.
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    /// Registry open failed (private-file checks, SQLite).
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Vault setup failed.
    #[error(transparent)]
    Vault(#[from] VaultError),
    /// `service.lock` is held, a socket path is occupied, or binding failed.
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// What a broker runs on; [`BrokerOptions::new`] gives the production seams.
pub struct BrokerOptions {
    /// State directory (`--state`).
    pub state: PathBuf,
    /// Keychain store for the vault's wrapping key.
    pub store: Box<dyn KeyStore>,
    /// Keychain service name (production: [`SERVICE`]).
    pub service: String,
    /// Audit `ts` clock.
    pub audit_clock: Box<dyn Clock>,
    /// Randomness for nonces and ids.
    pub entropy: Arc<dyn Entropy>,
    /// Grant clock.
    pub clock: Arc<dyn Monotonic>,
    /// Lease clock (`time.monotonic` in `leases.py`/`pages.py`).
    pub lease_clock: Arc<dyn Monotonic>,
    /// Wall clock of `TabClaimed.expires_at` (`time.time`).
    pub wall_clock: Arc<dyn Clock>,
    /// Enrollment peer check (test builds only; see review af942b9e95 #4).
    #[cfg(feature = "test-seams")]
    pub host_check: Arc<dyn HostCheck>,
}

impl BrokerOptions {
    /// Production seams: OS entropy, system clocks, the real Chrome host check.
    pub fn new(state: &Path, store: Box<dyn KeyStore>) -> BrokerOptions {
        BrokerOptions {
            state: state.to_path_buf(),
            store,
            service: SERVICE.to_owned(),
            audit_clock: Box::new(SystemClock),
            entropy: Arc::new(OsEntropy),
            clock: Arc::new(SystemMonotonic),
            lease_clock: Arc::new(SystemMonotonic),
            wall_clock: Arc::new(SystemClock),
            #[cfg(feature = "test-seams")]
            host_check: Arc::new(ChromeHostCheck),
        }
    }
}

/// A tab lease taken by `select`.
#[derive(Debug, Clone)]
pub struct Claim {
    /// Peer generation.
    pub generation: String,
    /// Tab id.
    pub tab: i64,
    /// Owning session's owner token.
    pub owner: String,
    /// Owning session id.
    pub owner_session: u64,
    /// Lease token.
    pub token: String,
    /// Lease clock deadline.
    pub deadline: f64,
    /// A command is in flight.
    pub busy: bool,
    /// Last document id.
    pub document: Option<String>,
    /// Last origin (as the worker wrote it).
    pub origin: Option<String>,
    /// Shows the page indicator; `false` for code-session leases (#11239).
    pub indicator: bool,
}

/// An attached agent session, as `detach` sees it.
#[derive(Debug, Clone)]
pub struct SessionRef {
    /// Session id.
    pub id: u64,
    /// Profile it acts on.
    pub profile: String,
    /// Closes the agent connection.
    pub kill: Arc<Notify>,
}

/// Mutable broker state; held only between awaits.
pub struct Inner {
    /// Registry connection.
    pub db: Connection,
    /// Active grants, in creation order.
    pub grants: Vec<Grant>,
    /// Ended grants a session still names.
    pub ended: Vec<Grant>,
    /// Connected workers, one per profile.
    pub peers: Vec<Arc<Peer>>,
    /// Tab leases.
    pub claims: Vec<Claim>,
    /// Attached agent sessions.
    pub sessions: Vec<SessionRef>,
    /// Set by `close`; no sweep runs after it.
    pub halted: bool,
    /// The deadline sweep task.
    pub sweeper: Option<AbortHandle>,
    /// Last session id handed out.
    pub next_session: u64,
}

impl Inner {
    /// The connected worker of `profile`.
    pub fn peer(&self, profile: &str) -> Option<Arc<Peer>> {
        self.peers.iter().find(|p| p.profile == profile).cloned()
    }

    /// An active or session-held ended grant.
    pub fn grant(&self, ident: &str) -> Option<&Grant> {
        self.grants
            .iter()
            .chain(self.ended.iter())
            .find(|g| g.ident == ident)
    }
}

/// State shared by every handler.
pub struct Core {
    /// `--state`, absolute (not resolved), as Python's `expanduser().absolute()`.
    pub state: PathBuf,
    /// Pairing-key vault.
    pub vault: Vault,
    /// Audit log.
    pub audit: Audit,
    /// Randomness.
    pub entropy: Arc<dyn Entropy>,
    /// Grant clock.
    pub clock: Arc<dyn Monotonic>,
    /// Lease clock (`time.monotonic`).
    pub lease_clock: Arc<dyn Monotonic>,
    /// Wall clock (`time.time`).
    pub wall_clock: Arc<dyn Clock>,
    /// Enrollment peer check.
    pub(crate) host_check: Arc<dyn HostCheck>,
    /// Self, for spawned tasks.
    pub this: Weak<Core>,
    inner: Mutex<Inner>,
}

impl Core {
    /// Lock the mutable state (a poisoned lock is still usable state).
    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A running broker.
pub struct Broker {
    core: Arc<Core>,
    tasks: JoinSet<()>,
    paths: Vec<PathBuf>,
    _lock: File,
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

impl Broker {
    /// Open the state, take the lock and serve the four sockets.
    ///
    /// Why: one entry point keeps the start order of `Broker.__aenter__`.
    /// What: registry first, then `flock(LOCK_EX|LOCK_NB)` on `service.lock`
    /// (a held lock is an error), then each socket. Must run inside a tokio runtime.
    /// Test: `broker_sockets_have_python_paths_and_modes`, `a_second_broker_is_refused_by_the_lock`.
    pub fn start(options: BrokerOptions) -> Result<Broker, BrokerError> {
        let state = absolute(&options.state)?;
        let registry = Registry::open(&state)?;
        let lock = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(state.join("service.lock"))?;
        // SAFETY: flock on an fd we own; released when `lock` drops.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let vault = Vault::with_service(&state, &options.service, options.store)?;
        crate::legacy::finish_removals(registry.connection(), &vault, &state)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let inner = Inner {
            db: registry.into_connection(),
            grants: Vec::new(),
            ended: Vec::new(),
            peers: Vec::new(),
            claims: Vec::new(),
            sessions: Vec::new(),
            halted: false,
            sweeper: None,
            next_session: 0,
        };
        let core = Arc::new_cyclic(|this| Core {
            audit: Audit::with_clock(&state, options.audit_clock),
            state: state.clone(),
            vault,
            entropy: options.entropy,
            clock: options.clock,
            lease_clock: options.lease_clock,
            wall_clock: options.wall_clock,
            #[cfg(feature = "test-seams")]
            host_check: options.host_check,
            // See review af942b9e95 #4: without the seam the real check is fixed.
            #[cfg(not(feature = "test-seams"))]
            host_check: Arc::new(ChromeHostCheck),
            this: this.clone(),
            inner: Mutex::new(inner),
        });
        let mut broker = Broker {
            core,
            tasks: JoinSet::new(),
            paths: Vec::new(),
            _lock: lock,
        };
        for name in SOCKETS {
            let path = state.join(name);
            match fs::symlink_metadata(&path) {
                Ok(meta) if !meta.file_type().is_socket() => {
                    return Err(io::Error::other("socket path is occupied").into());
                }
                Ok(_) => fs::remove_file(&path)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let listener = UnixListener::bind(&path)?;
            broker.paths.push(path.clone());
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
            let core = Arc::clone(&broker.core);
            broker.tasks.spawn(accept_loop(core, listener, name));
        }
        Ok(broker)
    }

    /// The shared state (tests inspect grants through it).
    pub fn core(&self) -> &Arc<Core> {
        &self.core
    }

    /// Stop serving: halt the sweeper, close every connection, unlink the sockets.
    pub async fn close(mut self) {
        {
            let mut inner = self.core.lock();
            inner.halted = true;
            if let Some(task) = inner.sweeper.take() {
                task.abort();
            }
            for peer in &inner.peers {
                peer.kill();
            }
            for session in &inner.sessions {
                session.kill.notify_one();
            }
        }
        self.tasks.shutdown().await;
        for path in self.paths.drain(..) {
            let _ = fs::remove_file(path);
        }
    }
}

async fn accept_loop(core: Arc<Core>, listener: UnixListener, name: &'static str) {
    let mut conns = JoinSet::new();
    loop {
        let (stream, _) = accept_next(|| listener.accept()).await;
        let core = Arc::clone(&core);
        conns.spawn(handle(core, stream, name));
        while conns.try_join_next().is_some() {}
    }
}

/// CPython asyncio `constants.ACCEPT_RETRY_DELAY`, in seconds.
pub const ACCEPT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Why: a persistent accept error such as EMFILE must not spin the runtime.
/// What: accept until one call succeeds. On EMFILE, ENFILE, ENOBUFS or ENOMEM
/// wait [`ACCEPT_RETRY_DELAY`] first, as CPython's `selector_events`
/// `_accept_connection` does; any other error retries at once, as there.
/// Test: `accept_waits_the_retry_delay_after_emfile` in this file.
async fn accept_next<T, F, Fut>(mut accept: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = io::Result<T>>,
{
    loop {
        match accept().await {
            Ok(v) => return v,
            // #11440: Python `call_later(ACCEPT_RETRY_DELAY, _start_serving)`.
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
                ) =>
            {
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
            }
            Err(_) => tokio::task::yield_now().await,
        }
    }
}

async fn handle(core: Arc<Core>, stream: UnixStream, name: &'static str) {
    match name {
        "service.sock" => crate::broker_service::browser(core, stream).await,
        "agent.sock" => crate::broker_agent::agent(core, stream).await,
        "enrollment.sock" => crate::broker_enroll::bootstrap(core, stream).await,
        _ => crate::broker_enroll::operator(core, stream).await,
    }
}

/// `peer_ok`: the uid check, logged as `peer_uid` with that reason in the refusal.
pub fn peer_ok(
    core: &Core,
    stream: &UnixStream,
    site: &str,
) -> Result<(), crate::refusal::Refusal> {
    crate::peer::verify_peer(stream).map_err(|r| {
        core.audit.refuse(site, "peer_uid", None, None);
        r.with("reason", "peer_uid")
    })
}

/// `relayed`: a refusal for service.sock or agent.sock without its `reason`.
pub fn relayed(r: &crate::refusal::Refusal) -> crate::pyjson::Json {
    let mut out = crate::refusal::Refusal::new(r.code.clone());
    out.details = r
        .details
        .iter()
        .filter(|(k, _)| k != "reason")
        .cloned()
        .collect();
    out.response()
}

/// `refused`: log a refusal with the request's profile and email domain; return `Unauthorized`.
pub fn refused(
    core: &Core,
    site: &str,
    reason: &str,
    request: Option<&crate::pyjson::Json>,
) -> crate::refusal::Refusal {
    let profile = request.and_then(|r| crate::pairing::profile_id(r.get("profile")));
    let domain = request
        .and_then(|r| r.get("email"))
        .and_then(crate::pyjson::Json::as_str)
        .and_then(crate::audit::email_domain);
    core.audit.refuse(site, reason, profile, domain.as_deref());
    crate::refusal::Refusal::new("Unauthorized").with("reason", reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accept_waits_the_retry_delay_after_emfile() {
        let mut calls = 0;
        let start = std::time::Instant::now();
        let got = accept_next(|| {
            calls += 1;
            let r = if calls == 1 {
                Err(io::Error::from_raw_os_error(libc::EMFILE))
            } else {
                Ok(calls)
            };
            async move { r }
        })
        .await;
        assert_eq!(got, 2);
        let waited = start.elapsed();
        assert!(waited >= ACCEPT_RETRY_DELAY, "retried after {waited:?}");
    }
}
