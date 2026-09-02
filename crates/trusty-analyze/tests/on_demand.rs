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

/// Why (#6595): the unlink is what tells a client this server is gone, and the
/// client's answer is to spawn a successor that opens the same two redb files.
/// redb locks each file exclusively, so a lock released AFTER the unlink hands
/// the successor `Database already open. Cannot acquire lock.`; the successor
/// dies before it binds, `Supervisor::ensure_running` never notices, and the
/// caller waits out the whole 20s `spawn_probe` for a `SpawnTimeout`. That is
/// the failure this test exists to keep out — it took
/// `the_adapter_respawns_a_server_that_idled_out` red on two consecutive main
/// runs.
///
/// What makes it deterministic rather than load-dependent: the unlink is the
/// event, not a duration. A blocking spin watches the path with no sleep, so it
/// observes the unlink within microseconds, and the open runs at that instant.
/// Load lengthens the window this catches; it cannot close it. Against the
/// pre-fix ordering this failed 15 rounds out of 15, with the lock held for
/// 54–560 ms and `lsof` naming the exiting server as its only holder.
/// Test: this is the test.
// `serial` for the single-process `cargo test` run, not for a fixture: this
// spawns a child, and a sibling's `set_var` on PATH / TRUSTY_* would race that
// spawn's read of the environment (#6542). Under nextest it is a no-op.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_exit_frees_its_redb_locks_before_it_unlinks_the_socket() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let search = StubSearch::start().await;
    let (socket, mut child) = spawn_server(tmp.path(), &search, 1);
    let facts = tmp.path().join("store").join("facts.redb");

    assert!(
        await_serving(&socket, Duration::from_secs(30)).await,
        "the server never began serving {}",
        socket.display()
    );

    // A blocking task, so the spin cannot starve the runtime this test needs.
    let watched = socket.clone();
    let probed = facts.clone();
    let verdict = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(60);
        while watched.exists() && Instant::now() < deadline {
            std::hint::spin_loop();
        }
        assert!(!watched.exists(), "the server never unlinked its socket");
        trusty_analyze::core::FactStore::open(&probed).map(|_| ())
    })
    .await
    .expect("the spin task");

    let _ = child.kill();
    let _ = child.wait();

    if let Err(e) = verdict {
        panic!(
            "a successor spawned at the unlink cannot open {}: {e:#}",
            facts.display()
        );
    }
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
///
/// #6601 review: since `SHUTDOWN_FLUSH_TIMEOUT` became an alias of
/// `ANALYZE_SHUTDOWN_FLUSH`, the first assertion cannot fail — which is the
/// point, the drift is now unrepresentable rather than merely detected. The
/// second is the one that still has work to do: it pins the value the SUPERVISOR
/// was configured with, which a `ServiceTimeouts::new` call site could otherwise
/// pass a literal to. What binds the SERVE LOOP to the same number is
/// `serve_options_bind_the_shutdown_drain_to_this_services_own_budget`, in
/// `rpc_tests.rs` — that link was the one missing when this test was written,
/// and it is what let a 5 s patience sit under a 60 s drain.
/// Test: this is the test.
#[test]
fn analyze_flush_budget_matches_the_supervisor_contract() {
    assert_eq!(
        trusty_common::uds::ANALYZE_SHUTDOWN_FLUSH,
        trusty_analyze::service::SHUTDOWN_FLUSH_TIMEOUT,
        "the supervisor's flush budget must be this server's own, not a literal \
         that happens to match it today"
    );
    assert_eq!(
        trusty_common::uds::on_demand::ANALYZE_TIMEOUTS.shutdown_flush,
        trusty_analyze::service::SHUTDOWN_FLUSH_TIMEOUT,
        "the timeouts handed to the supervisor must carry that same budget"
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

    // #6411: probe while the winner is still RUNNING. Asking after the kills
    // below measures the kill, not the race — a SIGKILLed winner stops serving
    // and leaves its path behind, so the old check passed only when the connect
    // beat signal delivery and failed ~20% of the time on a loaded CI runner.
    let winner_still_serving =
        trusty_common::uds::socket_is_serving(&socket, Duration::from_secs(1)).await;

    let _ = first.kill();
    let _ = second.kill();

    assert_eq!(
        alive, 1,
        "exactly one server may hold the socket; {alive} processes survived the race"
    );
    // A bare "still serving" is what the property needs, and it is stricter than
    // the tolerate-a-missing-path form this replaces: an unlinked socket with
    // the winner still alive IS the defect under test, so accepting it as a pass
    // let the one outcome this test exists to catch through.
    assert!(
        winner_still_serving,
        "the loser must not have unlinked the winner's socket: {} is no longer served",
        socket.display()
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

/// One `trusty-analyze serve --mcp` process, driven the way an MCP client does.
///
/// Why a fixture rather than an inline spawn: the property under test needs the
/// stdio loop and the socket the SAME process binds, and it needs both still
/// there after a silence. Holding stdin open is what keeps the loop alive — it
/// ends when stdin closes — so the handle owns stdin for the length of the test
/// rather than writing to it and dropping it.
///
/// `kill_on_drop` is what keeps a panicking assertion from leaking a child that
/// holds a path inside the tempdir the test is about to remove.
struct McpSession {
    _child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    socket: PathBuf,
}

impl McpSession {
    /// Spawn `serve --mcp` under `dir`, with `idle_secs` as the daemon's window.
    ///
    /// The `--facts-path` and `--socket` overrides keep the child off the
    /// developer's real data directory, exactly as [`spawn_server`] does.
    fn start(dir: &Path, search: &StubSearch, idle_secs: u64) -> Self {
        use tokio::io::AsyncBufReadExt as _;

        let socket = dir.join("trusty-analyze.sock");
        let stores = dir.join("mcp-store");
        std::fs::create_dir_all(&stores).expect("create the child's store directory");
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_trusty-analyze"))
            .arg("--facts-path")
            .arg(stores.join("facts.redb"))
            .args(["serve", "--mcp", "--socket"])
            .arg(&socket)
            .env("TRUSTY_SEARCH_URL", &search.base_url)
            .env("TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS", idle_secs.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn trusty-analyze serve --mcp");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = tokio::io::BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        Self {
            _child: child,
            stdin,
            stdout,
            socket,
        }
    }

    /// Send one JSON-RPC request and read its response line.
    ///
    /// The exchange is in lockstep — one line out, one line in — which is what
    /// the MCP stdio contract is, and what lets a stalled loop surface as an
    /// explicit timeout rather than as a hung test.
    async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        use tokio::io::AsyncWriteExt as _;

        let line = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .expect("serialize the request");
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write the request to the child's stdin");
        self.stdin.flush().await.expect("flush stdin");

        let raw = tokio::time::timeout(Duration::from_secs(60), self.stdout.next_line())
            .await
            .unwrap_or_else(|_| panic!("no answer to {method} within 60s"))
            .expect("read the child's stdout")
            .unwrap_or_else(|| panic!("the child closed stdout before answering {method}"));
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{method} answered non-JSON: {e}: {raw}"))
    }

    /// Call `analyzer_health` and return the tool result, asserting it worked.
    ///
    /// 🔴 Why `isError` is the assertion and not a JSON-RPC `error` member:
    /// `tools/call` maps the dispatcher's `DispatchError::Transport` onto a
    /// SUCCESSFUL response carrying `isError: true` (`mcp::helpers::wrap_tool_error`),
    /// so a dead socket reaches a client as an ok-shaped frame. A test that only
    /// checked for a JSON-RPC `error` would pass against a daemon that had
    /// already exited.
    async fn health(&mut self, id: u64) -> serde_json::Value {
        let response = self
            .request(
                id,
                "tools/call",
                serde_json::json!({ "name": "analyzer_health", "arguments": {} }),
            )
            .await;
        assert!(
            response.get("error").is_none(),
            "analyzer_health must not answer a JSON-RPC error: {response}"
        );
        let result = response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("analyzer_health answered without a result: {response}"));
        assert_eq!(
            result.get("isError").and_then(serde_json::Value::as_bool),
            Some(false),
            "analyzer_health reached no daemon; the socket the stdio loop dials is gone: {result}"
        );
        result
    }
}

/// Why (#6355): the `--mcp` branch spawns the daemon in a task and then runs the
/// stdio loop in the foreground, and `mcp::rpc_client::call` dials that socket
/// once per tool call with no respawn. With the idle window applied to that
/// daemon, a session that went quiet past the window lost its transport for the
/// rest of the process's life: the daemon task exited and unlinked the socket
/// while the loop stayed connected to its client over stdin/stdout, and every
/// later tool call came back `isError` with a transport failure. Nothing
/// recovers from that — the client's only cure is to restart the server.
///
/// What: drives a real `serve --mcp` child with a two-second window, answers one
/// tool call, stays silent for six windows, then asserts both that the socket is
/// still served and that a second tool call still reaches a daemon.
///
/// 🔴 The silence is a bare sleep, deliberately — no liveness polling inside it.
/// A probe connection is an OPEN connection, and `IdleTracker::expired` sleeps a
/// whole further window whenever it observes one, so polling would push the idle
/// exit out and let this test pass against the wiring it exists to refuse.
///
/// Against `9d8cc6a9a` this fails at the `still_serving` assertion: the socket is
/// gone roughly two seconds into the silence.
/// Test: this is the test.
#[serial_test::serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_mcp_session_outlives_the_idle_window() {
    const IDLE_SECS: u64 = 2;
    /// Six windows: long enough that an idle exit has certainly happened by now,
    /// short enough to stay an ordinary test cost.
    const SILENCE: Duration = Duration::from_secs(12);

    let tmp = tempfile::tempdir().expect("tempdir");
    let search = StubSearch::start().await;
    let mut session = McpSession::start(tmp.path(), &search, IDLE_SECS);

    let initialized = session
        .request(1, "initialize", serde_json::Value::Null)
        .await;
    assert!(
        initialized.get("result").is_some(),
        "the stdio loop must initialize before anything else is proven: {initialized}"
    );

    let socket = session.socket.clone();
    assert!(
        await_serving(&socket, Duration::from_secs(30)).await,
        "the --mcp child never bound {}",
        socket.display()
    );

    // The "before" call. It also refreshes the idle window, so the silence below
    // is measured from a known instant rather than from the bind.
    session.health(2).await;

    tokio::time::sleep(SILENCE).await;

    let still_serving =
        trusty_common::uds::socket_is_serving(&socket, Duration::from_secs(2)).await;
    assert!(
        still_serving,
        "the daemon behind a live MCP session exited after {}s of silence; the --mcp \
         branch must not apply an idle window (#6355)",
        SILENCE.as_secs()
    );

    // The claim is about the DISPATCHER, not just the socket file: this is the
    // call that used to come back `isError` for the rest of the session.
    session.health(3).await;
}
