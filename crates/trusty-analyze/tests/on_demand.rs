//! End-to-end proof that trusty-analyze runs on demand and ends itself (#6350).
//!
//! Why these live here rather than in `trusty-common`: the shared entry point
//! can be unit-tested for its spawn spec and its timing budget, but the two
//! claims #6350 actually makes are about a REAL process — that a server nobody
//! talks to exits, and that two concurrent clients end up with one server rather
//! than two. Both need a binary that binds a socket, and this is the only crate
//! that has one.
//!
//! What the fixtures supply: `trusty-analyze serve` refuses to start when
//! trusty-search is unreachable, so [`StubSearch`] answers `/health` on a
//! loopback port for the duration of a test. Everything else — the socket, the
//! facts store, the idle window — is pointed inside a tempdir, so no test
//! touches a developer's real data directory or their running daemon.
//!
//! Test: this *is* the test file.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpListener;

/// A loopback `/health` responder standing in for trusty-search.
///
/// Why: `run_serve` exits 1 when `search.health()` is false, which would make
/// every spawn in this file fail for a reason that has nothing to do with the
/// behaviour under test. Answering one 2xx on `/health` is the whole contract
/// these tests need from trusty-search.
///
/// Why axum and not a hand-written responder: `TrustySearchClient` builds its
/// reqwest client with `http2_prior_knowledge()`, so it opens the connection by
/// writing the h2 preface and never reads an HTTP/1.1 reply. `axum::serve` runs
/// hyper's auto `Builder`, which recognises that preface — a raw
/// `HTTP/1.1 200 OK` produces exactly the "trusty-search is not reachable"
/// error a missing daemon would.
struct StubSearch {
    base_url: String,
    _task: tokio::task::JoinHandle<()>,
}

impl StubSearch {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let addr = listener.local_addr().expect("addr");
        // `/health` is what `run_serve` gates its startup on; `/indexes` is what
        // `analyze.index_list` proxies to, and a 404 there surfaces as an RPC
        // error the adapter reads as `Unreachable` — indistinguishable from a
        // dead server, which is exactly the verdict these tests must be able to
        // tell apart. An empty listing is the honest answer for a tempdir.
        let app = axum::Router::new()
            .route(
                "/health",
                axum::routing::get(|| async { axum::Json(serde_json::json!({ "status": "ok" })) }),
            )
            .route(
                "/indexes",
                axum::routing::get(|| async { axum::Json(serde_json::json!({ "indexes": [] })) }),
            );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base_url: format!("http://{addr}"),
            _task: task,
        }
    }
}

/// Spawn `trusty-analyze serve` the way a client would, under a tempdir.
///
/// The `--socket` and `--facts-path` overrides are what keep the test off the
/// developer's real data directory; `TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS` is what
/// makes an idle exit observable in seconds rather than minutes.
fn spawn_server(dir: &Path, search: &StubSearch, idle_secs: u64) -> (PathBuf, std::process::Child) {
    spawn_server_with_store(dir, search, idle_secs, "store")
}

/// [`spawn_server`] with the child's stores in their own subdirectory.
///
/// 🔴 Why the whole DIRECTORY moves, not just the facts filename: the server
/// opens two redb files, and `overlay_path_beside_facts` derives the second one
/// as a fixed `scip_overlays.redb` NEXT TO the first. Two children given
/// `a.redb` and `b.redb` in one directory therefore still collide on the
/// overlay, and redb's exclusive lock makes the loser exit there — before it
/// ever reaches its bind. A race test written that way passes while proving
/// nothing about `bind_singleton_hardened`, which is the thing under test. The
/// socket stays in the shared parent, because the race is what the two children
/// contend for.
fn spawn_server_with_store(
    dir: &Path,
    search: &StubSearch,
    idle_secs: u64,
    store_dir: &str,
) -> (PathBuf, std::process::Child) {
    let socket = dir.join("trusty-analyze.sock");
    let stores = dir.join(store_dir);
    std::fs::create_dir_all(&stores).expect("create the child's store directory");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_trusty-analyze"))
        .arg("--facts-path")
        .arg(stores.join("facts.redb"))
        .args(["serve", "--socket"])
        .arg(&socket)
        .env("TRUSTY_SEARCH_URL", &search.base_url)
        .env("TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS", idle_secs.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn trusty-analyze serve");
    (socket, child)
}

/// Put the binary under test at the front of `PATH`.
///
/// 🔴 Why every test that lets `ensure_running` spawn must call this first:
/// `OnDemandAnalyze` locates its program with
/// `trusty_common::bin_resolve::resolve_binary`, which consults the live `PATH`
/// and then the well-known bin directories. On any machine with trusty-analyze
/// installed — which is every developer machine, and any CI image that ran
/// `cargo install` — that resolves the INSTALLED binary, so the test spawns a
/// build of some other commit and reports on it. That is not a hypothetical:
/// before this helper existed, the concurrency test's child logged a startup
/// line with no `idle` field at all, because the binary it ran predated #6350.
///
/// What: prepends `CARGO_BIN_EXE_trusty-analyze`'s directory to `PATH`.
/// Idempotent, so tests may call it in any order.
///
/// SAFETY: `PATH` is read by `resolve_binary` and by every `Command` spawn, and
/// this only ever PREPENDS — no test in this file depends on the original first
/// entry, and prepending twice is a no-op in effect.
fn use_the_binary_under_test() {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_trusty-analyze"));
    let dir = exe.parent().expect("the test binary has a directory");
    let current = std::env::var("PATH").unwrap_or_default();
    if current.starts_with(&format!("{}:", dir.display())) {
        return;
    }
    unsafe { std::env::set_var("PATH", format!("{}:{current}", dir.display())) };
}

/// Poll until `socket` answers, or give up after `budget`.
async fn await_serving(socket: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if trusty_common::uds::socket_is_serving(socket, Duration::from_millis(200)).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Why: this is the closure condition of #6350 — the server ends itself, and it
/// ends CLEANLY, leaving no socket file for the next spawn to trip over. A
/// server that exited but left the path occupied would push every later start
/// onto `bind_singleton_hardened`'s takeover branch, which exists for a crash,
/// not for the normal end of a lifetime.
/// Test: this is the test.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_exits_on_its_own_idle_window() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let search = StubSearch::start().await;
    let (socket, mut child) = spawn_server(tmp.path(), &search, 2);

    assert!(
        await_serving(&socket, Duration::from_secs(30)).await,
        "the server never began serving {}",
        socket.display()
    );

    let deadline = Instant::now() + Duration::from_secs(60);
    let exited = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => break None,
            None => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    };

    let Some(status) = exited else {
        let _ = child.kill();
        panic!("the server did not exit within 60s of a 2s idle window");
    };
    assert!(status.success(), "an idle exit must be clean: {status:?}");
    assert!(
        !socket.exists(),
        "an idle exit must unlink its socket; {} is still there",
        socket.display()
    );
}

/// Why: the fail-open branch this change must not have. A start that cannot
/// succeed has to reach the caller as an error it can report, never as a path
/// the caller then dials into a confusing transport failure.
///
/// What makes it deterministic: `resolve_binary` consults `PATH` and then the
/// well-known bin directories, so on a machine with trusty-analyze installed
/// the missing-binary branch is unreachable — and pointing `TRUSTY_SEARCH_URL`
/// at nothing is what forces a failure that does not depend on the machine.
/// `run_serve` refuses to start when trusty-search is unreachable, so the child
/// exits before binding and `ensure_running` reports `SpawnTimeout`. Without
/// this the test passed or failed according to whether the developer happened
/// to have trusty-search running.
/// Test: this is the test.
#[serial_test::serial]
#[tokio::test]
async fn a_server_that_cannot_start_is_an_error_not_a_success() {
    use_the_binary_under_test();
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("absent.sock");
    // Off the real data directory: a developer machine with a live analyze
    // daemon holds an exclusive redb lock on it, which would fail the spawned
    // child for a reason that has nothing to do with the property under test.
    // SAFETY: set before the spawn below; no other test in this file reads it
    // before setting its own.
    unsafe {
        std::env::set_var("TRUSTY_ANALYZER_FACTS", tmp.path().join("facts.redb"));
        // Port 1 is privileged and unbound: the child's startup probe of
        // trusty-search always refuses, so it always exits before its bind.
        std::env::set_var("TRUSTY_SEARCH_URL", "http://127.0.0.1:1");
    }

    let handle = trusty_common::uds::OnDemandAnalyze::at(&socket);
    let err = tokio::time::timeout(Duration::from_secs(60), handle.ensure_running())
        .await
        .expect("ensure_running must not hang")
        .expect_err("the child cannot start: its trusty-search probe always refuses");

    let rendered = format!("{err}");
    assert!(
        !rendered.is_empty(),
        "the failure must carry a message a caller can print"
    );
    assert!(
        !socket.exists(),
        "a failed start must not leave a socket implying success"
    );
}

/// Why: the concurrency claim. Two callers that race must end up with ONE
/// server — in-process the spawn gate serialises them and the second adopts the
/// first's socket; across processes `bind_singleton_hardened` refuses the loser.
/// A test that spawned two servers would show up here as two different pids
/// answering, or as a caller that got an error while a healthy server was up.
/// Test: this is the test.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_callers_share_one_server() {
    use_the_binary_under_test();
    let tmp = tempfile::tempdir().expect("tempdir");
    let search = StubSearch::start().await;
    let socket = tmp.path().join("trusty-analyze.sock");

    // The child must find trusty-search and its own facts store, and both are
    // per-test. `ensure_running` spawns with the parent's environment, so
    // setting them here is what the child inherits.
    // SAFETY: this test binary sets these once, before any spawn, and no other
    // test in this file reads them.
    unsafe {
        std::env::set_var("TRUSTY_SEARCH_URL", &search.base_url);
        std::env::set_var("TRUSTY_ANALYZER_FACTS", tmp.path().join("facts.redb"));
        std::env::set_var("TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS", "120");
    }

    let handle = Arc::new(trusty_common::uds::OnDemandAnalyze::at(&socket));
    let a = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.ensure_running().await }
    });
    let b = tokio::spawn({
        let handle = Arc::clone(&handle);
        async move { handle.ensure_running().await }
    });

    let (ra, rb) = tokio::join!(a, b);
    let pa = ra.expect("join a").expect("first caller must get a server");
    let pb = rb
        .expect("join b")
        .expect("second caller must get a server");

    assert_eq!(pa, socket);
    assert_eq!(
        pb, socket,
        "both callers must be pointed at the same socket"
    );
    assert!(
        trusty_common::uds::socket_is_serving(&socket, Duration::from_secs(2)).await,
        "one server must be left serving after both calls returned"
    );

    // Exactly one process is answering: a second server would have had to bind
    // the same path, and `bind_singleton_hardened` refuses that while the first
    // is live. Assert it directly by counting the processes that hold it.
    let holders = std::process::Command::new("lsof")
        .arg("-t")
        .arg(&socket)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .count()
        })
        .unwrap_or(1);
    assert!(
        holders <= 1,
        "{holders} processes are serving one socket; the singleton bind failed"
    );

    // Stop the server rather than waiting out its window: the tempdir is about
    // to be removed, and a live child holding a path inside it is a leak.
    let _ = std::process::Command::new("pkill")
        .arg("-f")
        .arg(socket.to_string_lossy().as_ref())
        .status();
}

/// Why: `ServiceTimeouts`' sourcing rule says the supervisor's `shutdown_flush`
/// must be the supervised binary's REAL budget, and `trusty-common` cannot
/// import this crate to read it. This equality is what turns that rule from a
/// comment into a check — the same shape trusty-memory's
/// `sigterm_patience_exceeds_the_daemon_flush_budget` uses.
/// Test: this is the test.
#[test]
fn analyze_flush_budget_matches_the_supervisor_contract() {
    assert_eq!(
        trusty_common::uds::ANALYZE_SHUTDOWN_FLUSH,
        trusty_analyze::service::SHUTDOWN_FLUSH_TIMEOUT,
        "the supervisor's flush budget must be this server's own, not a literal \
         that happens to match it today"
    );
}

/// Why (#6350): the cross-process half of "two callers, one server". The
/// in-process spawn gate cannot help two separate processes that probe an empty
/// socket in the same instant; what arbitrates there is
/// `bind_singleton_hardened`, which takes over only a path the kernel proves
/// nobody is serving. If both children could bind, the second would unlink the
/// first's socket and every client holding it would be talking to a process
/// nothing can reach.
///
/// What: races two real `trusty-analyze serve` children on one empty socket
/// directory and asserts the socket answers with exactly ONE of them still
/// alive — the loser must have exited on its bind rather than clobbering the
/// winner.
/// Test: this is the test.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_racing_server_processes_leave_exactly_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let search = StubSearch::start().await;

    // Started back to back with no await between, so both reach their bind
    // inside the same few milliseconds.
    let (socket, mut first) = spawn_server_with_store(tmp.path(), &search, 120, "first");
    let (_same, mut second) = spawn_server_with_store(tmp.path(), &search, 120, "second");

    assert!(
        await_serving(&socket, Duration::from_secs(30)).await,
        "one of the two must win the bind and serve {}",
        socket.display()
    );

    // Give the loser time to fail its bind and exit. It cannot be identified in
    // advance, so both are polled and the survivors counted.
    let deadline = Instant::now() + Duration::from_secs(30);
    let alive = loop {
        let a = first.try_wait().expect("try_wait").is_none();
        let b = second.try_wait().expect("try_wait").is_none();
        let alive = usize::from(a) + usize::from(b);
        if alive <= 1 || Instant::now() >= deadline {
            break alive;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    let _ = first.kill();
    let _ = second.kill();

    assert_eq!(
        alive, 1,
        "exactly one server may hold the socket; {alive} processes survived the race"
    );
    assert!(
        !socket.exists()
            || trusty_common::uds::socket_is_serving(&socket, Duration::from_secs(1)).await,
        "the loser must not have unlinked the winner's socket"
    );
}

/// Why (#6350): `HttpAnalyzeMetricsSource` starts the server once per source,
/// but a multi-repository report can outlive the idle window — the diagnostics
/// endpoint alone carries a multi-minute budget. Without a retry, every fetch
/// after the exit hard-fails and the rest of the report is silently scanned
/// instead of analysed. This is the property that closes that gap.
///
/// What: drives the REAL adapter against a real server with a two-second idle
/// window, waits for the server to reclaim itself between two fetches, and
/// asserts the second fetch still reaches a server. `NotIndexed` is the
/// expected verdict for both — the tempdir has no index — and it is the
/// interesting one: it can only be reached by a server that ANSWERED
/// `analyze.index_list`. `Unreachable` is what a missing retry produces.
/// Test: this is the test.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_adapter_respawns_a_server_that_idled_out() {
    use trusty_review::report::{AnalyzeFetch, AnalyzeMetricsSource as _};

    use_the_binary_under_test();
    let tmp = tempfile::tempdir().expect("tempdir");
    let search = StubSearch::start().await;
    let socket = tmp.path().join("trusty-analyze.sock");

    // `ensure_running` spawns with this process's environment, so the child
    // finds the stub search, its own facts store, and a short idle window here.
    // SAFETY: set once, before any spawn; no other test in this file reads them.
    unsafe {
        std::env::set_var("TRUSTY_SEARCH_URL", &search.base_url);
        std::env::set_var("TRUSTY_ANALYZER_FACTS", tmp.path().join("facts.redb"));
        std::env::set_var("TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS", "2");
    }

    let source = trusty_review::report::HttpAnalyzeMetricsSource::new(&socket)
        .expect("infallible since #6287");

    let first = source.fetch_named("absent-index").await;
    assert!(
        matches!(first, AnalyzeFetch::Missing(gap) if gap.as_str().contains("not built")),
        "the first fetch must reach a server that answers index_list"
    );

    // Wait out the idle window: the server reclaims itself and unlinks.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if !trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(200)).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        !trusty_common::uds::socket_is_serving(&socket, Duration::from_millis(500)).await,
        "the server must have idled out for this test to prove anything"
    );

    let second = source.fetch_named("absent-index").await;
    match second {
        AnalyzeFetch::Missing(gap) => assert!(
            gap.as_str().contains("not built"),
            "the second fetch must have reached a RESTARTED server, not given up: {}",
            gap.as_str()
        ),
        AnalyzeFetch::Fetched { .. } => panic!("no index exists; nothing could have been fetched"),
        // `AnalyzeFetch` is `#[non_exhaustive]`; a variant added later is not
        // this test's verdict either way, so it fails loudly rather than
        // silently passing on a shape nobody has considered.
        other => panic!("unexpected fetch verdict: {other:?}"),
    }

    let _ = std::process::Command::new("pkill")
        .arg("-f")
        .arg(socket.to_string_lossy().as_ref())
        .status();
}
