//! `tcode serve` daemon: STDIO + HTTP JSON-RPC transports + proof-of-life,
//! `session.*`, and `task.*` methods (#2053, #2054, #2056, #2058,
//! M1 control-plane cut line).
//!
//! Why: this module is the foundation the `tcode serve` binary subcommand
//! delegates to. It owns assembling the [`crate::jsonrpc::Router`] (and,
//! since #2054, the [`crate::session::SessionRegistry`] every `session.*`/
//! `task.*` method shares) and entering whichever transport loop was
//! requested; `main.rs` stays a thin clap-to-handler shim, matching the rest
//! of the crate's CLI wiring.
//! What: [`build_router`] registers every currently-known method group
//! (today: [`methods::register`]'s `ping`/`health` proof-of-life pair,
//! `crate::session::protocol::register`'s eight `session.*` methods
//! (including #2058's read-only `session.get_transcript`), and (#2056)
//! `crate::task::protocol::register`'s `task.run` — later tickets add
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

pub mod discovery;
pub mod http;
pub mod methods;
pub mod rest;
pub mod transport;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::info;

use crate::binding::ProjectBinding;
use crate::jsonrpc::Router;
use crate::session::SessionRegistry;
use crate::workstreams::{SharedWorkstreamStore, WorkstreamStore};

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
/// `trusty-mpm` daemon 7880, `trusty-review` 7891, `trusty-embedderd`
/// `--http` mode 7890). This constant previously reused `7881`, which turned
/// out to collide with `trusty-mpm`'s supervisor metrics listener
/// (`trusty_mpm::supervisor::config::DEFAULT_METRICS_ADDR`,
/// `crates/trusty-mpm/src/supervisor/config.rs`) — the two defaults were
/// picked independently and nothing pinned them apart, so a fresh install
/// with both `tm supervisor` and `tcode serve` running answered `/health` on
/// `tcode`'s port with the supervisor's generic `{"status":"ok"}`, masking a
/// real 404 for every other route (#3364). `7882` is verified free against
/// the full known-sibling table in
/// `docs/architecture/port-assignments.md` and matches the session-local
/// workaround used to confirm the fix (`tcode serve --http --port 7882`).
/// Pass `--port 0` to bind an OS-assigned ephemeral port instead (e.g. for
/// tests or running multiple instances side by side); the real bound port
/// is always logged to stderr regardless of which is used.
/// What: `7882`.
/// Test: `default_http_port_is_documented_value`,
/// `default_http_port_does_not_collide_with_known_siblings`.
pub const DEFAULT_HTTP_PORT: u16 = 7882;

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
/// `fs.list_dir` (UI Phase-1) is the one group that needs NO shared state — a
/// directory listing is a pure function of the filesystem at call time — so it
/// registers with neither `sessions` nor `project`.
/// `agents.*`/`skills.*` (issue #3449) are the Foundry GUI's catalog
/// management surface — list/create/delete over the disk tier
/// `crate::agents::resolve_agent` and `crate::skills::discover_skill_metadata`
/// already recognise, plus the embedded/bundled roster each falls back to.
/// Both register with immutable state resolved once here from `binding`
/// (`AgentsCatalogState`/`SkillsCatalogState` — no `Mutex`, every operation is
/// a stateless filesystem call at request time).
/// What: builds an empty `Router` + a fresh `SessionRegistry`, calls
/// `methods::register`, `crate::session::protocol::register`,
/// `crate::fs_browse::protocol::register`, and
/// `crate::task::protocol::register` on them, and returns both.
///
/// (DOC-48, issues #3294/#3295) also loads this project's
/// [`crate::workstreams::WorkstreamStore`] (`~/.trusty-code/workstreams-....json`,
/// derived from `binding` per DOC-48 §3.1), runs its boot reconciliation
/// (§3.3 — restores the persisted active pointer, or clears it if the
/// workstream it named was deleted; never destructive), wraps it in a
/// `tokio::sync::Mutex` (the store's mutating methods take `&mut self`, and
/// its file I/O is `async` — mirrors `SessionRegistry`'s `Arc`-shared-state
/// convention, see `crate::workstreams::activation::SharedWorkstreamStore`'s
/// docs for why `tokio`'s `Mutex` specifically), and registers
/// `crate::workstreams::protocol::register` — all six `workstream.*` methods
/// (`create`/`get`/`list`/`close` from #3295, `activate`/`deactivate` from
/// #3294) sharing this one store handle — alongside every other method
/// group. This is why `build_router` is now `async` and fallible — the
/// prior synchronous, infallible signature had no way to load persisted
/// state before the router starts answering requests, and a corrupt store
/// file must fail daemon startup loudly rather than silently discard the
/// user's workstreams (see `WorkstreamStore::load`'s docs).
///
/// `binding` is the daemon's project binding. Its `None` (projectless) state is
/// what makes the shell's entry screen implementable: previously `build_router`
/// took a required `PathBuf`, so a daemon could not even START without a
/// project, and `task.run` could not be called without one. `agents_dir` now
/// resolves via `ProjectBinding::agents_dir`, which falls back to the
/// user-level `~/.claude/agents` when projectless rather than to the process
/// CWD (which would silently bind a directory nobody chose).
/// (issue #3297) Also returns the [`SharedWorkstreamStore`] itself (not just
/// the `Router` it registered `workstream.*` against) — `run_http` needs the
/// SAME handle to hand to `crate::serve::http::run_http`, which merges
/// `crate::workstreams::sse::routes` (the `GET /workstreams/{id}/events`
/// aggregation route) alongside the JSON-RPC-backed REST surface. This is
/// why the return type grew a third element rather than staying a 2-tuple.
///
/// Test: `run_stdio_router_recognises_proof_of_life_methods`,
/// `build_router_wires_session_methods`, `build_router_wires_task_run`,
/// `build_router_is_projectless_without_a_project`,
/// `build_router_wires_fs_list_dir`, `build_router_wires_workstream_methods`,
/// `build_router_wires_agents_and_skills_catalog_methods`,
/// `build_router_reconciles_dangling_active_pointer_on_boot`.
pub async fn build_router(
    binding: ProjectBinding,
) -> Result<(Router, Arc<SessionRegistry>, SharedWorkstreamStore)> {
    build_router_at(binding, &crate::workstreams::default_data_dir()).await
}

/// Like [`build_router`] but takes an explicit workstream data directory
/// instead of always resolving `~/.trusty-code`.
///
/// Why: every unit test in this module builds a router; letting them all
/// resolve the REAL `~/.trusty-code` would read/write the developer's (or
/// CI runner's) actual home directory on every `cargo test` run — this seam
/// lets tests pass a throwaway tempdir instead, while `build_router` (the
/// only entry point `run_stdio`/`run_http` call) always uses the real
/// directory.
/// What: identical to `build_router`'s full docs otherwise — loads (or
/// creates) the workstream store at `data_dir` via `binding`'s derived
/// filename, runs boot reconciliation, and registers every method group.
async fn build_router_at(
    binding: ProjectBinding,
    data_dir: &std::path::Path,
) -> Result<(Router, Arc<SessionRegistry>, SharedWorkstreamStore)> {
    let sessions = Arc::new(SessionRegistry::new());
    let agents_dir = binding.agents_dir();

    let mut workstream_store = WorkstreamStore::load_for_binding(data_dir, &binding)
        .await
        .context("loading workstream store")?;
    workstream_store
        .reconcile_on_boot()
        .await
        .context("reconciling workstream store on boot")?;
    let workstreams = Arc::new(Mutex::new(workstream_store));

    let mut router = Router::new();
    methods::register(&mut router);
    crate::session::protocol::register(&mut router, sessions.clone(), workstreams.clone());
    crate::fs_browse::protocol::register(&mut router);
    crate::agents::protocol::register(
        &mut router,
        crate::agents::protocol::AgentsCatalogState::new(agents_dir.clone(), binding.is_bound()),
    );
    crate::skills::protocol::register(
        &mut router,
        crate::skills::protocol::SkillsCatalogState::new(binding.root()),
    );
    crate::task::protocol::register(
        &mut router,
        sessions.clone(),
        binding,
        agents_dir,
        workstreams.clone(),
    );
    crate::workstreams::protocol::register(&mut router, workstreams.clone());
    Ok((router, sessions, workstreams))
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
pub async fn run_stdio(binding: ProjectBinding) -> Result<()> {
    info!(
        binding = binding.state(),
        project = binding.label().unwrap_or_else(|| "<projectless>".into()),
        "tcode serve --stdio: starting"
    );
    let (router, sessions, _workstreams) = build_router(binding).await?;
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
pub async fn run_http(binding: ProjectBinding, port: u16) -> Result<()> {
    info!(
        binding = binding.state(),
        project = binding.label().unwrap_or_else(|| "<projectless>".into()),
        port,
        "tcode serve --http: starting"
    );
    let (router, sessions, workstreams) = build_router(binding).await?;
    http::run_http(router, sessions.clone(), workstreams, port).await?;
    sessions.shutdown_executions(SHUTDOWN_GRACE).await;
    info!("tcode serve --http: stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use trusty_common::mcp::Request;

    fn test_ctx() -> crate::jsonrpc::ConnectionContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        crate::jsonrpc::ConnectionContext::new(tx)
    }

    /// `build_router` must recognise both proof-of-life methods.
    #[tokio::test]
    async fn run_stdio_router_recognises_proof_of_life_methods() {
        let project = tempfile::tempdir().expect("project tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let (router, _sessions, _workstreams) = build_router_at(
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            data_dir.path(),
        )
        .await
        .expect("build_router_at");
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
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let (router, sessions, _workstreams) = build_router_at(
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            data_dir.path(),
        )
        .await
        .expect("build_router_at");
        let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);

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

    /// `build_router` must also wire `fs.list_dir` (UI Phase-1), so the UI's
    /// project picker is reachable over the same `/rpc` endpoint as everything
    /// else.
    #[tokio::test]
    async fn build_router_wires_fs_list_dir() {
        let project = tempfile::tempdir().expect("project tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let (router, _sessions, _workstreams) = build_router_at(
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            data_dir.path(),
        )
        .await
        .expect("build_router_at");

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "fs.list_dir".to_string(),
            params: Some(json!({"path": project.path().display().to_string()})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "fs.list_dir must be registered, got {:?}",
            resp.error
        );
        assert!(resp.result.expect("result")["entries"].is_array());
    }

    /// `build_router` must also wire `agents.list`/`skills.list` (issue
    /// #3449), so the Foundry GUI's Agents/Skills tabs are reachable over
    /// the same `/rpc` endpoint as everything else.
    #[tokio::test]
    async fn build_router_wires_agents_and_skills_catalog_methods() {
        let project = tempfile::tempdir().expect("project tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let (router, _sessions, _workstreams) = build_router_at(
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            data_dir.path(),
        )
        .await
        .expect("build_router_at");

        for method in ["agents.list", "skills.list"] {
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

    /// `DEFAULT_HTTP_PORT` must stay the documented value (7882) so a
    /// change is a deliberate edit to both the constant and its doc comment,
    /// not an accidental drift.
    #[test]
    fn default_http_port_is_documented_value() {
        assert_eq!(DEFAULT_HTTP_PORT, 7882);
    }

    /// Cross-crate port-uniqueness contract (#3364).
    ///
    /// Why: `DEFAULT_HTTP_PORT` previously reused `7881`, silently colliding
    /// with `trusty-mpm`'s supervisor metrics listener
    /// (`DEFAULT_METRICS_ADDR`, `crates/trusty-mpm/src/supervisor/config.rs`)
    /// — the supervisor's generic `/health` masked the collision until a real
    /// `tcode` route 404'd. This mirrors the `default_port_does_not_collide_
    /// with_known_siblings` guard already used by `trusty-console` and
    /// `trusty-review` so a future edit here that reintroduces a collision
    /// fails this test instead of shipping a crash-loop.
    /// What: asserts `DEFAULT_HTTP_PORT` is absent from the known-sibling
    /// ports list.
    /// Test: this is the test.
    #[test]
    fn default_http_port_does_not_collide_with_known_siblings() {
        // (binary, port, source-of-truth pointer)
        let known_siblings: &[(&str, u16, &str)] = &[
            (
                "trusty-memory",
                7070,
                "trusty-memory/src/http_server.rs::DEFAULT_HTTP_PORT",
            ),
            (
                "trusty-search",
                7878,
                "trusty-search/src/service/constants.rs::DEFAULT_PORT",
            ),
            (
                "trusty-analyze",
                7879,
                "trusty-analyze/src/service/events.rs::DEFAULT_PORT",
            ),
            (
                "trusty-mpm",
                7880,
                "trusty-mpm/src/core/discovery.rs::DEFAULT_DAEMON_ADDR",
            ),
            (
                // #3364: the collision that motivated this guard — trusty-mpm's
                // supervisor metrics listener, deployed via launchd and never
                // moved (unlike this crate's daemon default).
                "trusty-mpm-supervisor",
                7881,
                "trusty-mpm/src/supervisor/config.rs::DEFAULT_METRICS_ADDR",
            ),
            (
                "trusty-console",
                7788,
                "trusty-console/src/lib.rs::DEFAULT_PORT",
            ),
            (
                "trusty-embedderd",
                7890,
                "trusty-embedderd/src/lib.rs::Args::http_addr (--http default_value, manual/dev-run only)",
            ),
            (
                "trusty-review",
                7891,
                "trusty-review/src/service/mod.rs::DEFAULT_PORT",
            ),
            (
                "trusty-agents",
                8080,
                "trusty-agents/src/runtime/mode_dispatch.rs (--port default 8080)",
            ),
        ];
        for (binary, port, source) in known_siblings {
            assert_ne!(
                DEFAULT_HTTP_PORT, *port,
                "trusty-code DEFAULT_HTTP_PORT {DEFAULT_HTTP_PORT} collides with {binary}'s {port} ({source})"
            );
        }
    }

    /// The daemon must ASSEMBLE with no project bound — `build_router` took a
    /// required `PathBuf` before, so a projectless daemon could not exist and
    /// the shell had no entry state to render. `task.run` must be wired just
    /// the same in that state.
    #[tokio::test]
    async fn build_router_is_projectless_without_a_project() {
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let (router, _sessions, _workstreams) =
            build_router_at(ProjectBinding::None, data_dir.path())
                .await
                .expect("build_router_at");
        for method in ["task.run", "session.create", "session.list"] {
            let req = Request {
                jsonrpc: Some("2.0".to_string()),
                id: Some(json!(1)),
                method: method.to_string(),
                params: Some(json!({})),
            };
            let resp = router.dispatch(req, &test_ctx()).await;
            // Params are deliberately empty, so an `invalid_params`/
            // `invalid_argument` is fine and expected — the ONE thing that must
            // not happen is `-32601 Method not found`, which would mean a
            // projectless daemon serves a different method surface.
            if let Some(err) = resp.error {
                assert_ne!(
                    err.code, -32601,
                    "{method} must be wired on a projectless daemon, got method-not-found"
                );
            }
        }
    }

    #[tokio::test]
    async fn build_router_wires_task_run() {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::create_dir_all(project.path().join(".claude").join("agents")).expect("mkdir");
        std::fs::write(
            project.path().join(".claude").join("agents").join("pm.md"),
            "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nPM\n",
        )
        .expect("write pm.md");
        std::fs::write(
            project
                .path()
                .join(".claude")
                .join("agents")
                .join("python-engineer.md"),
            "---\nname: python-engineer\nmodel: deepseek/deepseek-chat\n---\n\neng\n",
        )
        .expect("write python-engineer.md");

        // SAFETY: test-only env mutation; serialized by the shared crate lock.
        let _guard = crate::task::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var(
                crate::task::mock_llm::MOCK_LLM_ENV,
                crate::task::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let (router, sessions, _workstreams) = build_router_at(
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            data_dir.path(),
        )
        .await
        .expect("build_router_at");
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

    /// `build_router` must also wire `workstream.activate`/
    /// `workstream.deactivate` (DOC-48 §6, issue #3294), and the activation
    /// pointer they write must be the SAME store a fresh `build_router_at`
    /// call over the same `data_dir`/`binding` reloads (AC-1.4 boot
    /// restoration, exercised end-to-end through the daemon's own assembly
    /// path rather than `workstreams::protocol_tests`' narrower unit tests).
    #[tokio::test]
    async fn build_router_wires_workstream_methods() {
        let project = tempfile::tempdir().expect("project tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let binding =
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind");

        let (router, _sessions, _workstreams) = build_router_at(binding.clone(), data_dir.path())
            .await
            .expect("build_router_at");

        let create_req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "workstream.activate".to_string(),
            params: Some(json!({"id": crate::workstreams::WorkstreamId::new().to_string()})),
        };
        // An id minted here names no existing workstream, so this must map
        // to `-32002 not_found`, NOT `-32601 method not found` — the point
        // of this assertion is that the method is REGISTERED, not that this
        // particular call succeeds (there is no `workstream.create` to seed
        // one with yet — that is #3295's surface).
        let resp = router.dispatch(create_req, &test_ctx()).await;
        let err = resp
            .error
            .expect("activating an unknown id must fail, not silently succeed");
        assert_eq!(err.code, -32002, "workstream.activate must be registered");

        let deactivate_req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(2)),
            method: "workstream.deactivate".to_string(),
            params: Some(json!({"id": crate::workstreams::WorkstreamId::new().to_string()})),
        };
        // Deactivating an unknown id is an idempotent no-op (DOC-48 §4.3),
        // so this must succeed — proving `workstream.deactivate` is wired
        // too.
        let resp = router.dispatch(deactivate_req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "workstream.deactivate must be registered, got {:?}",
            resp.error
        );

        // Boot restoration (AC-1.4): a fresh `build_router_at` over the SAME
        // data_dir/binding must load the store this instance persisted to,
        // rather than starting from a brand-new empty one — proven here by
        // reconciliation completing without error on the same file the
        // first instance created.
        let (_router2, _sessions2, _workstreams2) = build_router_at(binding, data_dir.path())
            .await
            .expect("a second build_router_at over the same data_dir must succeed");
    }

    /// Boot reconciliation (DOC-48 §3.3, AC-1.4/AC-6.1/AC-6.2) must run
    /// through the REAL daemon assembly path, not just
    /// `WorkstreamStore::reconcile_on_boot` called directly (already covered
    /// by `workstreams::store::store_tests::reconcile_clears_active_when_target_missing`).
    /// A store file seeded on disk with a DANGLING `active_workstream_id`
    /// (pointing at a workstream absent from `workstreams[]` — e.g. deleted
    /// out from under the pointer, or hand-edited) must have that pointer
    /// cleared by `build_router_at`'s `reconcile_on_boot()` call BEFORE the
    /// router ever answers a request; `workstream.list` reporting
    /// `active_workstream_id: null` is the end-to-end proof.
    #[tokio::test]
    async fn build_router_reconciles_dangling_active_pointer_on_boot() {
        let project = tempfile::tempdir().expect("project tempdir");
        let data_dir = tempfile::tempdir().expect("data tempdir");
        let binding =
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind");

        let store_path = crate::workstreams::store_path(data_dir.path(), &binding);
        let dangling_id = crate::workstreams::WorkstreamId::new();
        let raw = json!({
            "version": "1.0",
            "active_workstream_id": dangling_id,
            "workstreams": [],
        });
        tokio::fs::create_dir_all(data_dir.path())
            .await
            .expect("mkdir data_dir");
        tokio::fs::write(&store_path, serde_json::to_string_pretty(&raw).unwrap())
            .await
            .expect("seed raw store file with a dangling active_workstream_id");

        let (router, _sessions, _workstreams) = build_router_at(binding, data_dir.path())
            .await
            .expect("build_router_at must reconcile the dangling pointer, not fail");

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "workstream.list".to_string(),
            params: Some(json!({})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "workstream.list must succeed, got {:?}",
            resp.error
        );
        assert_eq!(
            resp.result.unwrap()["active_workstream_id"],
            Value::Null,
            "boot reconciliation through build_router_at must clear a dangling \
             active_workstream_id before the router answers requests"
        );
    }
}
