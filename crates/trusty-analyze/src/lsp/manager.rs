//! The child manager: one language server per `(workspace root, language)`
//! (#9078, DOC-72 §2).
//!
//! Why: a server is expensive to start and to keep, so one is shared per
//! workspace, started by the first call that needs it and killed when idle. A
//! server that dies or hangs must be reported, not respawned in a loop.
//! What: [`LspManager::session`] returns an [`LspLease`] on a live server,
//! spawning one if needed. With the flag off it refuses with
//! [`LspError::Disabled`] and spawns nothing. A dead or hung server is reaped
//! and its error replayed for `failure_cooloff` before a respawn is tried. A
//! sweeper task kills servers idle past `idle_timeout` while no lease is held.
//! [`LspManager::shutdown_all`] is the daemon's exit hook; dropping the manager
//! kills every child too.
//! Test: `idle_child_is_reaped_and_the_next_call_respawns`,
//! `crash_mid_request_is_an_error_not_a_hang`,
//! `stalled_requests_time_out_then_the_hung_child_is_reaped`,
//! `disabled_flag_spawns_nothing`, `daemon_idle_exit_kills_the_child`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::task::JoinHandle;

use super::child::LspChild;
use super::client::LspClient;
use super::config::{LspConfig, LspLanguage};
use super::{lock, LspError};

type Key = (PathBuf, LspLanguage);

enum Slot {
    Live(Arc<Session>),
    Failed { error: LspError, at: Instant },
}

#[derive(Default)]
struct Slots {
    map: HashMap<Key, Slot>,
    /// Read and written under the slots lock, so a sweeper deciding to stop and
    /// a spawn deciding to start one cannot miss each other.
    sweeping: bool,
}

struct Session {
    child: LspChild,
    last_used: Mutex<Instant>,
    in_flight: AtomicUsize,
}

impl Session {
    fn idle_for(&self) -> Duration {
        lock(&self.last_used).elapsed()
    }
}

struct Inner {
    config: LspConfig,
    slots: tokio::sync::Mutex<Slots>,
    sweeper: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(h) = lock(&self.sweeper).take() {
            h.abort();
        }
    }
}

/// Owns every language-server child of this daemon.
///
/// Why/What: see the module doc. Constructing one spawns nothing.
/// Test: `spawned_server_initializes_and_answers`.
pub struct LspManager {
    inner: Arc<Inner>,
}

/// A handle on one live server. While any lease is held the server is in
/// flight and the idle sweeper leaves it alone.
pub struct LspLease {
    session: Arc<Session>,
    request_timeout: Duration,
    max_timeouts: u32,
}

impl LspLease {
    /// The client, for notifications, document sync and diagnostics.
    pub fn client(&self) -> &LspClient {
        self.session.child.client()
    }

    /// The server's pid.
    pub fn pid(&self) -> u32 {
        self.session.child.pid()
    }

    /// Send a request under the configured per-request timeout.
    ///
    /// # Errors
    /// As [`LspClient::request`]; once `max_consecutive_timeouts` requests in a
    /// row have timed out, or a timeout left a write unfinished (the server
    /// stopped reading), the server is killed and this returns
    /// [`LspError::Hung`].
    ///
    /// Test: `stalled_requests_time_out_then_the_hung_child_is_reaped`,
    /// `a_server_that_stops_reading_is_killed_as_hung`.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, LspError> {
        let client = self.client();
        let res = client.request(method, params, self.request_timeout).await;
        if matches!(res, Err(LspError::Timeout { .. })) {
            let consecutive = client.consecutive_timeouts();
            // #9078: a wedged write closes the client; that server is hung.
            if consecutive >= self.max_timeouts || client.is_closed() {
                self.session.child.kill_as_hung(consecutive).await;
                return Err(LspError::Hung { consecutive });
            }
        }
        res
    }
}

impl Drop for LspLease {
    fn drop(&mut self) {
        *lock(&self.session.last_used) = Instant::now();
        self.session.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

impl LspManager {
    /// A manager over `config`. Pure: no process, no task.
    pub fn new(config: LspConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                slots: tokio::sync::Mutex::new(Slots::default()),
                sweeper: Mutex::new(None),
            }),
        }
    }

    /// The configuration this manager runs under.
    pub fn config(&self) -> &LspConfig {
        &self.inner.config
    }

    /// Whether the default-off flag is on.
    pub fn is_enabled(&self) -> bool {
        self.inner.config.enabled
    }

    /// A lease on the server for `root` and `language`, spawning it if needed.
    ///
    /// The slots lock is held across a spawn, so concurrent first calls for one
    /// root share a single child. A call for another root waits behind it; with
    /// one language and one workspace per machine in 1.9.0 that is accepted.
    ///
    /// # Errors
    /// [`LspError::Disabled`] with the flag off; [`LspError::Document`] for a
    /// root that does not exist; the recorded failure of a dead or hung server
    /// inside the cool-off; any [`LspChild::spawn`] error.
    ///
    /// Test: `disabled_manager_refuses_with_lsp_disabled`,
    /// `crash_mid_request_is_an_error_not_a_hang`.
    pub async fn session(&self, root: &Path, language: LspLanguage) -> Result<LspLease, LspError> {
        let config = &self.inner.config;
        if !config.enabled {
            return Err(LspError::Disabled);
        }
        let root = std::fs::canonicalize(root).map_err(|e| LspError::Document {
            path: root.display().to_string(),
            reason: format!("workspace root: {e}"),
        })?;
        let key = (root, language);
        let mut slots = self.inner.slots.lock().await;
        match slots.map.get(&key) {
            Some(Slot::Live(s)) if s.child.is_usable() => return Ok(self.lease(s)),
            Some(Slot::Live(s)) => {
                // #9078: reported, never silently respawned.
                let dead = Arc::clone(s);
                dead.child.shutdown(config.shutdown_grace).await;
                let error = dead.child.failure();
                let at = Instant::now();
                slots.map.insert(
                    key,
                    Slot::Failed {
                        error: error.clone(),
                        at,
                    },
                );
                return Err(error);
            }
            Some(Slot::Failed { error, at }) if at.elapsed() < config.failure_cooloff => {
                return Err(error.clone());
            }
            _ => {}
        }
        let command = config.command_for(language);
        match LspChild::spawn(command, &key.0, config).await {
            Ok(child) => {
                let session = Arc::new(Session {
                    child,
                    last_used: Mutex::new(Instant::now()),
                    in_flight: AtomicUsize::new(0),
                });
                let lease = self.lease(&session);
                slots.map.insert(key, Slot::Live(session));
                if !slots.sweeping {
                    slots.sweeping = true;
                    let task = tokio::spawn(sweep(Arc::downgrade(&self.inner)));
                    *lock(&self.inner.sweeper) = Some(task);
                }
                Ok(lease)
            }
            Err(error) => {
                tracing::warn!(error = %error, "language server did not start");
                let at = Instant::now();
                slots.map.insert(
                    key,
                    Slot::Failed {
                        error: error.clone(),
                        at,
                    },
                );
                Err(error)
            }
        }
    }

    fn lease(&self, session: &Arc<Session>) -> LspLease {
        session.in_flight.fetch_add(1, Ordering::SeqCst);
        *lock(&session.last_used) = Instant::now();
        LspLease {
            session: Arc::clone(session),
            request_timeout: self.inner.config.request_timeout,
            max_timeouts: self.inner.config.max_consecutive_timeouts.max(1),
        }
    }

    /// Pids of the servers currently usable, ascending.
    pub async fn live_pids(&self) -> Vec<u32> {
        let slots = self.inner.slots.lock().await;
        let mut pids: Vec<u32> = slots
            .map
            .values()
            .filter_map(|s| match s {
                Slot::Live(s) if s.child.is_usable() => Some(s.child.pid()),
                _ => None,
            })
            .collect();
        pids.sort_unstable();
        pids
    }

    /// Stop every server and wait for each reap. The daemon's exit hook: it
    /// runs on idle exit and on SIGTERM, from `serve_with_idle`.
    ///
    /// Test: `daemon_idle_exit_kills_the_child`, `shutdown_all_kills_a_child_that_ignores_exit`.
    pub async fn shutdown_all(&self) {
        let sessions: Vec<Arc<Session>> = {
            let mut slots = self.inner.slots.lock().await;
            slots.sweeping = false;
            slots
                .map
                .drain()
                .filter_map(|(_, s)| match s {
                    Slot::Live(s) => Some(s),
                    Slot::Failed { .. } => None,
                })
                .collect()
        };
        if let Some(h) = lock(&self.inner.sweeper).take() {
            h.abort();
        }
        let grace = self.inner.config.shutdown_grace;
        futures::future::join_all(sessions.iter().map(|s| s.child.shutdown(grace))).await;
    }
}

/// Kill servers idle past the window. Exits when no usable server remains or
/// the manager is gone; the next spawn starts a new sweeper.
async fn sweep(inner: Weak<Inner>) {
    loop {
        let tick = match inner.upgrade() {
            Some(i) => (i.config.idle_timeout / 4)
                .clamp(Duration::from_millis(10), Duration::from_secs(30)),
            None => return,
        };
        tokio::time::sleep(tick).await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let config = &inner.config;
        let (expired, usable) = {
            let mut slots = inner.slots.lock().await;
            // #9078: retire dead or hung servers here too, so a slot never
            // stays Live once its child cannot serve.
            let mut retired = Vec::new();
            for slot in slots.map.values_mut() {
                if let Slot::Live(s) = slot {
                    if !s.child.is_usable() {
                        let dead = Arc::clone(s);
                        *slot = Slot::Failed {
                            error: dead.child.failure(),
                            at: Instant::now(),
                        };
                        retired.push(dead);
                    }
                }
            }
            let idle: Vec<Key> = slots
                .map
                .iter()
                .filter_map(|(k, s)| match s {
                    Slot::Live(s)
                        if s.child.is_usable()
                            && s.in_flight.load(Ordering::SeqCst) == 0
                            && s.idle_for() >= config.idle_timeout =>
                    {
                        Some(k.clone())
                    }
                    Slot::Failed { at, .. } if at.elapsed() >= config.failure_cooloff => {
                        Some(k.clone())
                    }
                    _ => None,
                })
                .collect();
            let mut expired: Vec<Arc<Session>> = idle
                .iter()
                .filter_map(|k| match slots.map.remove(k) {
                    Some(Slot::Live(s)) => Some(s),
                    _ => None,
                })
                .collect();
            expired.extend(retired);
            let usable = slots
                .map
                .values()
                .any(|s| matches!(s, Slot::Live(s) if s.child.is_usable()));
            if !usable {
                slots.sweeping = false;
            }
            (expired, usable)
        };
        for s in &expired {
            tracing::info!(
                pid = s.child.pid(),
                "language server idle or dead; stopping it"
            );
            s.child.shutdown(config.shutdown_grace).await;
        }
        if !usable {
            return;
        }
    }
}

/// The workspace root DOC-72 §2 assigns to `path`: `index_root` when it
/// contains `path`, else the nearest ancestor holding `.git`.
///
/// # Errors
/// [`LspError::Document`] when `path` does not exist or no root contains it;
/// there is no guessed fallback.
///
/// Test: `workspace_root_prefers_the_index_root_then_the_git_toplevel`.
pub fn resolve_workspace_root(path: &Path, index_root: Option<&Path>) -> Result<PathBuf, LspError> {
    let doc = |reason: String| LspError::Document {
        path: path.display().to_string(),
        reason,
    };
    let path = std::fs::canonicalize(path).map_err(|e| doc(e.to_string()))?;
    if let Some(index_root) = index_root.and_then(|r| std::fs::canonicalize(r).ok()) {
        if path.starts_with(&index_root) {
            return Ok(index_root);
        }
    }
    path.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
        .ok_or_else(|| doc("no workspace root: not under the index root or a git work tree".into()))
}
