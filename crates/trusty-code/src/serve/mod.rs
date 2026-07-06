//! `tcode serve` daemon: STDIO + HTTP JSON-RPC transports + proof-of-life,
//! `session.*`, and `task.*` methods (#2053, #2054, #2056, M1 control-plane
//! cut line).
//!
//! Why: this module is the foundation the `tcode serve` binary subcommand
//! delegates to. It owns assembling the [`crate::jsonrpc::Router`] (and,
//! since #2054, the [`crate::session::SessionRegistry`] every `session.*`/
//! `task.*` method shares) and entering whichever transport loop was
//! requested; `main.rs` stays a thin clap-to-handler shim, matching the rest
//! of the crate's CLI wiring.
//! What: [`build_router`] registers every currently-known method group
//! (today: [`methods::register`]'s `ping`/`health` proof-of-life pair,
//! `crate::session::protocol::register`'s seven `session.*` methods, and
//! (#2056) `crate::task::protocol::register`'s `task.run` — later tickets add
//! `harness.describe` #2066 here, one line each). [`run_stdio`] and
//! [`run_http`] both build a router + registry via `build_router` and hand
//! them to their respective transport ([`transport::run_stdio_loop`] /
//! [`http::run_http`]) — every method behaves identically over either
//! transport because both dispatch through the same `Router` against the
//! same `SessionRegistry`. Both also call
//! `SessionRegistry::shutdown_executions` before returning, so a graceful
//! stop (SIGTERM/stdin EOF) gives any in-flight `task.run` execution a
//! bounded chance to unwind — the same connection-safe-restart discipline
//! (issue #534) this crate's daemons already apply to connections, now
//! extended to background tasks.
//!
//! What's NOT here yet: the transports are independent modes (`--stdio` xor
//! `--http`), not simultaneous; running both at once would need a shared
//! `Arc<Router>`/`Arc<SessionRegistry>` across two concurrently-spawned
//! tasks, which no current ticket requires (the registry is already `Arc`,
//! so this is a small extension when it's needed).
//!
//! Test: `serve::tests::*`.

pub mod http;
pub mod methods;
pub mod transport;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::info;

use crate::agents::locate_agents_dir;
use crate::jsonrpc::Router;
use crate::session::SessionRegistry;

/// How long a graceful stop waits for in-flight `task.run` executions to
/// observe cancellation and unwind before the process exits anyway.
///
/// Why: bounds shutdown latency — an operator restarting the daemon should
/// not wait indefinitely on a stuck tool call; best-effort, not a hard
/// guarantee (see `SessionRegistry::shutdown_executions`'s docs).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Default TCP port for `tcode serve --http` when `--port` is omitted.
///
/// Why: the trusty-* family reserves a block of fixed local ports so
/// operators/tooling can find a daemon without a discovery file
/// (`trusty-memory` 7070, `trusty-search` 7878, `trusty-analyze` 7879,
/// `trusty-review` 7880). `7881` is the next free port in that sequence.
/// Pass `--port 0` to bind an OS-assigned ephemeral port instead (e.g. for
/// tests or running multiple instances side by side); the real bound port
/// is always logged to stderr regardless of which is used.
/// What: `7881`.
/// Test: `default_http_port_is_documented_value`.
pub const DEFAULT_HTTP_PORT: u16 = 7881;

/// Assemble the router (+ its session registry) with every method this
/// build of `tcode` knows about, scoped to `project`.
///
/// Why: single place that lists which method groups are wired in, so a new
/// ticket adding `harness.describe` touches exactly one line here plus its
/// own `register` function. Both transports (`run_stdio`, `run_http`) call
/// this so they can never drift into different method surfaces, and both
/// need the SAME `SessionRegistry` instance — `run_http` hands it separately
/// to the `GET /sessions/{id}/events` SSE route, which bypasses the `Router`
/// entirely (see `crate::serve::http` module docs). `task.run` (#2056) is
/// the first method that needs `project` for more than logging — it resolves
/// `agents_dir` from it via `crate::agents::locate_agents_dir` (the same
/// `.claude/agents` / `.open-mpm/agents` convention `tcode run-task` uses)
/// and passes both down to spawned executions.
/// What: builds an empty `Router` + a fresh `SessionRegistry`, calls
/// `methods::register`, `crate::session::protocol::register`, and
/// `crate::task::protocol::register` on them, and returns both.
/// Test: `run_stdio_router_recognises_proof_of_life_methods`,
/// `build_router_wires_session_methods`, `build_router_wires_task_run`.
pub fn build_router(project: PathBuf) -> (Router, Arc<SessionRegistry>) {
    let sessions = Arc::new(SessionRegistry::new());
    let agents_dir = locate_agents_dir(&project);
    let mut router = Router::new();
    methods::register(&mut router);
    crate::session::protocol::register(&mut router, sessions.clone());
    crate::task::protocol::register(&mut router, sessions.clone(), project, agents_dir);
    (router, sessions)
}

/// Run `tcode serve --stdio` to completion.
///
/// Why: the top-level entry point `main.rs` calls for the `serve --stdio`
/// subcommand.
/// What: builds the router, logs start/stop to stderr, and enters
/// [`transport::run_stdio_loop`], returning when it returns (stdin EOF or
/// SIGTERM/SIGINT). On return, calls `SessionRegistry::shutdown_executions`
/// (bounded by [`SHUTDOWN_GRACE`]) so an in-flight `task.run` gets a chance
/// to unwind before the process exits.
/// Test: `run_stdio_router_recognises_proof_of_life_methods` covers the
/// router assembly; the transport loop itself is covered by
/// `transport::tests`.
pub async fn run_stdio(project: PathBuf) -> Result<()> {
    info!(project = %project.display(), "tcode serve --stdio: starting");
    let (router, sessions) = build_router(project);
    transport::run_stdio_loop(router).await?;
    sessions.shutdown_executions(SHUTDOWN_GRACE).await;
    info!("tcode serve --stdio: stopped");
    Ok(())
}

/// Run `tcode serve --http` to completion.
///
/// Why: the top-level entry point `main.rs` calls for the `serve --http`
/// subcommand.
/// What: builds the router + session registry, logs start/stop to stderr,
/// and enters [`http::run_http`] bound to `port` (`0` = OS-assigned
/// ephemeral port), returning when it returns (SIGTERM/SIGINT). On return,
/// calls `SessionRegistry::shutdown_executions` (bounded by
/// [`SHUTDOWN_GRACE`]) — see [`run_stdio`]'s docs for why.
/// Test: `run_stdio_router_recognises_proof_of_life_methods` covers the
/// shared router assembly; `http::tests` cover the routing/dispatch logic.
pub async fn run_http(project: PathBuf, port: u16) -> Result<()> {
    info!(project = %project.display(), port, "tcode serve --http: starting");
    let (router, sessions) = build_router(project);
    http::run_http(router, sessions.clone(), port).await?;
    sessions.shutdown_executions(SHUTDOWN_GRACE).await;
    info!("tcode serve --http: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trusty_common::mcp::Request;

    fn test_ctx() -> crate::jsonrpc::ConnectionContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        crate::jsonrpc::ConnectionContext::new(tx)
    }

    /// `build_router` must recognise both proof-of-life methods.
    #[tokio::test]
    async fn run_stdio_router_recognises_proof_of_life_methods() {
        let project = tempfile::tempdir().expect("project tempdir");
        let (router, _sessions) = build_router(project.path().to_path_buf());
        for method in ["ping", "health"] {
            let req = Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: method.to_string(),
                params: None,
            };
            let resp = router.dispatch(req, &test_ctx()).await;
            assert!(
                resp.error.is_none(),
                "{method} must be registered, got {:?}",
                resp.error
            );
        }
    }

    /// `build_router` must also wire every `session.*` method (#2054),
    /// sharing the registry it returns.
    #[tokio::test]
    async fn build_router_wires_session_methods() {
        let project = tempfile::tempdir().expect("project tempdir");
        let (router, sessions) = build_router(project.path().to_path_buf());
        let session = sessions.create("t".to_string(), None, None);

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "session.status".to_string(),
            params: Some(json!({"session_id": session.id})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "session.status must be registered, got {:?}",
            resp.error
        );
        assert_eq!(resp.result.unwrap()["id"], session.id);
    }

    /// `DEFAULT_HTTP_PORT` must stay the documented value (7881) so a
    /// change is a deliberate edit to both the constant and its doc comment,
    /// not an accidental drift.
    #[test]
    fn default_http_port_is_documented_value() {
        assert_eq!(DEFAULT_HTTP_PORT, 7881);
    }

    /// `build_router` must also wire `task.run` (#2056).
    #[tokio::test]
    async fn build_router_wires_task_run() {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::create_dir_all(project.path().join(".claude").join("agents")).expect("mkdir");
        std::fs::write(
            project
                .path()
                .join(".claude")
                .join("agents")
                .join("pm.toml"),
            "[agent]\nname = \"pm\"\nmodel = \"openai/gpt-4o-mini\"\n[system_prompt]\ncontent = \"PM\"\n",
        )
        .expect("write pm.toml");
        std::fs::write(
            project
                .path()
                .join(".claude")
                .join("agents")
                .join("python-engineer.toml"),
            "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n[system_prompt]\ncontent = \"eng\"\n",
        )
        .expect("write python-engineer.toml");

        // SAFETY: test-only env mutation; serialized by the shared crate lock.
        let _guard = crate::task::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var(
                crate::task::mock_llm::MOCK_LLM_ENV,
                crate::task::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let (router, sessions) = build_router(project.path().to_path_buf());
        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "task.run".to_string(),
            params: Some(json!({"task_description": "say hi"})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        sessions
            .shutdown_executions(std::time::Duration::from_secs(5))
            .await;
        unsafe {
            std::env::remove_var(crate::task::mock_llm::MOCK_LLM_ENV);
        }

        assert!(
            resp.error.is_none(),
            "task.run must be registered, got {:?}",
            resp.error
        );
        assert_eq!(resp.result.unwrap()["status"], "running");
    }
}
