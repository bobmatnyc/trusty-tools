//! Coverage for the shared on-demand analyze entry point (#6350).
//!
//! What is provable here: the spawn spec, the timing budget, the idle-window
//! parser, and the external-mode opt-out — all without a built binary. The real
//! spawn, the idle exit, and the two-concurrent-callers race need a real
//! `trusty-analyze` and are proven in that crate's `on_demand_tests.rs`.

use super::*;

/// Why: the child must bind the path the parent probes. A spec that dropped
/// `--socket` would have the child derive the real data directory instead, and
/// every test using a tempdir would spawn a server nobody dials — a spawn that
/// times out for a reason no error message names.
/// Test: this test itself.
#[test]
fn analyze_spawn_spec_runs_a_bare_serve() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("sockets").join("trusty-analyze.sock");

    let Ok(spec) = analyze_spawn_spec(&socket) else {
        eprintln!("skip: trusty-analyze is not installed on this machine");
        return;
    };

    let args: Vec<String> = spec
        .args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(args[0], "serve", "the child must be a plain serve");
    assert_eq!(args[1], "--socket");
    assert_eq!(args[2], socket.to_string_lossy());
    assert!(
        spec.create_dirs
            .contains(&socket.parent().expect("parent").to_path_buf()),
        "the socket's directory must be created before the child tries to bind it"
    );
    assert!(
        !args.iter().any(|a| a == "--port" || a == "--mcp"),
        "no retired transport flag may be passed: {args:?}"
    );
}

/// Why: `ServiceTimeouts::new` is a `const fn` whose assert fires at build time,
/// so an inverted pair would already have failed the compile. What this test
/// adds is the direction of the margin — a patience that merely matched the
/// flush would SIGKILL inside it.
/// Test: this test itself.
#[test]
fn analyze_timeouts_leave_room_for_the_flush() {
    assert!(
        ANALYZE_TIMEOUTS.sigterm_patience > ANALYZE_TIMEOUTS.shutdown_flush,
        "the SIGKILL must land after the flush window, never inside it"
    );
    assert_eq!(ANALYZE_TIMEOUTS.shutdown_flush, ANALYZE_SHUTDOWN_FLUSH);
    assert!(
        ANALYZE_TIMEOUTS.spawn_probe >= Duration::from_secs(5),
        "the budget has to cover opening two redb files on a cold page cache"
    );
}

/// Why: the default is the whole operator-facing promise of #6350 — a server
/// that reclaims itself — and the two special values are the escape hatches.
/// Test: this test itself.
#[test]
fn idle_timeout_parses_its_three_meanings() {
    assert_eq!(
        analyze_idle_timeout(None),
        Some(DEFAULT_ANALYZE_IDLE_TIMEOUT)
    );
    assert_eq!(
        analyze_idle_timeout(Some("")),
        Some(DEFAULT_ANALYZE_IDLE_TIMEOUT)
    );
    assert_eq!(
        analyze_idle_timeout(Some("0")),
        None,
        "0 must mean never exit, not exit immediately"
    );
    assert_eq!(
        analyze_idle_timeout(Some(" 90 ")),
        Some(Duration::from_secs(90))
    );
    assert_eq!(
        analyze_idle_timeout(Some("soon")),
        Some(DEFAULT_ANALYZE_IDLE_TIMEOUT),
        "a typo must not stop the server from starting"
    );
    assert!(
        DEFAULT_ANALYZE_IDLE_TIMEOUT >= Duration::from_secs(5 * 60)
            && DEFAULT_ANALYZE_IDLE_TIMEOUT <= Duration::from_secs(15 * 60),
        "the #6350 ruling fixed the default inside a 5–15 minute band"
    );
}

/// Why: an operator running `trusty-analyze serve` by hand owns that process.
/// With the opt-out set, `ensure_running` must hand back the path and start
/// nothing, even with no socket there at all — otherwise a debugging session
/// gets a second server spawned underneath it.
/// Test: this test itself.
#[serial_test::serial]
#[tokio::test]
async fn external_mode_returns_the_socket_without_spawning() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("absent.sock");
    let handle = OnDemandAnalyze::at(&socket);

    // SAFETY: `#[serial]` keeps every other test in this binary off the
    // environment for the duration, which is the precondition `set_var`/
    // `remove_var` carry in edition 2024.
    unsafe { std::env::set_var(ANALYZE_EXTERNAL_ENV, "1") };
    let path = handle.ensure_running().await;
    unsafe { std::env::remove_var(ANALYZE_EXTERNAL_ENV) };

    assert_eq!(
        path.expect("external mode must succeed without a server"),
        socket
    );
    assert!(
        !socket.exists(),
        "external mode must not have started anything"
    );
}

/// Why: the socket a client dials and the socket the server binds are the same
/// derived path, and this handle is where a client resolves it. A divergence
/// here is the class of bug the retired `http_addr` discovery file used to
/// produce.
/// Test: this test itself.
#[test]
fn the_default_handle_uses_the_shared_socket_path() {
    let Ok(expected) = crate::daemon_socket_path(ANALYZE_SERVICE) else {
        eprintln!("skip: no resolvable data directory in this environment");
        return;
    };
    let handle = OnDemandAnalyze::new().expect("the path resolved a moment ago");
    assert_eq!(handle.socket(), expected);
}
