//! #1152 regression test: N concurrent bridge PROCESSES start exactly ONE daemon.
//!
//! Why: this is the failure #1152 actually was. Under load — dozens of agent
//! worktrees each launching an MCP stdio bridge — every bridge's health probe
//! missed inside the same window and every bridge spawned a daemon. ~36 orphan
//! daemons resulted, one squatting redb's exclusive write lock, and all writes
//! failed machine-wide. A probe is a window; only a lock is an exclusion. This
//! test is what keeps that distinction enforced.
//!
//! What: spawns N real child processes, each calling the production
//! [`trusty_mcp::ensure_daemon_up_single_flight`] against one shared
//! lock file and one shared (initially absent) daemon. The "daemon" is this test
//! binary re-executed in a daemon role — a real process binding a real TCP port
//! and serving `/health` — so the spawn, probe, and readiness paths under test
//! are the production ones, not stubs. Each daemon instance records its start by
//! creating a uniquely-named file, so the assertion is a simple count with no
//! timing assumptions: exactly one start, no matter how the children interleave.
//!
//! Determinism: the children are released simultaneously and the assertion is on
//! a count of on-disk artifacts after every child has exited. Nothing depends on
//! a sleep, a scheduling order, or a race being "won" by anyone in particular.
//!
//! Test: `cargo test -p trusty-mcp --test single_flight_exclusion`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use trusty_mcp::{DaemonBridgeConfig, ensure_daemon_up_single_flight};

/// Env var naming which role a re-executed copy of this binary should play.
const ROLE_VAR: &str = "TRUSTY_SF_TEST_ROLE";
/// Env var carrying the shared scratch directory every role coordinates through.
const DIR_VAR: &str = "TRUSTY_SF_TEST_DIR";

/// Number of concurrent bridges. Chosen above the ~7 bridges seen live so the
/// test exercises real contention rather than the happy path.
const BRIDGES: usize = 12;

/// Resolve the shared scratch directory from the environment.
fn scratch_dir() -> PathBuf {
    PathBuf::from(std::env::var(DIR_VAR).expect("scratch dir env var must be set"))
}

/// Where the daemon publishes the address it actually bound.
fn addr_file(dir: &Path) -> PathBuf {
    dir.join("http_addr")
}

/// Directory in which each started daemon drops one uniquely-named marker.
///
/// Why: counting files is order-independent and needs no synchronisation, so the
/// assertion cannot itself become a race.
fn starts_dir(dir: &Path) -> PathBuf {
    dir.join("starts")
}

/// Build the bridge config used by every child, pointed at the scratch dir.
///
/// Why: all children must resolve the SAME base URL and spawn the SAME daemon
/// command, exactly as the real bridges share one `daemon_start_config`.
/// What: `base_url_fn` reads the daemon's published address file each call (as
/// the production dynamic-port resolver does); `spawn_args` re-executes this
/// test binary in the daemon role.
fn bridge_config(dir: &Path) -> DaemonBridgeConfig {
    let dir_owned = dir.to_path_buf();
    DaemonBridgeConfig {
        service_name: "sf-test".to_string(),
        spawn_args: vec![
            "daemon_role_entrypoint".to_string(),
            "--exact".to_string(),
            "--nocapture".to_string(),
        ],
        health_path: "/health".to_string(),
        base_url_fn: Box::new(move || {
            std::fs::read_to_string(addr_file(&dir_owned))
                .map(|s| format!("http://{}", s.trim()))
                // Before the daemon publishes an address there is nothing to
                // probe; a port that refuses instantly keeps the probe honest.
                .unwrap_or_else(|_| "http://127.0.0.1:1".to_string())
        }),
        startup_timeout: Some(Duration::from_secs(20)),
        poll_interval: Some(Duration::from_millis(100)),
        no_spawn: false,
        no_spawn_hint: None,
    }
}

/// The daemon role: bind a port, record the start, serve `/health` forever.
///
/// Why: a real listening process is what makes the probe/readiness path under
/// test the production one. Recording the start BEFORE binding means a daemon
/// that starts is always counted, so the test can never under-report a
/// duplicate — it fails closed.
/// What: creates a unique marker in `starts/`, binds an ephemeral port, writes
/// the address atomically, then answers every connection with a 200. Exits when
/// the parent test removes the scratch directory.
#[test]
fn daemon_role_entrypoint() {
    if std::env::var(ROLE_VAR).as_deref() != Ok("daemon") {
        return; // Not the re-executed daemon: no-op during a normal test run.
    }
    let dir = scratch_dir();

    // Record this start first — an uncounted daemon would hide the very
    // duplicate this test exists to catch.
    std::fs::create_dir_all(starts_dir(&dir)).expect("create starts dir");
    let marker = starts_dir(&dir).join(format!("{}", std::process::id()));
    std::fs::write(&marker, b"started").expect("write start marker");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    // Publish atomically: a reader must never observe a half-written address.
    let tmp = dir.join("http_addr.tmp");
    std::fs::write(&tmp, addr.to_string()).expect("write tmp addr");
    std::fs::rename(&tmp, addr_file(&dir)).expect("publish addr");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        serve_health(stream);
        if !dir.exists() {
            break; // Parent cleaned up; nothing left to serve.
        }
    }
}

/// Answer one HTTP request with a 200, whatever was asked for.
fn serve_health(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    // Drain headers so the client sees a well-formed exchange.
    loop {
        let mut h = String::new();
        match reader.read_line(&mut h) {
            Ok(0) => break,
            Ok(_) if h.trim().is_empty() => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    let _ = stream.flush();
}

/// The bridge role: run the production ensure-daemon-up path exactly once.
///
/// Why: each child must exercise the real function, not a reimplementation, or
/// the test proves nothing about production behavior.
#[test]
fn bridge_role_entrypoint() {
    if std::env::var(ROLE_VAR).as_deref() != Ok("bridge") {
        return; // Not the re-executed bridge: no-op during a normal test run.
    }
    let dir = scratch_dir();

    // The daemon this bridge may spawn inherits our environment, so hand it the
    // daemon role. Without this it would inherit "bridge" and no-op, and the
    // readiness wait would time out instead of testing anything.
    // Safety: single-threaded at this point; no other thread reads the env.
    unsafe { std::env::set_var(ROLE_VAR, "daemon") };

    let config = bridge_config(&dir);
    let lock = dir.join("start.lock");

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let result = rt.block_on(ensure_daemon_up_single_flight(&config, &lock));
    // A bridge that could not reach a daemon must fail loudly — never proceed.
    result.expect("bridge must end with a reachable daemon");
}

/// Spawn one child of this test binary in the given role.
fn spawn_role(role: &str, dir: &Path, test_name: &str) -> std::process::Child {
    Command::new(std::env::current_exe().expect("current exe"))
        .arg(test_name)
        .arg("--exact")
        .arg("--nocapture")
        .env(ROLE_VAR, role)
        .env(DIR_VAR, dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn role child")
}

/// Count the daemons that recorded a start.
fn started_count(dir: &Path) -> usize {
    std::fs::read_dir(starts_dir(dir))
        .map(|rd| rd.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

/// Kill every daemon this test started, so none outlives the run.
fn reap_daemons(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(starts_dir(dir)) {
        for entry in rd.filter_map(|e| e.ok()) {
            if let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<i32>().ok())
            {
                #[cfg(unix)]
                // Safety: `pid` names a process this test spawned.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

/// Why: THE #1152 regression test. Without the exclusion, N bridges each probe a
/// missing daemon, each miss, and each spawn — the exact shape that produced ~36
/// orphan daemons and a machine-wide write-lock outage.
/// What: releases 12 real bridge processes against one shared lock and one
/// absent daemon, waits for all of them to exit successfully, and asserts
/// exactly ONE daemon recorded a start.
/// Test: itself.
#[test]
fn n_concurrent_bridges_start_exactly_one_daemon() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(starts_dir(&dir)).expect("create starts dir");

    let children: Vec<_> = (0..BRIDGES)
        .map(|_| spawn_role("bridge", &dir, "bridge_role_entrypoint"))
        .collect();

    let mut failures = Vec::new();
    for (i, mut child) in children.into_iter().enumerate() {
        let status = child.wait().expect("wait for bridge child");
        if !status.success() {
            failures.push(i);
        }
    }

    let started = started_count(&dir);
    reap_daemons(&dir);

    assert!(
        failures.is_empty(),
        "every bridge must succeed; these failed: {failures:?}"
    );
    assert_eq!(
        started, 1,
        "{BRIDGES} concurrent bridges must start exactly ONE daemon (#1152); \
         {started} daemons started"
    );
}

/// Why: the fast path must cost nothing and start nothing. A bridge that
/// restarted a healthy daemon would be its own outage.
/// What: starts one daemon, waits for it to be healthy, then runs a bridge and
/// asserts the start count is still 1.
/// Test: itself.
#[test]
fn already_running_daemon_is_not_restarted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(starts_dir(&dir)).expect("create starts dir");

    let mut daemon = spawn_role("daemon", &dir, "daemon_role_entrypoint");

    // Wait for it to publish an address and answer a probe.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && started_count(&dir) == 0 {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(started_count(&dir), 1, "fixture daemon must start");
    while Instant::now() < deadline && !addr_file(&dir).exists() {
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut bridge = spawn_role("bridge", &dir, "bridge_role_entrypoint");
    let status = bridge.wait().expect("wait for bridge");

    let started = started_count(&dir);
    let _ = daemon.kill();
    // Reap it so the fixture daemon leaves no zombie behind.
    let _ = daemon.wait();
    reap_daemons(&dir);

    assert!(
        status.success(),
        "bridge must succeed against a live daemon"
    );
    assert_eq!(
        started, 1,
        "a healthy daemon must not be restarted; {started} daemons exist"
    );
}

/// Why: the fail-closed rule. A bridge whose daemon never becomes ready must
/// return an error, never an `Ok` that surfaces downstream as an empty recall.
/// That silent-degradation shape is this repo's recurring defect.
/// What: configures a spawn command that exits immediately without ever binding,
/// and asserts `ensure_daemon_up_single_flight` returns `Err` within the budget.
/// Test: itself.
#[test]
fn daemon_that_never_becomes_ready_is_a_hard_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();

    let mut config = bridge_config(&dir);
    // A role no entrypoint answers: the child exits without binding anything.
    config.spawn_args = vec![
        "daemon_role_entrypoint".to_string(),
        "--exact".to_string(),
        "--nocapture".to_string(),
    ];
    config.startup_timeout = Some(Duration::from_secs(2));
    config.poll_interval = Some(Duration::from_millis(100));

    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let result = rt.block_on(ensure_daemon_up_single_flight(
        &config,
        &dir.join("start.lock"),
    ));

    assert!(
        result.is_err(),
        "an unready daemon must be a hard error, never a silent Ok"
    );
}

/// Why: a starter killed mid-start must not wedge every later bridge. `flock` is
/// released by the kernel on process death, which is precisely why this design
/// uses it instead of an `O_EXCL` lockfile that would survive the crash.
/// What: spawns a child that takes the lock and is then SIGKILLed, and asserts a
/// subsequent acquisition succeeds promptly rather than blocking forever.
/// Test: itself.
#[test]
fn crashed_starter_does_not_deadlock_the_next_bridge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    let lock = dir.join("start.lock");

    // Hold the lock in a child, then kill it without letting it release.
    let holder = spawn_role("lock_holder", &dir, "lock_holder_entrypoint");
    let pid = holder.id();

    // Wait until the child signals it holds the lock.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !dir.join("held").exists() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(dir.join("held").exists(), "holder must acquire the lock");

    #[cfg(unix)]
    // Safety: `pid` names the child we just spawned.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    let mut holder = holder;
    let _ = holder.wait();

    // The kernel released the flock when the process died: this must not block.
    let start = Instant::now();
    let acquired = trusty_mcp::StartLock::acquire_blocking(&lock).expect("acquire after crash");
    let elapsed = start.elapsed();
    drop(acquired);

    assert!(
        elapsed < Duration::from_secs(5),
        "a crashed starter must not deadlock the next bridge; waited {elapsed:?}"
    );
}

/// Helper role: take the lock, announce it, then block until killed.
#[test]
fn lock_holder_entrypoint() {
    if std::env::var(ROLE_VAR).as_deref() != Ok("lock_holder") {
        return;
    }
    let dir = scratch_dir();
    let _lock = trusty_mcp::StartLock::acquire_blocking(&dir.join("start.lock"))
        .expect("holder acquires lock");
    std::fs::write(dir.join("held"), b"1").expect("signal held");
    // Block forever; the parent SIGKILLs us while the lock is still held.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
