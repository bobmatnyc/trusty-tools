//! The one client for the trusty-mpm daemon's `mpm.residency.active` method
//! (#7087 slice 1a).
//!
//! Why: trusty-memory and trusty-search both need the same "derive the
//! socket, send one JSON-RPC frame, decode the typed answer" pair that
//! `memory_rpc` and `trusty-mpm`'s own `search_rpc`
//! (`crates/trusty-mpm/src/daemon/search_rpc.rs`) already exist for their
//! daemons. A second bespoke client here is exactly the drift the workspace's
//! common-entry-point rule exists to prevent.
//!
//! What: [`crate::mpm_rpc::mpm_socket`] resolves the daemon's socket the same fail-open way
//! `memory_rpc::resolve_memory_socket_or_unreachable` does — a caller whose
//! data directory cannot be resolved gets a path nothing serves rather than
//! an error to propagate, so a dead-daemon call and an
//! unresolvable-data-directory call surface identically, as an ordinary
//! [`crate::uds::UdsRpcError::Dial`]. [`crate::mpm_rpc::fetch_active_projects`] /
//! [`crate::mpm_rpc::fetch_active_projects_at`] send one `mpm.residency.active` frame over
//! [`crate::uds::send_framed_request`] and decode the daemon's `result` into
//! [`crate::residency::ActiveProjectSet`] (re-exported from
//! [`crate::residency`], which owns the wire types so a consumer that only
//! wants [`crate::residency::ResidencySnapshot`] is not forced to compile in
//! this module's `uds` dependency).
//!
//! **Errors are [`crate::uds::UdsRpcError`], not a bespoke `MpmRpcError`.**
//! Every failure this method can produce collapses onto the transport
//! vocabulary: a dead or unreachable socket is
//! [`crate::uds::UdsRpcError::Dial`] straight from
//! [`crate::uds::send_framed_request`]; a `result` that does not decode into
//! [`crate::residency::ActiveProjectSet`], or an explicit JSON-RPC `error`
//! half (the method takes no params, so the daemon has nothing to reject one
//! for short of an internal fault), both become
//! [`crate::uds::UdsRpcError::Decode`] — there is no "application refused the
//! call" case worth a caller distinguishing here, so reusing the transport's
//! own decode variant is more honest than adding a parallel error type only
//! `memory_rpc::MemoryRpcError` and `search_rpc::SearchRpcError` needed
//! because THEY carry a daemon-specific `code` a caller inspects
//! (`is_not_found`).
//!
//! Test: `mpm_socket_honours_the_env_override`,
//! `fetch_active_projects_at_round_trips_a_set`,
//! `fetch_active_projects_at_reports_a_dead_socket_rather_than_hanging`,
//! `fetch_active_projects_at_reports_a_malformed_payload`.

// #7087

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::Error as _;
use serde_json::json;

pub use crate::residency::{ACTIVE_PROJECT_SET_SCHEMA, ActiveProject, ActiveProjectSet};
use crate::uds::UdsRpcError;
use crate::uds::send_framed_request;
use crate::uds::server::{JSONRPC_VERSION, RpcResponse};

/// Environment variable that pins the daemon's socket path explicitly.
///
/// Same purpose as `crate::memory_rpc::TRUSTY_MEMORY_SOCKET_ENV` and
/// `search_rpc::TRUSTY_SEARCH_SOCKET_ENV`: a test rig or a CI job points a
/// caller at a daemon it started on a temp path, without redirecting every
/// other trusty-* client in the process through
/// `TRUSTY_DATA_DIR_OVERRIDE`.
///
/// Test: `mpm_socket_honours_the_env_override`.
pub const TRUSTY_MPM_SOCKET_ENV: &str = "TRUSTY_MPM_SOCKET";

/// The app name trusty-mpm derives its socket path under.
///
/// Matches the daemon's own `daemon_socket_path("trusty-mpm")` call, which is
/// why caller and daemon compute the same path with nothing published
/// between them.
const MPM_APP_NAME: &str = "trusty-mpm";

/// A path nothing can be serving, for the fail-open branch of [`mpm_socket`].
///
/// Mirrors `memory_rpc`'s own placeholder: a dial against a path under a
/// directory that cannot exist is refused by the kernel immediately, which is
/// what lets a caller whose data directory is unresolvable fail as fast as one
/// whose daemon is simply not running.
const UNREACHABLE_PLACEHOLDER: &str = "/nonexistent/trusty-mpm/trusty-mpm.sock";

/// The one method this module calls: no params, answers an [`ActiveProjectSet`].
pub const METHOD_RESIDENCY_ACTIVE: &str = "mpm.residency.active";

/// Resolve the socket the trusty-mpm daemon binds.
///
/// Fail-open, deliberately — every caller of this module degrades to "treat
/// the producer as unreachable" rather than aborting, so there is no
/// operator-actionable distinction here worth returning a `Result` for. On the
/// rare failure to resolve or create the data directory, this warns on stderr
/// and returns [`UNREACHABLE_PLACEHOLDER`] instead.
///
/// Test: `mpm_socket_honours_the_env_override`.
pub fn mpm_socket() -> PathBuf {
    if let Ok(raw) = std::env::var(TRUSTY_MPM_SOCKET_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    crate::daemon_addr::daemon_socket_path(MPM_APP_NAME).unwrap_or_else(|e| {
        eprintln!("trusty-mpm: {e}");
        PathBuf::from(UNREACHABLE_PLACEHOLDER)
    })
}

/// Pull the active-project set from the trusty-mpm daemon at its resolved
/// socket.
///
/// # Errors
///
/// As [`fetch_active_projects_at`].
pub async fn fetch_active_projects(timeout: Duration) -> Result<ActiveProjectSet, UdsRpcError> {
    fetch_active_projects_at(&mpm_socket(), timeout).await
}

/// [`fetch_active_projects`] against an already-resolved socket.
///
/// Why a separate entry point: a caller that resolved the socket once and
/// threads it through (a boot-time pull, or a test driving a daemon it started
/// itself on a temp path) should not re-resolve — same reason
/// `memory_rpc::call_memory_tool_at` exists beside
/// `memory_rpc::call_memory_tool`.
///
/// # Errors
///
/// [`UdsRpcError::Dial`] (including a socket nothing is serving),
/// [`UdsRpcError::Timeout`], or another transport variant from
/// [`crate::uds::send_framed_request`] when the exchange itself fails;
/// [`UdsRpcError::Decode`] when the daemon's `result` does not decode into
/// [`ActiveProjectSet`], or when the daemon answers with a JSON-RPC `error`
/// (see the module doc for why that collapses onto `Decode` rather than a
/// bespoke variant).
///
/// Test: `fetch_active_projects_at_round_trips_a_set`,
/// `fetch_active_projects_at_reports_a_dead_socket_rather_than_hanging`,
/// `fetch_active_projects_at_reports_a_malformed_payload`.
pub async fn fetch_active_projects_at(
    socket: &Path,
    timeout: Duration,
) -> Result<ActiveProjectSet, UdsRpcError> {
    let request = json!({
        "jsonrpc": JSONRPC_VERSION,
        "id": 1,
        "method": METHOD_RESIDENCY_ACTIVE,
        "params": {},
    });

    let response: RpcResponse = send_framed_request(socket, &request, timeout).await?;
    decode_active_project_set(socket, response)
}

/// Turn one [`RpcResponse`] into the typed answer or an [`UdsRpcError`].
fn decode_active_project_set(
    socket: &Path,
    response: RpcResponse,
) -> Result<ActiveProjectSet, UdsRpcError> {
    match (response.result, response.error) {
        (Some(result), _) => serde_json::from_value(result).map_err(|source| UdsRpcError::Decode {
            path: socket.to_path_buf(),
            source,
        }),
        (None, Some(e)) => Err(UdsRpcError::Decode {
            path: socket.to_path_buf(),
            source: serde_json::Error::custom(format!(
                "{METHOD_RESIDENCY_ACTIVE} failed: {} ({})",
                e.message, e.code
            )),
        }),
        // The server's own contract is that exactly one of the two is present.
        (None, None) => Err(UdsRpcError::Decode {
            path: socket.to_path_buf(),
            source: serde_json::Error::custom(format!(
                "{METHOD_RESIDENCY_ACTIVE} answered with neither a result nor an error"
            )),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;
    use tokio::sync::oneshot;

    use super::*;
    use crate::data_dir::ENV_LOCK;
    use crate::uds::server::{RpcRouter, RpcServeOptions, serve_until};

    /// A socket serving `mpm.residency.active` with a caller-supplied result.
    ///
    /// Dropping the returned guard stops the accept loop.
    struct FakeProducer {
        socket: PathBuf,
        _dir: TempDir,
        stop: Option<oneshot::Sender<()>>,
    }

    impl Drop for FakeProducer {
        fn drop(&mut self) {
            if let Some(tx) = self.stop.take() {
                let _ = tx.send(());
            }
        }
    }

    /// Serve `result` verbatim as `mpm.residency.active`'s JSON-RPC result.
    async fn fake_producer(result: serde_json::Value) -> FakeProducer {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("trusty-mpm.sock");
        let listener = crate::uds::bind_hardened(&socket).expect("bind");

        let router = RpcRouter::new().typed::<serde_json::Value, serde_json::Value, _, _>(
            METHOD_RESIDENCY_ACTIVE,
            move |_params: serde_json::Value| {
                let result = result.clone();
                async move { Ok(result) }
            },
        );

        let (stop, shutdown) = oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_until(
                &listener,
                Arc::new(router),
                RpcServeOptions::default(),
                async {
                    let _ = shutdown.await;
                },
            )
            .await;
        });

        FakeProducer {
            socket,
            _dir: dir,
            stop: Some(stop),
        }
    }

    fn sample_set() -> ActiveProjectSet {
        ActiveProjectSet {
            schema: ACTIVE_PROJECT_SET_SCHEMA,
            generation: 7,
            published_at_unix: 1_700_000_000,
            projects: vec![ActiveProject {
                root: PathBuf::from("/repo"),
                palace_id: Some("palace-1".to_string()),
                index_ids: vec!["idx-1".to_string()],
                session_ids: vec!["sess-1".to_string()],
                last_activity_unix: Some(1_700_000_500),
            }],
        }
    }

    /// Why: the override is the only way a test or a CI job can point this
    /// client at a daemon it started itself without redirecting every other
    /// trusty-* client in the process through `TRUSTY_DATA_DIR_OVERRIDE`.
    /// Test: itself.
    #[test]
    fn mpm_socket_honours_the_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(TRUSTY_MPM_SOCKET_ENV, "/tmp/example-mpm.sock");
        }
        let resolved = mpm_socket();
        unsafe {
            std::env::remove_var(TRUSTY_MPM_SOCKET_ENV);
        }
        assert_eq!(resolved, PathBuf::from("/tmp/example-mpm.sock"));
    }

    /// Why: the whole point of a shared wire contract is that what the
    /// producer serializes is what a caller reads back unchanged.
    /// Test: itself.
    #[tokio::test]
    async fn fetch_active_projects_at_round_trips_a_set() {
        let set = sample_set();
        let daemon = fake_producer(serde_json::to_value(&set).expect("serialize")).await;

        let fetched = fetch_active_projects_at(&daemon.socket, Duration::from_secs(5))
            .await
            .expect("the daemon answers");

        assert_eq!(fetched, set);
    }

    /// Why: every fail-open caller degrades on "the daemon is not running",
    /// and has to learn that promptly rather than by waiting out a timeout.
    /// Test: itself.
    #[tokio::test]
    async fn fetch_active_projects_at_reports_a_dead_socket_rather_than_hanging() {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("absent.sock");
        let started = std::time::Instant::now();

        let err = fetch_active_projects_at(&socket, Duration::from_secs(5))
            .await
            .expect_err("nothing is listening");

        assert!(
            matches!(err, UdsRpcError::Dial { .. }),
            "a refused dial is a Dial error, not something else: {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a refused dial must not wait out the budget: {:?}",
            started.elapsed()
        );
    }

    /// Why: a payload that does not decode into `ActiveProjectSet` must be
    /// reported, never silently coerced into an empty set — an empty set reads
    /// as "trusty-mpm says nothing is active" and would evict every resident
    /// project.
    /// Test: itself.
    #[tokio::test]
    async fn fetch_active_projects_at_reports_a_malformed_payload() {
        let daemon = fake_producer(serde_json::json!("not an active project set")).await;

        let err = fetch_active_projects_at(&daemon.socket, Duration::from_secs(5))
            .await
            .expect_err("a bare string does not decode into ActiveProjectSet");

        assert!(
            matches!(err, UdsRpcError::Decode { .. }),
            "a malformed payload is a Decode error, not something else: {err:?}"
        );
    }
}
