//! Crate-wide test-only helpers: stub HTTP servers, a stubbed data dir, and the
//! process-global env-var lock (#4246).
//!
//! Why: `trusty-installer` ships an EMPTY `[dev-dependencies]` — there is no
//! `wiremock`, and adding one is out of scope. The `ensure::project_setup` tests
//! had already grown the only real test vehicle in the crate: a raw
//! `tokio::net::TcpListener` answering fixed responses, plus a `TRUSTY_DATA_DIR_
//! OVERRIDE`-stubbed data dir holding a real `http_addr` file. #4246 needs that
//! exact vehicle from TWO more modules (`probe_http` and `verify_tail`), so it is
//! promoted here rather than copied a third time.
//!
//! What: [`ENV_TEST_LOCK`] (the process-global serialiser every env-mutating
//! test must hold), [`stub_once`] / [`stub_seq`] / [`stub_hang`] /
//! [`stub_seq_blocking`] (loopback servers: one-shot, sequenced, silent, and one
//! hosted on its own thread for callers that block), [`dead_addr`] (an address
//! guaranteed to refuse), and [`stub_data_dir`] / [`stub_empty_data_dir`] /
//! [`clear_data_dir_override`] (a throwaway data dir with or without a planted
//! `http_addr`).
//!
//! Test: this module IS test support; it is exercised by every test that imports
//! it (`ensure::project_setup`, `probe`, `probe_http`, `verify_tail`).

/// Process-wide lock serialising tests that mutate global env vars.
///
/// Why: `TRUSTY_DATA_DIR_OVERRIDE`, `HTTP_PROXY`, `TCTL_ENSURE_WAIT_*` are all
/// process-global, so tests in different modules would race each other — and
/// `cargo test` runs them on a thread pool by default. A single crate-shared
/// lock, held for the whole duration of each env-mutating test, is what makes
/// those assertions deterministic.
/// What: a `Mutex<()>` reachable from every module's test code. Poisoning is
/// irrelevant here (the guarded data is `()`), so callers take it with
/// `.unwrap_or_else(|e| e.into_inner())`.
/// Test: used by the env-mutating tests in `ensure::{daemon, project_setup,
/// readiness}`, `probe_http`, and `verify_tail`.
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A member binary name that is guaranteed resolvable on PATH, has NO documented
/// default port, and is not a real trusty daemon (#4246).
///
/// Why: `probe::probe_member_health` short-circuits to
/// `ProbeOutcome::NotInstalled` before it ever reaches the HTTP transport when
/// `resolve_binary_path` finds nothing — so a test that wants to exercise the
/// probe→kickstart-gate path cannot use a made-up binary name. Using a REAL
/// member name instead (`trusty-search`, …) is worse: `fixed_port_for` would
/// resolve its documented port and the probe's second leg would hit whatever
/// daemon happens to be running on the developer's machine, making the test
/// environment-dependent. `sh` satisfies all three constraints: always present
/// on the unix targets this crate's tests already assume (they spawn `echo` and
/// `sleep` unconditionally), absent from
/// [`crate::commands::probe_http::fixed_port_for`], and never a daemon — so the
/// ONLY address that resolves is the `http_addr` the test itself plants.
/// What: `"sh"`.
/// Test: used by `probe::tests::probe_member_health_*` and
/// `verify_tail::tests::verify_one_*`.
pub(crate) const PROBEABLE_BINARY: &str = "sh";

/// Spawn a one-shot TCP server that answers the first request with a fixed
/// HTTP response, and return its `host:port`.
///
/// Why: standing up a real `reqwest` round-trip against a controlled response
/// lets request-issuing logic be tested end-to-end without a live daemon.
/// What: binds an ephemeral loopback port, accepts one connection, drains the
/// request headers, writes `body` behind `status_line`, and returns the bound
/// address.
/// Test: used by every stub-server test in the crate.
pub(crate) async fn stub_once(status_line: &'static str, body: &'static str) -> String {
    stub_seq(vec![(status_line, body)]).await
}

/// Spawn a TCP server that answers a *sequence* of requests, one fixed response
/// per accepted connection in order.
///
/// Why: some flows issue two requests (`ensure`'s palace-create does an
/// existence GET then a create POST); covering those needs a stub that can
/// answer differently per connection.
///
/// # Preconditions
/// The caller must remain on a runtime that can poll the spawned accept loop —
/// i.e. it must `await` rather than block. A caller that blocks its own runtime
/// thread (the #4246 sync probe bridge) must use [`stub_seq_blocking`] instead,
/// or the accept loop never runs and the probe times out.
/// What: binds an ephemeral loopback port, `tokio::spawn`s the accept loop, and
/// returns the bound address.
/// Test: used by `ensure::project_setup::tests::create_palace_created`.
pub(crate) async fn stub_seq(responses: Vec<(&'static str, &'static str)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(serve_fixed(listener, responses));
    addr
}

/// Answer `responses.len()` connections on `listener`, one fixed response each.
///
/// Why: the shared body of [`stub_seq`] and [`stub_seq_blocking`] — the two
/// differ only in WHERE the accept loop is driven, never in what it answers, and
/// a second copy of the header-draining logic would be free to drift.
/// What: for each `(status_line, body)`, accepts one connection, drains the
/// request headers, writes the response with a correct `Content-Length`, and
/// shuts the socket down.
/// Test: exercised by every stub-server test in the crate.
async fn serve_fixed(
    listener: tokio::net::TcpListener,
    responses: Vec<(&'static str, &'static str)>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (status_line, body) in responses {
        let Ok((mut sock, _)) = listener.accept().await else {
            break;
        };
        // Drain the request up to (and including) the end-of-headers marker
        // before replying. A single fixed-size read can split a request whose
        // body is long, which used to race the write and flake CI; reading until
        // `\r\n\r\n` (or EOF) consumes the whole header block deterministically.
        // We don't need the body — only that the request is fully sent.
        let mut acc = Vec::with_capacity(2048);
        let mut chunk = [0u8; 2048];
        loop {
            match sock.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&chunk[..n]);
                    if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let resp = format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
    }
}

/// [`stub_seq`], but hosted on its own thread and callable from sync code
/// (#4246).
///
/// Why: `probe_http::probe_member_http_blocking` BLOCKS its caller — that is its
/// contract, because `tctl`'s dispatch is synchronous. A stub whose accept loop
/// lives on the *test's* runtime would therefore deadlock: the test blocks
/// waiting for a response the runtime cannot produce because the test is holding
/// it. Hosting the stub on a wholly separate thread + runtime removes the shared
/// resource, which is what lets `verify_one`'s kickstart-gate tests be plain
/// `#[test]` functions driving the REAL probe.
/// What: spawns a thread with its own current-thread runtime that binds the
/// listener, reports the address back over a channel, then serves `responses`.
/// The thread exits once every response is written.
/// Test: used by `probe::tests::probe_member_health_*` and
/// `verify_tail::tests::verify_one_*`.
pub(crate) fn stub_seq_blocking(responses: Vec<(&'static str, &'static str)>) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        rt.block_on(async move {
            let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
                return;
            };
            let Ok(addr) = listener.local_addr() else {
                return;
            };
            if tx.send(addr.to_string()).is_err() {
                return;
            }
            serve_fixed(listener, responses).await;
        });
    });
    rx.recv()
        .expect("stub server must report its bound address")
}

/// Spawn a TCP server that ACCEPTS connections and then never answers, and
/// return its `host:port` (#4246).
///
/// Why: `ProbeOutcome::Timeout` and `ProbeOutcome::Refused` must be
/// distinguishable — that distinction is the whole basis of the confirmed-down
/// kickstart gate. A refusal is easy to stage ([`dead_addr`]); a timeout needs a
/// peer that completes the TCP handshake and then goes silent, which is exactly
/// the wedged-daemon shape the bound exists for.
/// What: binds an ephemeral loopback port and holds every accepted socket open
/// (never writing) until the test process ends. Returns the bound address.
/// Test: `probe_http::tests::probe_distinguishes_failure_causes`.
pub(crate) async fn stub_hang() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            // Keep the socket alive — dropping it would send FIN and turn the
            // timeout under test into a connection-closed error instead.
            held.push(sock);
        }
    });
    addr
}

/// A loopback `host:port` guaranteed to REFUSE a connection.
///
/// Why: the confirmed-down half of the probe taxonomy needs a deterministic
/// "nothing is listening" address. Hardcoding a port risks colliding with a real
/// daemon on the developer's machine (this workspace has had three port
/// collisions); binding an ephemeral port and immediately releasing it yields an
/// address the OS has just confirmed is free. Deliberately sync (`std::net`) so
/// it is usable from both `#[test]` and `#[tokio::test]`.
/// What: binds `127.0.0.1:0`, records the address, drops the listener, and
/// returns the address.
/// Test: `probe_http::tests::probe_distinguishes_failure_causes`,
/// `probe_http::tests::probe_port_walked_daemon_is_healthy`,
/// `verify_tail::tests::verify_one_kickstarts_a_genuinely_down_launchd_daemon`.
pub(crate) fn dead_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    addr
}

/// Point `resolve_data_dir` at a throwaway directory with NO `http_addr` in it.
///
/// Why: "the daemon never started" is a distinct, load-bearing state — `ensure`
/// must report an idempotent skip rather than a failure, and the #4246 probe
/// must fall back to the documented default port rather than inventing an
/// address. Staging it means an EMPTY data dir, so it cannot reuse
/// [`stub_data_dir`].
///
/// # Preconditions
/// The caller MUST be holding [`ENV_TEST_LOCK`].
/// # Postconditions
/// Returns the created directory; teardown is [`clear_data_dir_override`].
/// Test: used by `ensure::project_setup::tests::register_index_daemon_down`,
/// `create_palace_daemon_down`.
pub(crate) fn stub_empty_data_dir(tag: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "tctl-test-empty-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe {
        // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, &tmp);
    }
    tmp
}

/// Point `resolve_data_dir` at a throwaway directory and plant `app`'s
/// `http_addr` inside it.
///
/// Why: this exercises the PRIMARY daemon-discovery path for real — a genuine
/// `http_addr` file written by `trusty_common::write_daemon_addr` and read back
/// by `trusty_common::read_daemon_addr` — rather than mocking the resolver away.
///
/// # Preconditions
/// The caller MUST be holding [`ENV_TEST_LOCK`]: `TRUSTY_DATA_DIR_OVERRIDE` is
/// process-global.
/// # Postconditions
/// Returns the created directory; the caller is responsible for
/// [`clear_data_dir_override`] and removing it.
/// Test: used by `ensure::project_setup`, `probe_http`, `verify_tail`.
pub(crate) fn stub_data_dir(app: &str, addr: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "tctl-test-{}-{}-{}",
        app,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe {
        // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, &tmp);
    }
    trusty_common::write_daemon_addr(app, addr).unwrap();
    tmp
}

/// Remove the `TRUSTY_DATA_DIR_OVERRIDE` set by [`stub_data_dir`] and delete
/// `dir`.
///
/// Why: leaving the override set would leak into whichever test acquires
/// [`ENV_TEST_LOCK`] next; centralising the teardown (and its one `unsafe`
/// justification) keeps that from being re-argued per test.
/// # Preconditions
/// The caller is still holding [`ENV_TEST_LOCK`].
/// Test: used by every test that calls [`stub_data_dir`].
pub(crate) fn clear_data_dir_override(dir: &std::path::Path) {
    unsafe {
        // SAFETY: serialised by ENV_TEST_LOCK; no concurrent env access in this crate's tests.
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    let _ = std::fs::remove_dir_all(dir);
}
