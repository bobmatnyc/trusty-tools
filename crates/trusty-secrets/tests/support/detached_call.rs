//! One socket call made from a process outside this test's tree (#10386).
//!
//! Why: `secrets.set` and `secrets.delete` refuse a caller with a Claude Code
//! ancestor (#10386 Q2), and the public `serve` judges callers on the host's
//! real process table. A test run from an agent session has Claude Code
//! above it, so a `set` sent by the test process itself is refused there and
//! allowed in CI. The call is made from a re-run of this test binary that
//! has re-parented away from the test, so every host judges the same caller.
//! What: [`spawn`] writes the request to `<dir>/request.json` and starts
//! `/bin/sh`, which backgrounds this binary's [`ROLE_TEST`] and exits, so
//! the role re-parents (to launchd or init). The role waits until its parent
//! is no longer that shell, sends the request, and writes the reply to
//! `<dir>/response.json` through a rename. [`response`] polls for it. Each
//! binary that uses this module declares a `#[test] fn detached_call_role`
//! calling [`run_role_if_asked`]; outside a role it does nothing.
//! Test: `post_success_append_failure_is_audit_unavailable_and_the_change_stands`,
//! `sigterm_during_a_cli_call_kills_its_process_group`.

#![allow(dead_code, reason = "each test binary uses a subset")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use trusty_common::uds::send_framed_request;
use trusty_common::uds::server::RpcResponse;

/// The role test every user of this module declares.
pub const ROLE_TEST: &str = "detached_call_role";
/// The env var naming the role's working directory; unset outside a role.
const DIR_ENV: &str = "TS_DETACHED_CALL_DIR";
/// The env var naming the socket the role calls.
const SOCKET_ENV: &str = "TS_DETACHED_CALL_SOCKET";
/// The env var carrying the backgrounding shell's pid.
const SHELL_ENV: &str = "TS_DETACHED_CALL_SHELL";
/// How long the role may take to re-parent, and to get a reply.
const BOUND: Duration = Duration::from_secs(30);

/// Start the detached caller of `request` against `socket`; it reports in `dir`.
pub fn spawn(dir: &Path, socket: &Path, request: &Value) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("request.json"), request.to_string()).unwrap();
    let exe = std::env::current_exe().unwrap();
    // `$$` is the shell; the backgrounded role outlives it and re-parents.
    let script = format!(
        "{SHELL_ENV}=$$ \"$0\" {ROLE_TEST} --exact --nocapture --test-threads=1 \
         </dev/null >/dev/null 2>&1 &"
    );
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg(exe)
        .env(DIR_ENV, dir)
        .env(SOCKET_ENV, socket)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the backgrounding shell failed");
}

/// The detached caller's reply, waiting up to [`BOUND`].
pub async fn response(dir: &Path) -> RpcResponse {
    let path = dir.join("response.json");
    let started = Instant::now();
    while !path.exists() {
        assert!(
            started.elapsed() < BOUND,
            "the detached caller never replied"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

/// In a role re-run, make the call and report; otherwise do nothing.
pub fn run_role_if_asked() {
    let Some(dir) = std::env::var_os(DIR_ENV).map(PathBuf::from) else {
        return;
    };
    let socket = PathBuf::from(std::env::var_os(SOCKET_ENV).unwrap());
    let shell: libc::pid_t = std::env::var(SHELL_ENV).unwrap().parse().unwrap();
    let started = Instant::now();
    // SAFETY: `getppid` takes no arguments and cannot fail.
    while unsafe { libc::getppid() } == shell {
        assert!(started.elapsed() < BOUND, "the role never re-parented");
        std::thread::sleep(Duration::from_millis(20));
    }
    let request: Value =
        serde_json::from_slice(&std::fs::read(dir.join("request.json")).unwrap()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let sent: Result<RpcResponse, _> =
        runtime.block_on(send_framed_request(&socket, &request, BOUND));
    // A server the test stops mid-call leaves no reply to report.
    let Ok(reply) = sent else {
        return;
    };
    let partial = dir.join("response.json.partial");
    std::fs::write(&partial, serde_json::to_vec(&reply).unwrap()).unwrap();
    std::fs::rename(partial, dir.join("response.json")).unwrap();
}
