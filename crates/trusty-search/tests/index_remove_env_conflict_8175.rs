//! End-to-end regression for issue #8175: `index remove <PATH>` must never
//! let `TRUSTY_INDEX` silently win over — or silently lose to — an explicit
//! PATH argument that names a DIFFERENT index.
//!
//! Why: on 2026-09-26, `trusty-search index remove <scratch-path> --yes
//! --json` deleted the live `trusty-tools-4e2cf878` project index (about
//! 236k chunks) instead of the scratch index the PATH named, because the
//! shell also exported `TRUSTY_INDEX=trusty-tools-4e2cf878` and the CLI's
//! `-i`/`--index` global flag folds that env value in indistinguishably from
//! a real flag. The pre-fix `handle_index_remove` used
//! `explicit_index_id` — which is `cli.index`, i.e. exactly this folded value
//! — UNCONDITIONALLY whenever it was `Some`, skipping the PATH lookup
//! entirely. This file proves the fix: given a PATH naming index A and
//! `TRUSTY_INDEX` naming a DIFFERENT index B, the command refuses and BOTH
//! indexes are still registered afterward.
//!
//! What: serves a real axum router (`build_router`) with two bare, resident
//! indexes registered directly via `IndexHandle::bare` (no embedder, no
//! walk — issue #8175's Constraints section forbids touching any live
//! registered index, so this never runs against the operator's own daemon).
//! The router's address is written to the isolated `TRUSTY_DATA_DIR`'s
//! `http_addr` discovery file, then the real compiled `trusty-search` binary
//! is spawned as `index remove <root-of-A>` with `TRUSTY_INDEX=<id of B>` in
//! its environment. Asserts the process exits non-zero, the message names
//! both ids, and a follow-up `GET /indexes` still lists both.
//!
//! Fix-up (issue #8175, code-critic WARN): every spawned subprocess also gets
//! `HOME`/`XDG_CONFIG_HOME` pointed at a per-test fake home
//! ([`remove_command`]), and [`RealAllowlistGuard`] asserts the operator's
//! REAL `~/Library/Application Support/trusty-search/allowlist.toml` is
//! byte-for-byte and mtime-for-mtime unchanged across every run — see that
//! guard's doc comment for why this file specifically was at risk. Two
//! error-arm (Fail-Open Check) tests are added: a closed-port "daemon down"
//! and a `503`-during-agreement-check daemon must both refuse with NO
//! `DELETE` request ever sent, proven against a request log the fake router
//! records, not just against the exit code.
//!
//! Test: `cargo test -p trusty-search --test index_remove_env_conflict_8175`

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use tokio::sync::RwLock;

use trusty_search::allowlist::AllowlistConfig;
use trusty_search::core::indexer::CodeIndexer;
use trusty_search::core::registry::{IndexHandle, IndexId, IndexRegistry};
use trusty_search::service::server::{build_router, SearchAppState};

const INDEX_A: &str = "idx-a-8175";
const INDEX_B: &str = "idx-b-8175";

// ─── Real-allowlist guard (fix-up: code-critic WARN) ───────────────────────

/// Snapshot of the OPERATOR's real
/// `~/Library/Application Support/trusty-search/allowlist.toml` (or its
/// absence), read only to prove this file never touches it.
///
/// Why: `AllowlistConfig::default_path()` (`crates/trusty-search/src/allowlist/mod.rs:210`)
/// resolves via `dirs::config_dir()`, which on macOS reads `$HOME` and on
/// Linux reads `$XDG_CONFIG_HOME`/`$HOME` — NOT `TRUSTY_DATA_DIR`. The
/// best-effort allowlist cleanup at `index_remove.rs:309`
/// (`crate::allowlist::remove_from_allowlist`) runs on every path that
/// completes a real `DELETE`, so a subprocess that inherits the operator's
/// real `HOME` can rewrite that real file even though the `DELETE` itself
/// always targets the fake router. [`remove_command`] closes this by pinning
/// `HOME`/`XDG_CONFIG_HOME` to a fake per-test home; this guard is the proof
/// that pin actually holds. Read-only access to the real path is taken ONLY
/// here, and only to build the before/after comparison.
/// What: absence is a valid, expected state (CI has no such file at all) —
/// `exists: false` on both sides is success, not a skipped check. Presence
/// is compared on content bytes AND mtime, so even a content-preserving
/// rewrite (a temp-file-then-rename with unchanged bytes) is caught.
/// Test: every `#[tokio::test]` below calls `RealAllowlistGuard::capture`
/// before its subprocess and `assert_unchanged` after.
struct RealAllowlistGuard {
    exists: bool,
    bytes: Option<Vec<u8>>,
    mtime: Option<std::time::SystemTime>,
}

impl RealAllowlistGuard {
    fn capture() -> Self {
        let path = AllowlistConfig::default_path();
        match std::fs::metadata(&path) {
            Ok(meta) => Self {
                exists: true,
                bytes: std::fs::read(&path).ok(),
                mtime: meta.modified().ok(),
            },
            Err(_) => Self {
                exists: false,
                bytes: None,
                mtime: None,
            },
        }
    }

    /// Why: called after the subprocess under test has exited, so any write
    /// it performed against the real path — however cheap or well-intentioned
    /// — is captured here and turned into a hard test failure.
    fn assert_unchanged(&self, label: &str) {
        let after = Self::capture();
        assert_eq!(
            self.exists, after.exists,
            "{label}: the real allowlist.toml went from present to absent or vice versa"
        );
        assert_eq!(
            self.bytes, after.bytes,
            "{label}: the real allowlist.toml content changed"
        );
        assert_eq!(
            self.mtime, after.mtime,
            "{label}: the real allowlist.toml mtime changed (even a no-op \
             rewrite bumps this, so an unchanged mtime is the strongest signal \
             nothing touched it)"
        );
    }
}

// ─── Request log + chaos middleware (fix-up: error-arm tests) ──────────────

/// Every `(method, path)` the fake router received, in arrival order.
///
/// Why: the error-arm tests must prove NO `DELETE` reached the router, not
/// merely that the process exited non-zero — a refusal that happened to also
/// return a bad exit code for an unrelated reason would pass an exit-code-only
/// assertion.
#[derive(Default)]
struct RequestLog(Mutex<Vec<(String, String)>>);

impl RequestLog {
    fn snapshot(&self) -> Vec<(String, String)> {
        self.0.lock().expect("request log mutex").clone()
    }

    fn contains_delete(&self) -> bool {
        self.snapshot().iter().any(|(m, _)| m == "DELETE")
    }
}

async fn log_requests(State(log): State<Arc<RequestLog>>, req: Request, next: Next) -> Response {
    log.0
        .lock()
        .expect("request log mutex")
        .push((req.method().to_string(), req.uri().path().to_string()));
    next.run(req).await
}

/// Chaos middleware: answers every `GET /indexes/*/status` with `503`,
/// simulating a daemon that cannot serve the per-index status lookup
/// `find_index_by_path` needs to resolve PATH → id during the agreement
/// check (issue #8175 fix-up, error-arm test (b)).
async fn force_status_503(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if req.method() == Method::GET && path.starts_with("/indexes/") && path.ends_with("/status") {
        return Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"error":"chaos_503_test"}"#))
            .expect("build 503 response");
    }
    next.run(req).await
}

/// Serve a real daemon router with two independent, resident, bare indexes
/// registered — no embedder, no walk, no disk corpus, so nothing here can
/// touch a real operator index. `chaos` optionally wraps the router with
/// [`force_status_503`]. Returns the base URL and the request log.
async fn spawn_daemon_with_two_indexes(
    root_a: &Path,
    root_b: &Path,
    chaos: bool,
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
    let state = SearchAppState::new(registry);
    let log = Arc::new(RequestLog::default());
    let mut app = build_router(state);
    if chaos {
        app = app.layer(middleware::from_fn(force_status_503));
    }
    // Outermost: the log sees every request, including ones the chaos layer
    // (if present) short-circuits before the real handler ever runs.
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

/// Point the CLI's daemon discovery at `base` inside an isolated
/// `TRUSTY_DATA_DIR`, so the spawned `trusty-search` process finds the test
/// router instead of any real daemon on the machine.
fn write_discovery_file(data_dir: &Path, base: &str) {
    std::fs::create_dir_all(data_dir).expect("create scratch TRUSTY_DATA_DIR");
    let addr = base.trim_start_matches("http://");
    std::fs::write(data_dir.join("http_addr"), addr).expect("write http_addr discovery file");
}

/// Base `Command` for `trusty-search index remove`, pre-wired with
/// `TRUSTY_DATA_DIR` and — fix-up, issue #8175 — `HOME`/`XDG_CONFIG_HOME`
/// pointed at `fake_home`, so `AllowlistConfig::default_path()` can never
/// resolve to the operator's real config directory even on the code path
/// that performs a real `DELETE`. See [`RealAllowlistGuard`].
fn remove_command(data_dir: &Path, fake_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-search"));
    cmd.args(["index", "remove"])
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("HOME", fake_home)
        .env("XDG_CONFIG_HOME", fake_home);
    cmd
}

fn run(mut cmd: Command) -> (i32, String) {
    let out = cmd.output().expect("spawn trusty-search index remove");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), combined)
}

/// `GET /indexes` against the test router — the list of currently-registered
/// ids.
async fn list_indexes(base: &str) -> Vec<String> {
    let body: serde_json::Value = reqwest::get(format!("{base}/indexes"))
        .await
        .expect("GET /indexes")
        .json()
        .await
        .expect("parse /indexes body");
    body["indexes"]
        .as_array()
        .expect("indexes array")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

/// Core regression: PATH names A, `TRUSTY_INDEX` names B — the command must
/// refuse and touch neither.
///
/// Why: this is the exact incident shape. On pre-#8175 `main.rs` /
/// `handle_index_remove`, `cli.index` (folding `TRUSTY_INDEX`) was used
/// unconditionally whenever `Some`, so this same invocation would have
/// resolved straight to index B via `find_index_by_id` and deleted it with no
/// refusal — silently ignoring the PATH argument naming A entirely.
///
/// `multi_thread` is required: the blocking `Command::output()` call below
/// occupies one worker thread for the whole subprocess run, and the test
/// router's `axum::serve` task needs a second one free to keep accepting
/// connections while that call is in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_path_a_and_env_b_refuses_and_touches_neither() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    let (base, _log) = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), false).await;
    write_discovery_file(data_dir.path(), &base);

    let before = list_indexes(&base).await;
    assert!(before.contains(&INDEX_A.to_string()) && before.contains(&INDEX_B.to_string()));

    let mut cmd = remove_command(data_dir.path(), fake_home.path());
    cmd.arg(root_a.path())
        .arg("--yes")
        .env("TRUSTY_INDEX", INDEX_B);
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "PATH=A vs TRUSTY_INDEX=B must refuse rather than pick one; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_A) && output.contains(INDEX_B),
        "the refusal must name both conflicting values; output:\n{output}"
    );

    let after = list_indexes(&base).await;
    assert!(
        after.contains(&INDEX_A.to_string()),
        "index A must survive a refused remove; still registered: {after:?}"
    );
    assert!(
        after.contains(&INDEX_B.to_string()),
        "index B must survive a refused remove; still registered: {after:?}"
    );
    guard.assert_unchanged("remove_with_path_a_and_env_b_refuses_and_touches_neither");
}

/// `index remove` with NO PATH argument, only `TRUSTY_INDEX`, must also
/// refuse — a destructive verb never resolves its target from the
/// environment alone (issue #8175's "Wanted" list, second bullet).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_no_argument_and_env_only_refuses() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    let (base, _log) = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), false).await;
    write_discovery_file(data_dir.path(), &base);

    // No PATH argument at all — cwd of the spawned process is irrelevant
    // because the refusal must fire before any path resolution happens.
    let mut cmd = remove_command(data_dir.path(), fake_home.path());
    cmd.arg("--yes")
        .env("TRUSTY_INDEX", INDEX_B)
        .current_dir(std::env::temp_dir());
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "TRUSTY_INDEX alone, with no PATH/-i, must refuse; output:\n{output}"
    );
    assert!(
        output.contains(INDEX_B),
        "the refusal must name the TRUSTY_INDEX id it declined to touch: {output}"
    );

    let after = list_indexes(&base).await;
    assert!(
        after.contains(&INDEX_A.to_string()) && after.contains(&INDEX_B.to_string()),
        "both indexes must survive; still registered: {after:?}"
    );
    guard.assert_unchanged("remove_with_no_argument_and_env_only_refuses");
}

// ─── Error-arm tests (Fail-Open Check) ──────────────────────────────────────

/// (a) PATH and `-i` both given, daemon down (discovery points at a port that
/// is provably closed): `remove` must exit non-zero, and the real router —
/// which discovery deliberately does NOT point at — must receive nothing at
/// all, `DELETE` included.
///
/// Why: closes the daemon-guard's own "is anything listening?" gap. A
/// `daemon.lock` naming PID 1 (always alive, same trick
/// `daemon_env_precedence.rs` uses) makes `ensure_daemon_running_or_exit`
/// take its "already running, waiting" branch instead of auto-spawning a
/// real daemon subprocess — undesirable in a test even when scoped to an
/// isolated `TRUSTY_DATA_DIR`. A `daemon.port` file pins the health-probe
/// fallback to the SAME closed port rather than the compiled-in default
/// (7878), so this can never accidentally probe a real daemon that happens
/// to be listening on the default port on this machine.
/// What: binds a loopback listener, drops it (closes it), and points both
/// discovery files at that now-dead address.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_path_and_flag_when_daemon_is_down_refuses_with_no_requests() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    // A real router DOES run, so we can prove it received nothing — but
    // discovery below points elsewhere, at a closed port.
    let (_base, log) = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), false).await;

    let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a throwaway listener to mint a closed port");
    let closed_port = closed_listener.local_addr().expect("local addr").port();
    drop(closed_listener); // now provably closed — nothing is listening

    std::fs::create_dir_all(data_dir.path()).expect("create scratch TRUSTY_DATA_DIR");
    std::fs::write(
        data_dir.path().join("http_addr"),
        format!("127.0.0.1:{closed_port}"),
    )
    .expect("write http_addr");
    std::fs::write(data_dir.path().join("daemon.port"), closed_port.to_string())
        .expect("write daemon.port");
    // PID 1 (init) is always alive — matches daemon_env_precedence.rs's
    // established trick for making the guard skip auto-spawn.
    std::fs::write(data_dir.path().join("daemon.lock"), b"1").expect("write daemon.lock");

    let mut cmd = remove_command(data_dir.path(), fake_home.path());
    cmd.arg(root_a.path())
        .arg("--index")
        .arg(INDEX_B)
        .arg("--yes");
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "a down daemon must refuse, not silently pick a target; output:\n{output}"
    );
    assert!(
        log.snapshot().is_empty(),
        "a down daemon must receive NO request at all: {:?}",
        log.snapshot()
    );
    guard
        .assert_unchanged("remove_with_path_and_flag_when_daemon_is_down_refuses_with_no_requests");
}

/// (b) PATH and `-i` both given, the daemon is reachable but answers every
/// per-index status lookup with `503` — the lookup of the `-i` id during the
/// agreement check. `remove` must exit non-zero with NO `DELETE` ever sent.
///
/// Why: with `--index` set, `handle_index_remove` resolves the id through
/// `find_index_by_id`, whose `error_for_status()?` propagates the `503` as a
/// hard error before the id-agreement
/// comparison (and therefore before any `DELETE`) is ever reached. This pins
/// that existing propagation against a REAL 503, not just a synthetic
/// `Result::Err` in a unit test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_with_path_and_flag_when_status_503s_refuses_with_no_delete() {
    let root_a = tempfile::tempdir().expect("root A");
    let root_b = tempfile::tempdir().expect("root B");
    let data_dir = tempfile::tempdir().expect("scratch TRUSTY_DATA_DIR");
    let fake_home = tempfile::tempdir().expect("fake HOME");
    let guard = RealAllowlistGuard::capture();

    let (base, log) = spawn_daemon_with_two_indexes(root_a.path(), root_b.path(), true).await;
    write_discovery_file(data_dir.path(), &base);

    let mut cmd = remove_command(data_dir.path(), fake_home.path());
    cmd.arg(root_a.path())
        .arg("--index")
        .arg(INDEX_B)
        .arg("--yes");
    let (code, output) = run(cmd);

    assert_ne!(
        code, 0,
        "a 503 during PATH resolution must refuse, not silently pick a target; \
         output:\n{output}"
    );
    let requests = log.snapshot();
    assert!(
        !requests.is_empty(),
        "the daemon must have been contacted (GET /indexes, GET .../status)"
    );
    assert!(
        !log.contains_delete(),
        "NO DELETE must ever be sent when PATH resolution 503s: {requests:?}"
    );

    let after = list_indexes(&base).await;
    assert!(
        after.contains(&INDEX_A.to_string()) && after.contains(&INDEX_B.to_string()),
        "both indexes must survive; still registered: {after:?}"
    );
    guard.assert_unchanged("remove_with_path_and_flag_when_status_503s_refuses_with_no_delete");
}
