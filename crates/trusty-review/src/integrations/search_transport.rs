//! Which leg reaches trusty-search: its Unix socket or its HTTP listener (#9214).
//!
//! Why: ADR-0032 retires trusty-search's HTTP listener, and phase C of #9214
//! deletes it. Every client here must have a socket path BEFORE that happens,
//! and none may lose HTTP until it does — so for phase B both legs work and
//! one rule picks between them.
//! What: [`SearchTransport::resolve`] applies the #9214 precedence:
//! 1. `TRUSTY_SEARCH_SOCKET` set and non-empty → that socket;
//! 2. an explicit URL (`TRUSTY_SEARCH_URL` set, or a `search_url` that is not
//!    the default) → HTTP on that URL;
//! 3. otherwise the default socket when its file exists, else HTTP on the
//!    configured URL. The default socket follows the daemon's own rule:
//!    `<TRUSTY_DATA_DIR>/trusty-search.sock` when `TRUSTY_DATA_DIR` is set,
//!    else `search_rpc::search_socket()`.
//!
//! A socket file whose daemon is dead is a dial error on the socket leg. It
//! never falls back to HTTP: a silent fallback would hide a split-brain.
//! `call_socket` is the socket leg's one call, over
//! `trusty_common::search_rpc::call_at`; it maps the daemon's JSON-RPC codes
//! onto the same [`SearchClientError`] values the HTTP leg produces, so every
//! caller's error handling is unchanged.
//!
//! Test: `search_transport_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

use serde_json::Value;
use trusty_common::daemon_guard::DaemonAddrLayout;
use trusty_common::search_rpc::{self, SearchRpcError, TRUSTY_SEARCH_SOCKET_ENV};

use super::search_client::SearchClientError;

/// The env var that pins an explicit HTTP URL.
pub const TRUSTY_SEARCH_URL_ENV: &str = "TRUSTY_SEARCH_URL"; // #9214 phase C: delete

/// `ReviewConfig::search_url` when nothing sets it.
pub(crate) const DEFAULT_SEARCH_URL: &str = "http://localhost:7878"; // #9214 phase C: delete

// #9214: switch to trusty_common constants after B1
/// The daemon's "no such index / entry point" code (HTTP 404).
pub(crate) const CODE_NOT_FOUND: i64 = -32004;
// #9214: switch to trusty_common constants after B1
/// The daemon's retryable "unavailable" code (HTTP 503).
pub(crate) const CODE_UNAVAILABLE: i64 = -32002;
// #9214: switch to trusty_common constants after B1
/// The daemon's permanent "unavailable" code (HTTP 503, `retryable: false`).
pub(crate) const CODE_UNAVAILABLE_PERMANENT: i64 = -32012;

// #9214: switch to trusty_common constants after B1
/// `POST /indexes/{id}/search`'s socket twin; params `{index_id, body}`.
pub(crate) const METHOD_QUERY: &str = "search.query";
// #9214: switch to trusty_common constants after B1
/// `GET /indexes/{id}/call_chain`'s socket twin; the result is a bare string.
pub(crate) const METHOD_CALL_CHAIN: &str = "search.call_chain";

/// How a trusty-search client reaches the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchTransport {
    /// JSON-RPC over the daemon's Unix socket.
    Socket(PathBuf),
    /// The HTTP listener at this base URL, no trailing slash.
    Http(String), // #9214 phase C: delete
}

impl SearchTransport {
    /// Resolve the transport for a review's configured trusty-search.
    ///
    /// Why: one rule for every review-pipeline client, so a probe and the
    /// search it gates never talk to two different daemons.
    /// What: the module-doc precedence, with `config.search_url` as the HTTP
    /// URL. The URL counts as explicit when `TRUSTY_SEARCH_URL` is set or when
    /// `search_url` is not the default `http://localhost:7878`. Logs the leg
    /// once per process.
    /// Limitation: a `search_url` equal to the default counts as not explicit,
    /// so pinning HTTP to the default address needs `TRUSTY_SEARCH_URL` set.
    /// Test: `socket_is_used_when_present`, `http_is_used_when_the_socket_is_absent`,
    /// `socket_overrides_a_config_url_that_is_not_explicit`,
    /// `explicit_socket_env_beats_explicit_url`.
    #[must_use]
    pub fn resolve(config: &crate::config::ReviewConfig) -> Self {
        let url = config.search_url.trim_end_matches('/').to_string();
        let explicit = url_env_is_set() || url != DEFAULT_SEARCH_URL; // #9214 phase C: delete
        Self::resolve_with(explicit, || url)
    }

    /// Resolve the transport for the daemon trusty-search itself advertises.
    ///
    /// Why: the report pass addresses the daemon the audit indexed, not the
    /// review config's URL; its HTTP leg resolves through [`DaemonAddrLayout`].
    /// What: the same precedence; explicit only when `TRUSTY_SEARCH_URL` is set,
    /// and the HTTP leg keeps the pre-#9214 `DaemonAddrLayout` address.
    /// Test: `trace_entry_node_and_usages_go_over_the_socket` covers the leg it
    /// feeds; the precedence is `resolve`'s.
    #[must_use]
    pub fn resolve_advertised() -> Self {
        Self::resolve_with(url_env_is_set(), || {
            DaemonAddrLayout::TRUSTY_SEARCH.resolve_base_url() // #9214 phase C: delete
        })
    }

    /// The precedence itself; `http_url` runs only when HTTP is chosen.
    fn resolve_with(url_explicit: bool, http_url: impl FnOnce() -> String) -> Self {
        let chosen = if let Some(pinned) = socket_env() {
            Self::Socket(pinned)
        } else if url_explicit {
            Self::Http(http_url()) // #9214 phase C: delete
        } else {
            match default_socket() {
                Some(path) => Self::Socket(path),
                None => Self::Http(http_url()), // #9214 phase C: delete
            }
        };
        log_once(&chosen);
        chosen
    }

    /// Name the leg for an error or log line: `socket <path>` or the URL.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Socket(path) => format!("socket {}", path.display()),
            Self::Http(url) => url.clone(), // #9214 phase C: delete
        }
    }

    /// The socket path, when this is the socket leg.
    #[must_use]
    pub fn socket_path(&self) -> Option<&Path> {
        match self {
            Self::Socket(path) => Some(path),
            Self::Http(_) => None, // #9214 phase C: delete
        }
    }

    /// The env var and value a child process reads to reach the same daemon.
    ///
    /// Why: `trusty-analyze review` runs as a child and must use the leg this
    /// process resolved, not re-resolve from an environment that may differ.
    /// What: `TRUSTY_SEARCH_SOCKET=<path>` or `TRUSTY_SEARCH_URL=<url>`.
    /// Test: `analyze_child_env_carries_the_resolved_transport`.
    #[must_use]
    pub fn child_env(&self) -> (&'static str, String) {
        match self {
            Self::Socket(path) => (TRUSTY_SEARCH_SOCKET_ENV, path.display().to_string()),
            Self::Http(url) => (TRUSTY_SEARCH_URL_ENV, url.clone()), // #9214 phase C: delete
        }
    }
}

/// Rule 3's socket: the default path, when its file exists.
///
/// #9214: a unit-test build never reads the operator's real path. Unless a test
/// isolated the data dir with `TRUSTY_DATA_DIR_OVERRIDE` or `TRUSTY_DATA_DIR`,
/// it gets `hermetic_socket()`, a path that never exists, so no test dials or
/// stats the live daemon's socket.
/// Test: `unit_tests_never_resolve_the_real_default_socket`.
fn default_socket() -> Option<PathBuf> {
    #[cfg(test)]
    if std::env::var_os(trusty_common::DATA_DIR_OVERRIDE_ENV).is_none()
        && isolated_data_dir().is_none()
    {
        return Some(hermetic_socket());
    }
    default_socket_path().filter(|path| path.exists())
}

/// The path the trusty-search daemon binds, by its own rule.
///
/// Why: an instance isolated with `TRUSTY_DATA_DIR` binds
/// `<TRUSTY_DATA_DIR>/trusty-search.sock` (trusty-search
/// `service::socket::resolve_socket_path`, #7801), but
/// `search_rpc::search_socket()` honours only `TRUSTY_DATA_DIR_OVERRIDE`. Without
/// this branch an isolated report pass would read the production daemon.
/// What: `<TRUSTY_DATA_DIR>/trusty-search.sock` when that var is non-empty; `None`
/// when it is relative, which the daemon refuses, so no isolated socket exists;
/// otherwise the shared derivation.
/// Test: `trusty_data_dir_isolates_the_default_socket`.
fn default_socket_path() -> Option<PathBuf> {
    // #9214: drop once B1 makes search_rpc::search_socket() honour TRUSTY_DATA_DIR
    if let Some(dir) = isolated_data_dir() {
        return dir.is_absolute().then(|| dir.join(SEARCH_SOCKET_FILE)); // #9214 B1: delete
    }
    search_rpc::search_socket().ok()
}

// #9214: drop once B1 makes search_rpc::search_socket() honour TRUSTY_DATA_DIR
/// The env var that isolates one trusty-search instance (the daemon's own).
pub(crate) const TRUSTY_DATA_DIR_ENV: &str = "TRUSTY_DATA_DIR";

// #9214: drop once B1 makes search_rpc::search_socket() honour TRUSTY_DATA_DIR
/// The basename the daemon joins onto `TRUSTY_DATA_DIR`.
const SEARCH_SOCKET_FILE: &str = "trusty-search.sock";

/// `TRUSTY_DATA_DIR`, when set and non-empty (the daemon's own test).
fn isolated_data_dir() -> Option<PathBuf> {
    std::env::var_os(TRUSTY_DATA_DIR_ENV)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// A per-process socket path under the temp dir that nothing ever binds.
#[cfg(test)]
pub(crate) fn hermetic_socket() -> PathBuf {
    std::env::temp_dir().join(format!(
        "trusty-review-no-search-{}.sock",
        std::process::id()
    ))
}

/// `TRUSTY_SEARCH_SOCKET`, when set and non-empty.
fn socket_env() -> Option<PathBuf> {
    std::env::var(TRUSTY_SEARCH_SOCKET_ENV)
        .ok()
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
        .map(PathBuf::from)
}

/// Whether `TRUSTY_SEARCH_URL` is set and non-empty.
fn url_env_is_set() -> bool {
    std::env::var(TRUSTY_SEARCH_URL_ENV) // #9214 phase C: delete
        .is_ok_and(|v| !v.trim().is_empty()) // #9214 phase C: delete
}

/// Log the chosen leg once per process.
fn log_once(chosen: &SearchTransport) {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
        let leg = match chosen {
            SearchTransport::Socket(_) => "socket",
            SearchTransport::Http(_) => "http", // #9214 phase C: delete
        };
        tracing::info!(transport = leg, target = %chosen.describe(), "trusty-search transport");
    });
}

/// Call one method on the daemon at `socket`, with the HTTP leg's error shape.
///
/// Why: callers branch on `SearchClientError` (`is_unknown_index`, the 503
/// degradation), and the socket leg must not change what they see.
/// What: `-32004` → `Api{404, message}`; `-32002`/`-32012` → `Api{503, data}`
/// (the 503 body, or the message when the daemon sent no data); any other
/// daemon code → `Api{500}`; a dial or frame failure → `Transport`, as an HTTP
/// connect failure is.
/// Test: `rpc_32002_maps_to_the_http_503_error`, `rpc_32012_maps_to_503_too`,
/// `rpc_32004_maps_to_the_http_404_error`,
/// `dead_socket_file_does_not_fall_back_to_http`.
pub(crate) async fn call_socket(
    socket: &Path,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, SearchClientError> {
    search_rpc::call_at(socket, method, params, timeout)
        .await
        .map_err(map_rpc_error)
}

/// Map a `call_at` failure onto [`SearchClientError`]; see [`call_socket`].
fn map_rpc_error(err: anyhow::Error) -> SearchClientError {
    let Some(rpc) = err.downcast_ref::<SearchRpcError>() else {
        return SearchClientError::Transport(format!("{err:#}"));
    };
    match rpc.code {
        CODE_NOT_FOUND => SearchClientError::Api {
            status: 404,
            body: rpc.message.clone(),
        },
        CODE_UNAVAILABLE | CODE_UNAVAILABLE_PERMANENT => SearchClientError::Api {
            status: 503,
            body: rpc
                .data
                .as_ref()
                .map_or_else(|| rpc.message.clone(), Value::to_string),
        },
        _ => SearchClientError::Api {
            status: 500,
            body: rpc.to_string(),
        },
    }
}

/// Decode a socket result into the HTTP body type it mirrors.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    value: Value,
    what: &str,
) -> Result<T, SearchClientError> {
    serde_json::from_value(value).map_err(|e| SearchClientError::Parse(format!("{what}: {e}")))
}

#[cfg(test)]
#[path = "search_transport_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "search_socket_fixture.rs"]
pub(crate) mod fixture;
