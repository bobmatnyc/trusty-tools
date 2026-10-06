//! End-to-end regression for issue #8737: `reindex`, `quantize` and
//! `index relocate` must never act on a target read from `TRUSTY_INDEX` alone,
//! and `reindex` must never let `TRUSTY_INDEX` (or `-i`) re-point an index at
//! an explicit PATH that belongs to a different index.
//!
//! Why: before the fix all three took their target purely from `cli.index`,
//! which clap folds together with `TRUSTY_INDEX`. `reindex <PATH>` then POSTed
//! `{root_path: PATH}` to the env-named index — rebasing a live index onto an
//! unrelated root and overwriting its corpus — the same class as #8175.
//! What: serves a real `build_router` with two bare, resident indexes (no
//! embedder, no walk), wrapped in three layers: a request log that records
//! method, path and body; an optional chaos layer failing every
//! `GET /indexes/*/status`; and a mutation block that answers every reindex,
//! quantize and relocate request with `500` so the real handlers never run.
//! The compiled `trusty-search` binary is spawned against it with
//! `TRUSTY_DATA_DIR`, `HOME` and `XDG_CONFIG_HOME` pinned to tempdirs, the
//! inherited `TRUSTY_INDEX` removed, and a scratch working directory.
//! Assertions read the request log, not just the exit code.
//!
//! Test: `cargo test -p trusty-search --test integration reindex_quantize_env_conflict_8737::`

use crate::test_daemon;
use std::path::Path;

use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use tokio::sync::RwLock;

use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::service::server::{build_router, SearchAppState};

use crate::real_allowlist_guard;
use real_allowlist_guard::RealAllowlistGuard;

const INDEX_A: &str = "idx-a-8737";
const INDEX_B: &str = "idx-b-8737";

/// One recorded request: method, path, body.
type Logged = (String, String, String);

#[derive(Default)]
struct RequestLog(Mutex<Vec<Logged>>);

impl RequestLog {
    fn snapshot(&self) -> Vec<Logged> {
        self.0.lock().expect("request log mutex").clone()
    }

    /// Every request that could rebuild, re-quantize, relocate or delete.
    fn mutations(&self) -> Vec<Logged> {
        self.snapshot()
            .into_iter()
            .filter(|(m, _, _)| m != "GET")
            .collect()
    }
}

async fn log_requests(State(log): State<Arc<RequestLog>>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20)
        .await
        .unwrap_or_default();
    log.0.lock().expect("request log mutex").push((
        parts.method.to_string(),
        parts.uri.path().to_string(),
        String::from_utf8_lossy(&bytes).into_owned(),
    ));
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

fn json_response(status: StatusCode, body: &'static str) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .expect("build fixture response")
}

/// Answers every reindex / quantize / relocate request with `500`, so the
/// real handlers never run even when the CLI does send one.
async fn block_mutations(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let blocked = (req.method() == Method::POST
        && (path.ends_with("/reindex") || path.ends_with("/quantize")))
        || req.method() == Method::PATCH;
    if blocked {
        return json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"fixture_blocks_mutation"}"#,
        );
    }
    next.run(req).await
}

/// Serve the router with indexes A and B. `status_chaos` makes
/// `GET /indexes/*/status` answer with that code — the call the PATH-vs-id
/// agreement check needs — for every index, or only for the one id given.
async fn spawn_daemon(
    root_a: &Path,
    root_b: &Path,
    status_chaos: Option<(StatusCode, Option<&'static str>)>,
) -> (String, Arc<RequestLog>) {
    let registry = IndexRegistry::new();
    for (id, root) in [(INDEX_A, root_a), (INDEX_B, root_b)] {
        let indexer = CodeIndexer::new(id, root.to_string_lossy().into_owned());
        registry.register(IndexHandle::bare(
            IndexId::new(id),
            Arc::new(RwLock::new(indexer)),
            root.to_path_buf(),
        ));
    }
    let log = Arc::new(RequestLog::default());
    let mut app =
        build_router(SearchAppState::new(registry)).layer(middleware::from_fn(block_mutations));
    if let Some((code, only_id)) = status_chaos {
        app = app.layer(middleware::from_fn(move |req: Request, next: Next| {
            let path = req.uri().path();
            let is_status = req.method() == Method::GET
                && path.starts_with("/indexes/")
                && path.ends_with("/status")
                && only_id.is_none_or(|id| path == format!("/indexes/{id}/status"));
            async move {
                if is_status {
                    return json_response(code, r#"{"error":"chaos_status_test"}"#);
                }
                next.run(req).await
            }
        }));
    }
    // Outermost, so it also sees requests the inner layers short-circuit.
    let app = app.layer(middleware::from_fn_with_state(log.clone(), log_requests));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), log)
}

/// Per-test scratch state: two index roots, the data dir the CLI discovers
/// the router through, a fake home, and a working directory outside any repo.
struct Scratch {
    root_a: tempfile::TempDir,
    root_b: tempfile::TempDir,
    data_dir: tempfile::TempDir,
    home: tempfile::TempDir,
    cwd: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let t = || tempfile::tempdir().expect("tempdir");
        Self {
            root_a: t(),
            root_b: t(),
            data_dir: t(),
            home: t(),
            cwd: t(),
        }
    }

    fn point_discovery_at(&self, base: &str) {
        let addr = base.trim_start_matches("http://");
        std::fs::write(self.data_dir.path().join("http_addr"), addr).expect("write http_addr");
    }

    /// `trusty-search <args>` with every path the CLI could write pinned to a
    /// tempdir and no inherited `TRUSTY_INDEX`.
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = test_daemon::command();
        cmd.args(args)
            .env("TRUSTY_DATA_DIR", self.data_dir.path())
            .env("HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path())
            .env_remove("TRUSTY_DATA_DIR_OVERRIDE")
            .env_remove("TRUSTY_INDEX")
            .current_dir(self.cwd.path());
        cmd
    }
}

// #8900: stamped by `test_daemon::command` and bounded here, so a daemon the
// CLI auto-starts can neither outlive the run nor hold this call open.
fn run(mut cmd: Command) -> (i32, String) {
    let out = test_daemon::run_bounded(&mut cmd, std::time::Duration::from_secs(120));
    (out.code.unwrap_or(-1), out.combined)
}

/// Assert an env-only invocation refused with NO request at all.
fn assert_env_only_refusal(code: i32, output: &str, log: &RequestLog) {
    assert_ne!(code, 0, "env-only target must refuse; output:\n{output}");
    assert!(
        output.contains(INDEX_B) && output.contains("TRUSTY_INDEX"),
        "the refusal must name the id and its source; output:\n{output}"
    );
    assert!(
        log.snapshot().is_empty(),
        "an env-only refusal happens before any request: {:?}",
        log.snapshot()
    );
}

/// The one reindex POST the CLI sent, as `(path, root_path)`.
fn reindex_posts(log: &RequestLog) -> Vec<(String, String)> {
    log.mutations()
        .into_iter()
        .map(|(_, path, body)| {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let root = v["root_path"].as_str().unwrap_or_default().to_string();
            (path, root)
        })
        .collect()
}

// `multi_thread` everywhere: the blocking `Command::output()` holds one worker
// while the router needs another to keep accepting connections.

/// Core regression: PATH names A, `TRUSTY_INDEX` names B. Pre-fix, `reindex`
/// POSTed `/indexes/B/reindex {root_path: A}` — rebasing B onto A's root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_a_with_env_b_refuses_and_sends_no_reindex() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let (base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    s.point_discovery_at(&base);

    let mut cmd = s.command(&["reindex"]);
    cmd.arg(s.root_a.path()).env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "PATH=A vs TRUSTY_INDEX=B must refuse; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_A) && output.contains(INDEX_B),
        "the refusal must name both values; output:\n{output}"
    );
    assert!(
        log.mutations().is_empty(),
        "no reindex may be sent on a mismatch: {:?}",
        log.mutations()
    );
    guard.assert_unchanged("reindex_path_a_with_env_b_refuses_and_sends_no_reindex");
}

/// `reindex` with only `TRUSTY_INDEX` refuses before any request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_env_only_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let (base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    s.point_discovery_at(&base);

    let mut cmd = s.command(&["reindex"]);
    cmd.env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_env_only_refusal(code, &output, &log);
    guard.assert_unchanged("reindex_env_only_refuses_with_no_requests");
}

/// `quantize` with only `TRUSTY_INDEX` refuses before any request — pre-fix it
/// sent the dry-run and then the real conversion to the env-named index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quantize_env_only_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let (base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    s.point_discovery_at(&base);

    let mut cmd = s.command(&["quantize", "--to", "f16", "--yes"]);
    cmd.env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_env_only_refusal(code, &output, &log);
    guard.assert_unchanged("quantize_env_only_refuses_with_no_requests");
}

/// `index relocate` rewrites an index's root, so an env-only target refuses
/// before any request (and before it approves the destination).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relocate_env_only_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let (base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    s.point_discovery_at(&base);

    let mut cmd = s.command(&["index", "relocate", "--to"]);
    cmd.arg(s.root_a.path()).env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_env_only_refusal(code, &output, &log);
    guard.assert_unchanged("relocate_env_only_refuses_with_no_requests");
}

/// A real `--index A` from an unrelated working directory reindexes A at its
/// REGISTERED root. Pre-fix the root came from CWD detection, rebasing A.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_flag_alone_targets_the_registered_root() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let (base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    s.point_discovery_at(&base);

    let (_code, output) = run(s.command(&["reindex", "--index", INDEX_A]));

    let expected = (
        format!("/indexes/{INDEX_A}/reindex"),
        s.root_a.path().to_string_lossy().into_owned(),
    );
    assert_eq!(
        reindex_posts(&log),
        vec![expected],
        "exactly one reindex, of A at A's registered root; output:\n{output}"
    );
    guard.assert_unchanged("reindex_flag_alone_targets_the_registered_root");
}

/// PATH alone targets the index registered at PATH, not the CWD's; PATH plus
/// an agreeing `TRUSTY_INDEX` proceeds to the same index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_alone_or_agreeing_targets_the_path_index() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let (base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;
    s.point_discovery_at(&base);

    let mut path_only = s.command(&["reindex"]);
    path_only.arg(s.root_a.path());
    let (_, out_path_only) = run(path_only);
    let mut agreeing = s.command(&["reindex"]);
    agreeing.arg(s.root_a.path()).env("TRUSTY_INDEX", INDEX_A);
    let (_, out_agreeing) = run(agreeing);

    let expected = (
        format!("/indexes/{INDEX_A}/reindex"),
        s.root_a.path().to_string_lossy().into_owned(),
    );
    assert_eq!(
        reindex_posts(&log),
        vec![expected.clone(), expected],
        "both runs must reindex A at A's root; outputs:\n{out_path_only}\n{out_agreeing}"
    );
    guard.assert_unchanged("reindex_path_alone_or_agreeing_targets_the_path_index");
}

// ─── Error arms: the agreement check cannot run ────────────────────────────

/// PATH A and `--index A` AGREE, but every status lookup answers `503` or
/// `404`, so agreement cannot be confirmed: refuse, with no reindex sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_and_flag_when_status_fails_sends_no_reindex() {
    for code in [StatusCode::SERVICE_UNAVAILABLE, StatusCode::NOT_FOUND] {
        let s = Scratch::new();
        let guard = RealAllowlistGuard::capture();
        let (base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), Some((code, None))).await;
        s.point_discovery_at(&base);

        let mut cmd = s.command(&["reindex", "--index", INDEX_A]);
        cmd.arg(s.root_a.path());
        let (exit, output) = run(cmd);

        assert_ne!(
            exit, 0,
            "{code}: unconfirmed agreement must refuse; output:\n{output}"
        );
        assert!(
            log.snapshot()
                .iter()
                .any(|(m, p, _)| m == "GET" && p.ends_with("/status")),
            "{code}: the agreement check must have been attempted: {:?}",
            log.snapshot()
        );
        assert!(
            log.mutations().is_empty(),
            "{code}: no reindex may be sent: {:?}",
            log.mutations()
        );
        guard.assert_unchanged("reindex_path_and_flag_when_status_fails_sends_no_reindex");
    }
}

/// #8737 review: A and B share one root and only A's status answers `503`.
/// Pre-fix, `find_index_by_path` skipped A and matched B by root path, so
/// both `reindex <root>` and `reindex <root> --index B` POSTed a reindex to B.
/// The lookup must instead fail closed, naming the unreadable A.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_refuses_when_a_same_root_index_status_503s() {
    for with_flag in [false, true] {
        let s = Scratch::new();
        let guard = RealAllowlistGuard::capture();
        let chaos = Some((StatusCode::SERVICE_UNAVAILABLE, Some(INDEX_A)));
        let (base, log) = spawn_daemon(s.root_a.path(), s.root_a.path(), chaos).await;
        s.point_discovery_at(&base);

        let mut cmd = s.command(&["reindex"]);
        cmd.arg(s.root_a.path());
        if with_flag {
            cmd.args(["--index", INDEX_B]);
        }
        let (exit, output) = run(cmd);

        assert_ne!(
            exit, 0,
            "flag={with_flag}: an unreadable same-root index must refuse; output:\n{output}"
        );
        assert!(
            output.contains(&format!("\"{INDEX_A}\"")),
            "flag={with_flag}: the refusal must name the unreadable index; output:\n{output}"
        );
        assert!(
            log.mutations().is_empty(),
            "flag={with_flag}: no reindex may be sent: {:?}",
            log.mutations()
        );
        guard.assert_unchanged("reindex_path_refuses_when_a_same_root_index_status_503s");
    }
}

/// PATH and `--index` given, daemon down: refuse, and the running router —
/// which discovery deliberately does NOT point at — receives nothing.
///
/// A `daemon.lock` naming PID 1 makes the guard wait instead of auto-spawning
/// a real daemon, and `daemon.port` pins the health probe to the same closed
/// port (the `daemon_env_precedence.rs` / #8175 technique).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_path_and_flag_when_daemon_is_down_refuses_with_no_requests() {
    let s = Scratch::new();
    let guard = RealAllowlistGuard::capture();
    let (_base, log) = spawn_daemon(s.root_a.path(), s.root_b.path(), None).await;

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a throwaway listener to mint a closed port");
    let port = closed.local_addr().expect("local addr").port();
    drop(closed);
    let dir = s.data_dir.path();
    std::fs::write(dir.join("http_addr"), format!("127.0.0.1:{port}")).expect("http_addr");
    std::fs::write(dir.join("daemon.port"), port.to_string()).expect("daemon.port");
    std::fs::write(dir.join("daemon.lock"), b"1").expect("daemon.lock");

    let mut cmd = s.command(&["reindex", "--index", INDEX_A]);
    cmd.arg(s.root_a.path());
    let (code, output) = run(cmd);

    assert_ne!(code, 0, "a down daemon must refuse; output:\n{output}");
    assert!(
        log.snapshot().is_empty(),
        "a down daemon must receive NO request at all: {:?}",
        log.snapshot()
    );
    guard.assert_unchanged("reindex_path_and_flag_when_daemon_is_down_refuses_with_no_requests");
}
