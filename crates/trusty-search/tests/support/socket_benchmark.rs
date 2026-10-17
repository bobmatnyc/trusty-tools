//! Socket fixture for the live baseline suite (#9993).
//!
//! Why: the daemon has served only its Unix socket since #9214, so the
//! baseline suite's HTTP requests had nothing to reach. The suite now talks to
//! a sandbox daemon (`scripts/sandbox_search_daemon.sh`) over that socket,
//! through the crate's own [`DaemonClient`].
//! What: three required environment variables, none with a default:
//! - `TRUSTY_SEARCH_BENCH_SOCKET` — absolute path of the sandbox daemon's
//!   socket (`<sandbox dir>/data/trusty-search.sock`). Refused when it is the
//!   live install's default socket.
//! - `TRUSTY_SEARCH_BENCH_INDEX` — the index id every call names.
//! - `TRUSTY_SEARCH_TEST_CORPUS_ROOT` — the marked disposable corpus copy, as
//!   validated by `isolated_benchmark::validate_corpus`.
//!
//! [`fixture`] fails closed: an unset variable, or no daemon answering
//! `search.health` at the named socket, panics with a message naming it. Every
//! index call goes through [`Fixture::index_call`], which first reads the
//! index's `root_path` and refuses unless it equals the canonical corpus path.
//! Test: `a_missing_daemon_fails_closed_naming_the_socket`,
//! `socket_env_is_required_absolute_and_not_the_live_default`,
//! `an_unresolvable_live_socket_refuses_instead_of_skipping_the_check`,
//! `index_id_is_required`.

#![allow(dead_code)] // Each test binary uses a different subset.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};

use super::isolated_benchmark;

/// The sandbox daemon's socket path. No default.
pub const SOCKET_ENV: &str = "TRUSTY_SEARCH_BENCH_SOCKET";
/// The index id the suite reads. No default.
pub const INDEX_ENV: &str = "TRUSTY_SEARCH_BENCH_INDEX";

/// Per-call budget: generous for a warm query, bounded so a hung daemon fails.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A connected sandbox daemon, the index it serves, and that index's corpus.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// A client on the named socket; it has answered `search.health`.
    pub client: DaemonClient,
    /// The index id every call names.
    pub index: String,
    /// The canonical corpus root the index must be registered to.
    pub corpus: PathBuf,
}

/// Accept only an explicit, absolute socket path that is not the live default.
///
/// Why: the suite must never reach the installed daemon by default (#9993).
/// What: refuses an unset or empty value, a relative path, and a path equal
/// to `live` (compared after canonicalizing each parent directory). `live` is
/// a resolved path, never an `Option`, so the comparison cannot be skipped.
/// Test: `socket_env_is_required_absolute_and_not_the_live_default`.
pub fn validate_socket(raw: Option<&OsStr>, live: &Path) -> Result<PathBuf, String> {
    let raw = raw
        .map(|v| v.to_string_lossy().trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "set {SOCKET_ENV} to the sandbox daemon's socket \
                 (<dir>/data/trusty-search.sock from scripts/sandbox_search_daemon.sh); \
                 there is no default"
            )
        })?;
    let socket = PathBuf::from(raw);
    if !socket.is_absolute() {
        return Err(format!(
            "{SOCKET_ENV} must be absolute, got {}",
            socket.display()
        ));
    }
    if same_socket(&socket, live) {
        return Err(format!(
            "{SOCKET_ENV}={} is the live install's socket; use a sandbox daemon",
            socket.display()
        ));
    }
    Ok(socket)
}

/// Resolve the live install's default socket, or refuse naming the cause.
///
/// Why: a failure to resolve the live socket must never turn the live-default
/// refusal into a silent skip (#9993). `data_dir_override` is the argument of
/// `resolve_socket_path`; production passes `None`.
/// What: `Err("cannot resolve the live socket to refuse it: ...")` on failure.
/// Test: `an_unresolvable_live_socket_refuses_instead_of_skipping_the_check`.
pub fn resolve_live_socket(data_dir_override: Option<&OsStr>) -> Result<PathBuf, String> {
    trusty_search::service::socket::resolve_socket_path(data_dir_override)
        .map_err(|e| format!("cannot resolve the live socket to refuse it: {e:#}"))
}

/// The socket the suite may dial: `raw`, validated against the live default.
///
/// What: resolves the live socket first, so a resolution failure is an `Err`.
/// Test: `an_unresolvable_live_socket_refuses_instead_of_skipping_the_check`.
pub fn resolve_suite_socket(
    raw: Option<&OsStr>,
    live_override: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let live = resolve_live_socket(live_override)?;
    validate_socket(raw, &live)
}

/// Equal file names under the same canonical parent directory.
fn same_socket(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| {
        let parent = p
            .parent()
            .map(|d| d.canonicalize().unwrap_or_else(|_| d.to_path_buf()));
        (parent, p.file_name().map(OsStr::to_os_string))
    };
    canon(a) == canon(b)
}

/// Accept only a non-empty index id.
///
/// Test: `index_id_is_required`.
pub fn validate_index_id(raw: Option<&OsStr>) -> Result<String, String> {
    raw.map(|v| v.to_string_lossy().trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("set {INDEX_ENV} to the sandbox index id; there is no default"))
}

/// Dial `socket` and require a `search.health` answer.
///
/// Why: a missing daemon must fail the suite, never skip it (#9993).
/// What: `Err` naming the socket when the health call fails.
/// Test: `a_missing_daemon_fails_closed_naming_the_socket`.
pub async fn connect(socket: &Path) -> Result<DaemonClient, String> {
    let client = DaemonClient::at(socket).with_timeout(CALL_TIMEOUT);
    client.health().await.map_err(|e| {
        format!(
            "baseline suite: no trusty-search daemon answered search.health at socket {}: {e}",
            socket.display()
        )
    })?;
    Ok(client)
}

/// Resolve and connect the fixture, panicking on any refusal.
pub async fn fixture() -> Fixture {
    let socket = resolve_suite_socket(std::env::var_os(SOCKET_ENV).as_deref(), None)
        .unwrap_or_else(|e| panic!("{e}"));
    let index =
        validate_index_id(std::env::var_os(INDEX_ENV).as_deref()).unwrap_or_else(|e| panic!("{e}"));
    let client = connect(&socket).await.unwrap_or_else(|e| panic!("{e}"));
    let corpus = isolated_benchmark::corpus_root("");
    Fixture {
        client,
        index,
        corpus,
    }
}

impl Fixture {
    /// The socket the fixture dials.
    pub fn socket(&self) -> &Path {
        self.client.socket()
    }

    /// `search.health`, unguarded: it touches no index.
    pub async fn health(&self) -> Value {
        self.client
            .health()
            .await
            .unwrap_or_else(|e| panic!("search.health at {}: {e}", self.socket().display()))
    }

    /// `search.indexes.list`, unguarded: it touches no index.
    pub async fn list_indexes(&self) -> Value {
        self.client
            .call("search.indexes.list", json!({}))
            .await
            .unwrap_or_else(|e| panic!("search.indexes.list: {e}"))
    }

    /// Read the index's status and refuse unless its root is the corpus.
    ///
    /// Why: the Architect's live-index rule (#9993) — no call reaches an
    /// index the suite was not given. A missing index is a refusal too.
    /// What: `search.index.status` for [`Self::index`], then
    /// `isolated_benchmark::validate_registered_root`, which compares the two
    /// canonical paths for equality.
    /// Test: `rejects_existing_index_registered_to_another_root` (equality).
    pub async fn guard(&self) -> Result<Value, String> {
        let status = self
            .client
            .call("search.index.status", json!({ "index_id": self.index }))
            .await
            .map_err(|e| format!("index {} status before any call: {e}", self.index))?;
        let root = status["root_path"]
            .as_str()
            .ok_or_else(|| format!("index {} status has no root_path: {status}", self.index))?;
        isolated_benchmark::validate_registered_root(Path::new(root), &self.corpus)
            .map_err(|e| format!("refusing index {}: {e}", self.index))?;
        Ok(status)
    }

    /// Guard, then call an index-scoped `method`, timing only the call.
    ///
    /// What: params are `{"index_id", "body"}` when `body` is `Some`, else
    /// `{"index_id"}`. Returns `(latency_ms, result)`; the guard runs before
    /// the clock starts.
    pub async fn index_call(
        &self,
        method: &str,
        body: Option<Value>,
    ) -> Result<(u128, Value), String> {
        self.guard().await?;
        let params = match body {
            Some(body) => json!({ "index_id": self.index, "body": body }),
            None => json!({ "index_id": self.index }),
        };
        let started = Instant::now();
        let result = self.client.call(method, params).await;
        let ms = started.elapsed().as_millis();
        result
            .map(|value| (ms, value))
            .map_err(|e: DaemonCallError| format!("{method} on index {}: {e}", self.index))
    }

    /// Guard, then require every enabled stage of the index to be ready.
    pub async fn assert_ready(&self) {
        let status = self.guard().await.unwrap_or_else(|e| panic!("{e}"));
        isolated_benchmark::validate_ready(&status).unwrap_or_else(|e| {
            panic!("benchmark prerequisite failed: {e}; wait for indexing before rerunning")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_daemon_fails_closed_naming_the_socket() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let socket = dir.path().join("trusty-search.sock");
        let error = connect(&socket)
            .await
            .expect_err("no daemon serves the socket, so connect must fail");
        assert!(
            error.contains(&socket.display().to_string()),
            "the refusal must name the socket: {error}"
        );
    }

    #[test]
    fn socket_env_is_required_absolute_and_not_the_live_default() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let live = dir.path().join("trusty-search.sock");
        assert!(validate_socket(None, &live).is_err());
        assert!(validate_socket(Some(OsStr::new("  ")), &live).is_err());
        assert!(validate_socket(Some(OsStr::new("data/ts.sock")), &live).is_err());
        assert!(validate_socket(Some(live.as_os_str()), &live).is_err());
        let sandbox = dir.path().join("sandbox").join("trusty-search.sock");
        assert_eq!(
            validate_socket(Some(sandbox.as_os_str()), &live),
            Ok(sandbox)
        );
    }

    #[test]
    fn an_unresolvable_live_socket_refuses_instead_of_skipping_the_check() {
        // A relative data-dir override makes `resolve_socket_path` fail without
        // touching process-global env. A valid sandbox socket must still be refused.
        let dir = tempfile::tempdir().expect("scratch dir");
        let sandbox = dir.path().join("sandbox").join("trusty-search.sock");
        let error = resolve_suite_socket(Some(sandbox.as_os_str()), Some(OsStr::new("relative")))
            .expect_err("an unresolvable live socket must refuse, not skip the check");
        assert!(
            error.contains("cannot resolve the live socket to refuse it"),
            "the refusal must name the cause: {error}"
        );
    }

    #[test]
    fn index_id_is_required() {
        assert!(validate_index_id(None).is_err());
        assert!(validate_index_id(Some(OsStr::new(""))).is_err());
        assert_eq!(
            validate_index_id(Some(OsStr::new("perf-abc"))),
            Ok("perf-abc".to_string())
        );
    }
}
