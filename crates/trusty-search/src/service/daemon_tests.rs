//! Unit tests for `service::daemon` (extracted from `daemon.rs` to keep the
//! production file under the 500-SLOC cap — mirrors the `persistence_tests.rs`
//! split, issue #1372's pattern).
//!
//! Why: `daemon.rs` carries the lockfile and data-dir resolution and the
//! `run_daemon()` entry point itself; its tests outgrew the production SLOC
//! cap. Splitting them into this sibling `#[path]`-included module restores
//! compliance without changing coverage.
//! What: lockfile/PID-liveness tests, the `TRUSTY_DATA_DIR` path-resolution
//! regression tests (issue #3545), the shared-discovery deregistration test
//! (issue #3602 review), and the socket-only daemon tests (#9214).
//! Test: this module IS the tests.

use super::*;
use serial_test::serial;

#[test]
fn pid_alive_current_process_is_alive() {
    // Why: smoke-test the PID-aliveness predicate so the launchd
    // crash-loop fix has explicit coverage. Our own PID must register
    // as alive; a clearly-invalid PID must not.
    assert!(pid_alive(std::process::id()));
    // Find a clearly-dead PID. macOS `pid_max` defaults to 99999 and
    // Linux to 4194304; on both, a value just under i32::MAX is well
    // beyond the legal range and `kill(pid, 0)` returns ESRCH.
    // (u32::MAX would narrow to -1 on i32 cast, which `kill` interprets
    // as "every process the caller can signal" — never ESRCH.)
    assert!(!pid_alive(2_000_000_000));
}

#[test]
fn read_lockfile_pid_parses_pid() {
    // Why: `running_daemon_pid` depends on this parser. A malformed
    // file must return None rather than panic.
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.lock");
    std::fs::write(&good, "12345\n").unwrap();
    assert_eq!(read_lockfile_pid(&good), Some(12345));

    let bad = dir.path().join("bad.lock");
    std::fs::write(&bad, "not-a-pid").unwrap();
    assert_eq!(read_lockfile_pid(&bad), None);
}

#[test]
fn lockfile_contention_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    let _first = acquire_lock(&path).unwrap();
    let err = acquire_lock(&path).unwrap_err();
    assert!(matches!(err, DaemonError::AlreadyRunning(_)));
}

/// Why: `daemon_dir()` must respect `TRUSTY_DATA_DIR` so an isolated daemon
/// can run alongside the production daemon without lockfile conflicts (#281).
/// What: set env var to a tempdir path; assert `daemon_dir()` returns it.
/// Test: `daemon_dir_respects_trusty_data_dir_env_var` (this test).
///
/// `#[serial]` is required because this test mutates the `TRUSTY_DATA_DIR`
/// process env var; running it concurrently with other `TRUSTY_DATA_DIR`
/// mutations in `daemon_paths_under_data_dir_override` or the `start.rs`
/// auto-discover tests causes a flaky race condition.
#[test]
#[serial]
fn daemon_dir_respects_trusty_data_dir_env_var() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().to_path_buf();
    // SAFETY: test-only, single-threaded portion; no other thread reads
    // TRUSTY_DATA_DIR in this test binary at the same time.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", &override_path);
    }
    let result = daemon_dir();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
    let dir = result.expect("daemon_dir with TRUSTY_DATA_DIR should succeed");
    assert_eq!(dir, override_path, "daemon_dir should return the override");
    assert!(dir.exists(), "daemon_dir should create the directory");
}

/// Why: `daemon_lock_path()` and `daemon_port_path()` (service side) must
/// land under the override directory, not the platform default.
/// What: set env var, call both path functions, confirm they start with the
/// override root rather than the default data-local dir.
/// Test: `daemon_paths_under_data_dir_override` (this test).
///
/// `#[serial]` is required because this test mutates the `TRUSTY_DATA_DIR`
/// process env var; running it concurrently with other `TRUSTY_DATA_DIR`
/// mutations (e.g. `daemon_dir_respects_trusty_data_dir_env_var` or the
/// `start.rs` auto-discover tests) causes a flaky race on the env-var read
/// inside `daemon_dir()`.
#[test]
#[serial]
fn daemon_paths_under_data_dir_override() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", &override_path);
    }
    let lock = daemon_lock_path();
    let port = daemon_port_path();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
    let lock = lock.expect("lock path must resolve");
    let port = port.expect("port path must resolve");
    assert!(
        lock.starts_with(&override_path),
        "lock path {lock:?} should be under override {override_path:?}"
    );
    assert!(
        port.starts_with(&override_path),
        "port path {port:?} should be under override {override_path:?}"
    );
}

/// Regression for issue #3545: the stale `http_addr` an older isolated
/// daemon wrote lives under `TRUSTY_DATA_DIR`, so that is where the
/// socket-only daemon removes it (#9214) — never the shared `$HOME` file.
/// What: set `TRUSTY_DATA_DIR` to a tempdir; assert the returned path is
/// `{tempdir}/http_addr`, not the `$HOME`-relative default.
/// Test: `legacy_http_addr_path_respects_trusty_data_dir` (this test).
///
/// `#[serial]`: the env var is process-global.
#[test]
#[serial]
fn legacy_http_addr_path_respects_trusty_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", &override_path);
    }
    let path = legacy_http_addr_path();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
    assert_eq!(
        path,
        Some(override_path.join("http_addr")),
        "the stale http_addr lives under the TRUSTY_DATA_DIR override, not $HOME"
    );
}

/// Regression for the #3602 review finding, kept by #9214: the default
/// instance clears the shared registry entry an older build wrote, because
/// this build publishes no address and the entry can only name a dead port.
///
/// Why safe: same `TRUSTY_DATA_DIR_OVERRIDE` redirection as the writer
/// tests above.
/// What: seed the registry, unset `TRUSTY_DATA_DIR`, call
/// `deregister_shared_discovery`, assert the entry is gone.
/// Test: this function.
#[test]
#[serial]
fn deregister_shared_discovery_removes_when_default_instance() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
    }
    trusty_common::write_daemon_addr("trusty-search", "127.0.0.1:1").unwrap();
    deregister_shared_discovery();
    let got = trusty_common::read_daemon_addr("trusty-search").unwrap();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }
    assert!(
        got.is_none(),
        "the default instance must clear the stale shared registry entry"
    );
}

/// End-to-end regression for the #3602 review finding, under #9214: a REAL
/// isolated `run_daemon()` must leave the shared, non-isolated discovery
/// registry alone. The registry belongs to the default instance; an isolated
/// daemon that cleared it would erase another daemon's entry.
///
/// Why it is safe to run: with `TRUSTY_DATA_DIR` set, every daemon path
/// resolves under a tempdir, and `TRUSTY_DATA_DIR_OVERRIDE` redirects the
/// shared registry into another, so nothing touches a real daemon.
/// What: seeds the redirected shared registry, runs the isolated daemon until
/// its socket serves, and asserts the entry is untouched and that no
/// `http_addr` file appeared under the data dir.
/// Test: this function.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_daemon_isolated_instance_leaves_shared_discovery_alone() {
    use crate::core::registry::IndexRegistry;
    use crate::service::socket;

    with_isolated_daemon_paths(|data_dir| async move {
        trusty_common::write_daemon_addr("trusty-search", "127.0.0.1:1").unwrap();
        let socket_path = socket::socket_path().expect("resolve the isolated socket path");
        let state = SearchAppState::new(IndexRegistry::new());
        let shutdown_tx = state.shutdown_tx.clone();
        let handle = tokio::spawn(run_daemon(state, None));

        let serving = wait_for_socket(&socket_path).await;
        let shared_during = trusty_common::read_daemon_addr("trusty-search").unwrap();
        let addr_file = data_dir.join("http_addr").exists();
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

        assert!(serving, "the isolated daemon must serve its socket");
        assert_eq!(
            shared_during.as_deref(),
            Some("127.0.0.1:1"),
            "an isolated instance must not touch the shared discovery registry"
        );
        assert!(!addr_file, "the daemon must write no http_addr file");
    })
    .await;
}

/// #9030, #9214: `search.health` reports the transport `run_daemon` bound —
/// its socket, and no HTTP address.
///
/// Why: the console's search dashboard reads `health.transport` to show the
/// live transport instead of a hardcoded port. A value the daemon did not bind
/// would mislead it as badly as the hardcoded one did.
/// What: runs a real isolated `run_daemon` and asks the socket for its health
/// body; `socket_path` must equal the resolved socket path and `http_addr`
/// must be `null`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_daemon_health_reports_the_transport_it_bound() {
    use crate::core::registry::IndexRegistry;
    use crate::service::socket;

    with_isolated_daemon_paths(|_data_dir| async move {
        let socket_path = socket::socket_path().expect("resolve the isolated socket path");
        let state = SearchAppState::new(IndexRegistry::new());
        let shutdown_tx = state.shutdown_tx.clone();
        let handle = tokio::spawn(run_daemon(state, None));

        let serving = wait_for_socket(&socket_path).await;
        let frame: Option<serde_json::Value> = trusty_common::uds::send_framed_request(
            &socket_path,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": socket::METHOD_HEALTH }),
            std::time::Duration::from_secs(10),
        )
        .await
        .ok();
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

        assert!(serving, "the isolated daemon must serve its socket");
        assert_eq!(
            frame.map(|f| f["result"]["transport"].clone()),
            Some(serde_json::json!({
                "socket_path": socket_path.to_string_lossy(),
                "http_addr": null,
            })),
            "search.health over the socket must report the bound transport"
        );
    })
    .await;
}

/// The Fail-Open Check, driven through `run_daemon()` itself (#6285).
///
/// Why: `socket_tests::bind_refuses_a_socket_another_process_is_serving` proves
/// [`crate::service::socket::bind`] returns an `Err`. It does not prove the
/// daemon PROPAGATES it — a `let _ = socket::bind(..)` in `run_daemon` would
/// leave that test green while the daemon degraded to HTTP-only and every
/// consumer of the retire slice got an address with nothing behind it. This is
/// the assertion the slice-1 critic round deferred: the whole entry point runs,
/// against a socket path someone else is already serving, and must exit.
///
/// What: points `TRUSTY_DATA_DIR_OVERRIDE` at a tempdir — the same isolation
/// `run_daemon_isolated_instance_leaves_shared_discovery_alone` relies on, so
/// the lockfile, port file, `http_addr` file and socket all resolve under it
/// and never a real production path. A live listener takes the socket path
/// first, then `run_daemon` is called. Two things are asserted: it returns an
/// `Err` naming the path, and NO `http_addr` file was written — the latter is
/// what makes this about announcing a half-bound daemon rather than only about
/// the return value, since the bind happens before every publish step.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn run_daemon_refuses_a_socket_another_process_is_serving() {
    use crate::core::registry::IndexRegistry;
    use crate::service::socket;

    let override_tmp = tempfile::tempdir().unwrap();
    let data_dir_tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", override_tmp.path());
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    // The incumbent: a REAL listener answering the path this daemon would take,
    // which is what `bind_singleton_hardened` probes for before it reclaims.
    let socket_path = socket::socket_path().expect("resolve the isolated socket path");
    let incumbent = socket::bind(&socket_path)
        .await
        .expect("the incumbent must take the path first");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let incumbent_state = std::sync::Arc::new(SearchAppState::new(IndexRegistry::new()));
    let serving = tokio::spawn(async move {
        socket::serve_until_shutdown(incumbent, incumbent_state, async {
            let _ = stop_rx.await;
        })
        .await;
    });
    for _ in 0..200 {
        if trusty_common::uds::socket_is_serving(&socket_path, std::time::Duration::from_millis(50))
            .await
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let result = run_daemon(SearchAppState::new(IndexRegistry::new()), None).await;

    let isolated_http_addr = data_dir_tmp.path().join("http_addr");
    let published = isolated_http_addr.exists();
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), serving).await;
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }

    let err = result.expect_err("a daemon that cannot bind its socket must not start");
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&socket_path.display().to_string()),
        "the error must name the path an operator has to act on: {rendered}"
    );
    assert!(
        !published,
        "a daemon that failed its socket bind must not have announced itself at {}",
        isolated_http_addr.display()
    );
}

/// The Fail-Open Check for the daemon lock (#8760): a contended lock stops
/// `run_daemon` before it binds or publishes anything.
///
/// Why: `acquire_lock` returning `Err` proves nothing if `run_daemon` downgrades
/// it to a warning and starts anyway. This drives the whole entry point.
/// What: isolates every daemon path under tempdirs, takes the lock from another
/// descriptor while the file names a dead pid, then calls `run_daemon`. It must
/// return `AlreadyRunning` and leave no port or `http_addr` file behind.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn run_daemon_refuses_to_start_while_another_holder_has_the_lock() {
    use crate::core::registry::IndexRegistry;

    let override_tmp = tempfile::tempdir().unwrap();
    let data_dir_tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", override_tmp.path());
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }
    let lock_path = daemon_lock_path().expect("isolated lock path");
    std::fs::write(&lock_path, "2000000000").unwrap();
    let holder = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    holder.try_lock_exclusive().unwrap();

    let result = run_daemon(SearchAppState::new(IndexRegistry::new()), None).await;

    let port_written = daemon_port_path().map(|p| p.exists()).unwrap_or(false);
    let addr_written = data_dir_tmp.path().join("http_addr").exists();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }

    assert!(
        matches!(result, Err(DaemonError::AlreadyRunning(_))),
        "a daemon that lost the lock must not start: {result:?}"
    );
    assert!(
        !port_written && !addr_written,
        "a refused daemon announced itself"
    );
}

/// #4827: a line with no `=` used to be dropped in silence, so a typo cost the
/// operator the setting and reported nothing. The parser must hand malformed
/// lines back with their line numbers so the caller can warn.
#[test]
fn parse_daemon_env_reports_malformed_lines() {
    let (pairs, malformed) = super::parse_daemon_env(
        "# comment\n\
         TRUSTY_MEMORY_LIMIT_MB=512\n\
         \n\
         TRUSTY_MAX_CHUNKS 100000\n\
         TRUSTY_DEVICE = cpu \n",
    );
    assert_eq!(
        pairs,
        vec![
            ("TRUSTY_MEMORY_LIMIT_MB".to_string(), "512".to_string()),
            ("TRUSTY_DEVICE".to_string(), "cpu".to_string()),
        ],
        "keys and values must be trimmed; comments and blanks skipped"
    );
    assert_eq!(
        malformed,
        vec![(4, "TRUSTY_MAX_CHUNKS 100000".to_string())],
        "a line with no `=` must be reported, not swallowed"
    );
}

/// A typo'd credential assignment must not put the secret in the log.
///
/// `daemon.env` holds live provider keys. Writing `OPENROUTER_API_KEY sk-…`
/// instead of `OPENROUTER_API_KEY=sk-…` lands in the malformed arm, which used
/// to log the line verbatim — leaking the key in cleartext to anyone who can
/// read the daemon log.
#[test]
fn malformed_line_summary_redacts_a_typod_credential() {
    let secret = "sk-or-v1-0123456789abcdef0123456789abcdef";
    let summary = super::malformed_line_summary(&format!("OPENROUTER_API_KEY {secret}"));

    assert!(
        !summary.contains(secret),
        "the secret must never reach the log, got: {summary}"
    );
    assert!(
        !summary.contains("sk-or"),
        "not even a prefix of the secret may survive, got: {summary}"
    );
    assert!(
        summary.contains("OPENROUTER_API_KEY"),
        "the key name is the whole point of the diagnostic, got: {summary}"
    );
}

/// A bare token on its own line has no key name to report, so nothing but the
/// length may be logged — the leading token IS the secret in that case.
#[test]
fn malformed_line_summary_redacts_a_bare_secret() {
    let secret = "sk-or-v1-0123456789abcdef";
    let summary = super::malformed_line_summary(secret);

    assert!(
        !summary.contains(secret) && !summary.contains("sk-or"),
        "a bare secret must degrade to a length-only summary, got: {summary}"
    );
    assert_eq!(summary, "25 chars", "got: {summary}");
}

/// The common case — a fumbled separator on an ordinary setting — must still
/// name the setting, or the warning tells the operator nothing actionable.
#[test]
fn malformed_line_summary_names_a_conventional_key() {
    let summary = super::malformed_line_summary("TRUSTY_MAX_CHUNKS 100000");
    assert!(summary.contains("TRUSTY_MAX_CHUNKS"), "got: {summary}");
    assert!(
        !summary.contains("100000"),
        "the value is never this function's to reproduce, got: {summary}"
    );
}

/// #4827: the early pass must skip exactly the keys whose value a CLI flag
/// stamps in after parsing, plus `TRUSTY_DATA_DIR` — which decides where
/// `daemon.env` itself lives, so applying it early would let the production
/// data dir's file redirect a `--data-dir /tmp/isolated` run at production data.
#[test]
fn early_load_skips_flag_derived_keys() {
    for key in [
        "TRUSTY_DATA_DIR",
        "TRUSTY_DEVICE",
        "TRUSTY_SEARCH_FANOUT_CONCURRENCY",
    ] {
        assert!(
            super::EARLY_LOAD_EXCLUDED_ENV.contains(&key),
            "{key} must stay on the post-parse pass so the CLI flag still wins"
        );
    }
    assert!(
        !super::EARLY_LOAD_EXCLUDED_ENV.contains(&"TRUSTY_NO_AUTO_DISCOVER"),
        "TRUSTY_NO_AUTO_DISCOVER is the setting #4827 exists to make work"
    );
}

/// #4827: the daemon path is the only one that may source `daemon.env`.
#[test]
fn argv_selects_daemon_start_for_the_daemon_path() {
    let argv = |args: &[&str]| -> Vec<String> {
        std::iter::once("trusty-search")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect()
    };
    assert!(super::argv_selects_daemon_start(&argv(&["start"])));
    assert!(super::argv_selects_daemon_start(&argv(&[
        "start",
        "--foreground",
        "--port",
        "7878"
    ])));
    assert!(
        super::argv_selects_daemon_start(&argv(&["--verbose", "start"])),
        "a leading global flag must not hide the subcommand"
    );

    // A client subcommand must not inherit the daemon's environment —
    // TRUSTY_INDEX in daemon.env would otherwise repoint every CLI query.
    for cmd in ["query", "status", "serve", "doctor", "index"] {
        assert!(
            !super::argv_selects_daemon_start(&argv(&[cmd])),
            "{cmd} must not source daemon.env"
        );
    }
    assert!(!super::argv_selects_daemon_start(&argv(&[])));

    // `-i start` passes "start" as the VALUE of --index, not as the subcommand.
    assert!(!super::argv_selects_daemon_start(&argv(&[
        "-i", "start", "query", "x"
    ])));
    assert!(!super::argv_selects_daemon_start(&argv(&[
        "--index", "start", "status"
    ])));
}

/// Point every per-instance daemon path at fresh tempdirs (the isolation
/// `run_daemon_isolated_instance_leaves_shared_discovery_alone` uses) and run
/// `body`. The environment is restored on drop, so a failed assertion inside
/// `body` cannot leak `TRUSTY_DATA_DIR` into the next `#[serial]` test.
async fn with_isolated_daemon_paths<F, Fut>(body: F)
where
    F: FnOnce(std::path::PathBuf) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    struct RestoreEnv;
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            // SAFETY: every caller is `#[serial]` with the other env mutators.
            unsafe {
                std::env::remove_var("TRUSTY_DATA_DIR");
                std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
            }
        }
    }
    let override_tmp = tempfile::tempdir().unwrap();
    let data_dir_tmp = tempfile::tempdir().unwrap();
    // SAFETY: as above.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", override_tmp.path());
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }
    let _restore = RestoreEnv;
    body(data_dir_tmp.path().to_path_buf()).await;
}

/// Wait up to 5 s for the isolated daemon's socket to answer.
async fn wait_for_socket(path: &Path) -> bool {
    for _ in 0..250 {
        if trusty_common::uds::socket_is_serving(path, std::time::Duration::from_millis(50)).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

/// #9214: `run_daemon` is a real socket-only daemon.
///
/// Why: ADR-0032 retires TCP :7878. A daemon that still published an HTTP
/// address, or that skipped the background tickers along with the router,
/// would pass for socket-only while misleading every client.
/// What: runs a real isolated daemon and asserts: the
/// socket answers `search.health` with `transport.http_addr: null`; no port
/// file, no `http_addr` file and no shared registry entry exist; the lockfile
/// names this process, which is what `trusty-search stop` signals; the status
/// ticker emits, so the tickers run without a router; and a graceful stop
/// returns `Ok` and unlinks the socket.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_daemon_serves_only_the_socket() {
    use crate::core::registry::IndexRegistry;
    use crate::service::server::DaemonEvent;
    use crate::service::socket;

    with_isolated_daemon_paths(|data_dir| async move {
        let socket_path = socket::socket_path().expect("resolve the isolated socket path");
        let state = SearchAppState::new(IndexRegistry::new());
        let shutdown_tx = state.shutdown_tx.clone();
        let mut events = state.events.subscribe();
        let handle = tokio::spawn(run_daemon(state, None));

        let serving = wait_for_socket(&socket_path).await;
        let health: Option<serde_json::Value> = trusty_common::uds::send_framed_request(
            &socket_path,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": socket::METHOD_HEALTH }),
            std::time::Duration::from_secs(10),
        )
        .await
        .ok();
        let port_file = daemon_port_path().map(|p| p.exists()).unwrap_or(true);
        let addr_file = data_dir.join("http_addr").exists();
        let shared = trusty_common::read_daemon_addr("trusty-search").unwrap();
        let lock_pid = read_lockfile_pid(&daemon_lock_path().unwrap());
        let ticked = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                match events.recv().await {
                    Ok(DaemonEvent::StatusChanged { .. }) => return true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return false,
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or(false);

        let _ = shutdown_tx.send(true);
        let exit = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

        assert!(serving, "the daemon must serve its socket");
        let health = health.expect("search.health must answer over the socket");
        assert_eq!(
            health["result"]["transport"],
            serde_json::json!({
                "socket_path": socket_path.to_string_lossy(),
                "http_addr": null,
            }),
            "a socket-only daemon reports no HTTP address"
        );
        assert!(!port_file, "no port file may announce an unbound HTTP port");
        assert!(
            !addr_file,
            "no http_addr file may announce an unbound listener"
        );
        assert!(shared.is_none(), "nothing may enter the shared registry");
        assert_eq!(
            lock_pid,
            Some(std::process::id()),
            "`stop` finds the daemon by the lockfile pid, not by a port"
        );
        assert!(ticked, "the background tickers must run without a router");
        assert!(
            matches!(exit, Ok(Ok(Ok(())))),
            "a graceful stop must return Ok: {exit:?}"
        );
        assert!(!socket_path.exists(), "the socket is unlinked on shutdown");
    })
    .await;
}

/// #9214: a start withdraws the HTTP announcement an older build left.
///
/// Why: older clients read `http_addr`, then `daemon.port`. Left in place
/// after an upgrade, either would send them to a port this daemon never
/// holds — possibly one another process now owns.
/// What: seeds both files with the old daemon's address, starts the daemon,
/// and asserts both are gone once the socket serves.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_daemon_removes_a_stale_http_addr() {
    use crate::core::registry::IndexRegistry;
    use crate::service::socket;

    with_isolated_daemon_paths(|data_dir| async move {
        let socket_path = socket::socket_path().expect("resolve the isolated socket path");
        let stale_addr = data_dir.join("http_addr");
        let stale_port = daemon_port_path().expect("isolated port path");
        std::fs::write(&stale_addr, "127.0.0.1:7878\n").unwrap();
        std::fs::write(&stale_port, "7878\n").unwrap();

        let state = SearchAppState::new(IndexRegistry::new());
        let shutdown_tx = state.shutdown_tx.clone();
        let handle = tokio::spawn(run_daemon(state, None));
        let serving = wait_for_socket(&socket_path).await;
        let addr_left = stale_addr.exists();
        let port_left = stale_port.exists();
        let _ = shutdown_tx.send(true);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

        assert!(serving, "the daemon must serve its socket");
        assert!(!addr_left, "the stale http_addr file must be removed");
        assert!(!port_left, "the stale port file must be removed");
    })
    .await;
}

/// #9214: a stale `http_addr` the daemon cannot remove never stops it serving.
///
/// Why: the socket is bound before the withdrawal runs. A removal failure
/// that ended `run_daemon` with an error sent a launchd `KeepAlive` daemon
/// into a relaunch loop over one stale file.
/// What: plants a non-empty directory where `http_addr` lives, so removing
/// it fails, and runs the daemon under a WARN capture. The socket must serve,
/// a graceful stop must return `Ok`, and a warning must name the stale path.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_daemon_serves_when_a_stale_http_addr_cannot_be_removed() {
    use crate::core::registry::IndexRegistry;
    use crate::service::socket;
    use crate::service::watcher_teardown::tests::WarnCapture;
    use tracing::instrument::WithSubscriber;

    with_isolated_daemon_paths(|data_dir| async move {
        let socket_path = socket::socket_path().expect("resolve the isolated socket path");
        let stale_addr = data_dir.join("http_addr");
        std::fs::create_dir_all(&stale_addr).unwrap();
        std::fs::write(stale_addr.join("keep"), "x").unwrap();

        let capture = WarnCapture::default();
        let state = SearchAppState::new(IndexRegistry::new());
        let shutdown_tx = state.shutdown_tx.clone();
        let daemon = run_daemon(state, None).with_subscriber(capture.dispatch());
        let handle = tokio::spawn(daemon);
        let serving = wait_for_socket(&socket_path).await;
        let _ = shutdown_tx.send(true);
        let exit = tokio::time::timeout(std::time::Duration::from_secs(10), handle).await;

        let stale = stale_addr.display().to_string();
        let warnings = capture.warnings();
        assert!(
            serving,
            "a stale entry must not stop the daemon serving its socket"
        );
        assert!(
            matches!(exit, Ok(Ok(Ok(())))),
            "a graceful stop must return Ok: {exit:?}"
        );
        assert!(
            stale_addr.is_dir(),
            "the fixture must make the removal fail"
        );
        assert!(
            warnings.iter().any(|w| w.contains(&stale)),
            "a WARN must name the stale path {stale}: {warnings:?}"
        );
    })
    .await;
}

/// #9214: an unremovable shared registry entry is warned, never swallowed.
///
/// Why: the registry arm discarded its error while the file arm returned one,
/// so one stale state was fatal in one arm and silent in the other.
/// What: as the default instance, plants a non-empty directory where the
/// shared registry's `http_addr` lives, calls `deregister_shared_discovery`
/// under a WARN capture, and asserts a warning names that path.
/// Test: this function IS the test.
#[test]
#[serial]
fn deregister_shared_discovery_warns_when_the_entry_cannot_be_removed() {
    use crate::service::watcher_teardown::tests::WarnCapture;

    let tmp = tempfile::tempdir().unwrap();
    // SAFETY: `#[serial]` with every other env mutator in this module.
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
    }
    let stale = trusty_common::resolve_data_dir("trusty-search")
        .unwrap()
        .join("http_addr");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("keep"), "x").unwrap();
    let capture = WarnCapture::default();
    tracing::dispatcher::with_default(&capture.dispatch(), deregister_shared_discovery);
    // SAFETY: as above.
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }

    let stale_text = stale.display().to_string();
    let warnings = capture.warnings();
    assert!(stale.is_dir(), "the fixture must make the removal fail");
    assert!(
        warnings.iter().any(|w| w.contains(&stale_text)),
        "a WARN must name the stale registry path {stale_text}: {warnings:?}"
    );
}

/// #9214: a clean stop of a socket-only daemon returns `Ok`, every time.
///
/// Why: a normal stop cancels the drain AND ends the rpc serve loop, so both
/// arms of the stop wait can be ready together. An unbiased `select!` picked
/// the serve-loop arm about half the time and reported "the rpc socket stopped
/// serving", so a clean stop exited non-zero and
/// `run_daemon_serves_only_the_socket` flaked.
/// What: forces both inputs ready — the drain cancelled and the serve task
/// already finished — and calls the wait 50 times. Each call must return `Ok`.
/// With either arm free to win, 50 straight `Ok`s has odds of 2^-50.
/// Test: this function IS the test.
#[tokio::test]
async fn a_socket_only_stop_returns_ok_when_the_serve_loop_also_ended() {
    for attempt in 0..50 {
        let drain = tokio_util::sync::CancellationToken::new();
        let mut rpc_task = tokio::spawn(async {});
        while !rpc_task.is_finished() {
            tokio::task::yield_now().await;
        }
        drain.cancel();
        let (result, _) = super::socket_only::await_socket_only_stop(&drain, &mut rpc_task).await;
        assert!(
            result.is_ok(),
            "attempt {attempt}: a clean stop must not read as a dead socket: {result:?}"
        );
    }
}

/// #9459, #9477: a normal stop leaves every corpus closed cleanly.
///
/// Why: the daemon leaves through `process::exit(0)` once `run_daemon`
/// returns, so a corpus still open at that point never runs redb's `Drop` and
/// is left needing repair. A read-only open then fails, and
/// `project.resolve` loses the `reindexed_unix` stamp (#9477).
/// What: registers one index with a stamped corpus, starts the corpus reopen
/// sweep exactly as `handle_start` does (it holds a strong clone of the state
/// for the life of the process), runs a real socket-only daemon and stops it
/// through the admin-stop channel. Once `run_daemon` has returned, the
/// corpus must open read-only and the resolver's stamp read must return the
/// stamp. Before #9459 the sweep's clone kept the corpus open, so the
/// read-only open failed.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_normal_stop_leaves_every_corpus_openable_read_only() {
    use crate::core::corpus::CorpusStore;
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
    use crate::service::project_resolve::{Candidate, Disk, LiveDisk, RootKind};
    use crate::service::socket;
    use crate::service::storage_layout::{StorageLayout, REDB_FILE};
    use std::sync::Arc;
    const STAMP: u64 = 1_700_009_459;
    const ID: &str = "shutdown-9459";

    with_isolated_daemon_paths(|_data_dir| async move {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("lib.rs"), "fn alpha() {}\n").unwrap();
        let corpus_path = StorageLayout::DataDir
            .file(ID, root.path(), REDB_FILE)
            .expect("resolve the corpus path");
        let corpus = CorpusStore::open(&corpus_path).expect("open the corpus");
        corpus.write_reindexed_unix_sync(STAMP).expect("stamp it");
        let mut indexer = CodeIndexer::new(ID, root.path());
        indexer.set_corpus_store(Arc::new(corpus));
        let registry = IndexRegistry::new();
        registry.register(IndexHandle::bare(
            IndexId::new(ID),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            root.path().to_path_buf(),
        ));
        let state = SearchAppState::new(registry);
        crate::service::corpus_reopen::spawn_corpus_reopen_sweep(state.clone());

        let socket_path = socket::socket_path().expect("resolve the isolated socket path");
        let shutdown_tx = state.shutdown_tx.clone();
        let handle = tokio::spawn(run_daemon(state, None));
        assert!(wait_for_socket(&socket_path).await, "the socket must serve");
        let _ = shutdown_tx.send(true);
        let exit = tokio::time::timeout(std::time::Duration::from_secs(20), handle).await;
        assert!(
            matches!(exit, Ok(Ok(Ok(())))),
            "a graceful stop must return Ok: {exit:?}"
        );

        // What `process::exit(0)` leaves on disk from here on.
        let read_only = redb::ReadOnlyDatabase::open(&corpus_path);
        assert!(
            read_only.is_ok(),
            "#9459: after a normal stop the corpus must open read-only: {:?}",
            read_only.err()
        );
        drop(read_only);

        let candidate = Candidate {
            index_id: ID.to_string(),
            root_path: root.path().to_path_buf(),
            repo_identity: None,
            kind: RootKind::Indeterminate,
            resident: false,
            reindexed_unix: None,
            corpus_modified_unix: None,
            colocated: false,
            additional_roots: Vec::new(),
        };
        let disk = LiveDisk {
            names: trusty_common::workspace_layout::WorktreeDirNames::default(),
            resident: std::collections::HashMap::new(),
            runtime: Some(tokio::runtime::Handle::current()),
        };
        let stamp = tokio::task::spawn_blocking(move || disk.reindexed_unix(&candidate))
            .await
            .expect("the stamp read must not panic");
        assert_eq!(
            stamp,
            Some(STAMP),
            "#9477: project.resolve must read reindexed_unix after a normal stop"
        );
    })
    .await;
}

/// #9214: `start --socket` must not change the mode of a directory it did not
/// create.
///
/// Why: `--socket ~/x.sock` narrowed `$HOME` to `0700` — the bind hardens the
/// socket's parent, and that parent already existed.
/// What: runs a real isolated daemon with `--socket` inside an existing `0755`
/// directory, asserts it returned an error naming that directory (so the
/// refusal was reached, not an earlier failure such as the lock), and asserts
/// the directory is still `0755`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn run_daemon_with_socket_leaves_an_existing_parent_mode_unchanged() {
    use crate::core::registry::IndexRegistry;
    use std::os::unix::fs::PermissionsExt;

    with_isolated_daemon_paths(|_data_dir| async move {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("existing");
        std::fs::create_dir(&parent).expect("create the existing parent");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the existing parent to 0755");
        let socket_path = parent.join("ts.sock");

        let state = SearchAppState::new(IndexRegistry::new());
        let shutdown_tx = state.shutdown_tx.clone();
        let handle = tokio::spawn(run_daemon(state, Some(socket_path.clone())));
        // Either the daemon refuses and returns, or it binds; stop it either way.
        for _ in 0..250 {
            if handle.is_finished() || socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let _ = shutdown_tx.send(true);
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .expect("the daemon must return within 10 s of shutdown")
            .expect("the daemon task must not panic");
        let err = outcome.expect_err("start --socket must refuse a 0755 parent it did not create");
        let dir = parent.display().to_string();
        assert!(
            err.to_string().contains(&dir),
            "the refusal must name the --socket directory {dir}: {err}"
        );

        let mode = std::fs::metadata(&parent)
            .expect("stat the existing parent")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o755,
            "start --socket changed the mode of a directory it did not create (now {mode:04o})"
        );
    })
    .await;
}
