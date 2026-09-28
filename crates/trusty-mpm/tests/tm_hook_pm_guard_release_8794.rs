//! `tm hook --pm-guard` holds its exit, capped, for the builder-slot release a
//! timed-out claim spawned (#8794).
//!
//! Why: the release rides a spawned task, and the hook process exits right
//! after printing its deny, which drops the tokio runtime and cancels any task
//! still in flight. Only the real binary shows whether the release outlives
//! that exit, so these tests run it against a stub daemon.
//! What: the stub leaves the builder-slot claim unanswered, so the hook's 2 s
//! budget runs out and it denies and spawns a release; it answers the deny's
//! audit POST at once and the release per case. Each case compares the stub's
//! clock readings with the moment the child exited.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_release_8794::`.

use crate::common;

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The hook's cap on the release wait — `RELEASE_WAIT_CAP` in
/// `commands::pm_guard_builder_cap`.
const RELEASE_WAIT_CAP: Duration = Duration::from_millis(300);

/// An isolated builder dispatch: it skips the shared-tree claim and goes
/// straight to the builder-slot claim.
const BUILDER_DISPATCH: &str = r#"{"hook_event_name":"PreToolUse","session_id":"11111111-1111-1111-1111-111111111111","tool_use_id":"toolu_8794","tool_name":"Agent","tool_input":{"subagent_type":"rust-engineer","isolation":"worktree","prompt":"go"}}"#;

/// How the stub answers the release route.
#[derive(Clone, Copy)]
enum Release {
    /// Answer after this delay.
    AnswerAfter(Duration),
    /// Hold the request and never answer.
    Never,
}

/// When the stub saw the release, and when it began answering it.
#[derive(Default)]
struct Seen {
    release_arrived: Option<Instant>,
    release_answering: Option<Instant>,
}

/// Read one HTTP request; return its request line once the body is complete.
fn read_request(socket: &mut TcpStream) -> Option<String> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match socket.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => raw.extend_from_slice(&buf[..n]),
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let len: usize = head
            .lines()
            .find_map(|l| {
                let (key, value) = l.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .unwrap_or(0);
        if body.len() >= len {
            return head.lines().next().map(str::to_string);
        }
    }
}

fn answer(socket: &mut TcpStream, body: &str) {
    let _ = socket.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    );
}

/// A daemon stand-in: silent on the claim, `release` on the release, and an
/// immediate `{}` on every other route (the deny's audit POST among them).
fn spawn_daemon(release: Release) -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let seen = Arc::new(Mutex::new(Seen::default()));
    let shared = Arc::clone(&seen);
    std::thread::spawn(move || {
        for mut socket in listener.incoming().flatten() {
            let seen = Arc::clone(&shared);
            std::thread::spawn(move || {
                let Some(line) = read_request(&mut socket) else {
                    return;
                };
                if line.contains("/delegations/builder-slot/release") {
                    seen.lock().expect("seen").release_arrived = Some(Instant::now());
                    match release {
                        Release::AnswerAfter(delay) => {
                            std::thread::sleep(delay);
                            seen.lock().expect("seen").release_answering = Some(Instant::now());
                            answer(&mut socket, r#"{"released":true}"#);
                        }
                        Release::Never => std::thread::sleep(Duration::from_secs(30)),
                    }
                } else if line.contains("/delegations/builder-slot") {
                    // Outlives the hook's 2 s claim budget.
                    std::thread::sleep(Duration::from_secs(30));
                } else {
                    answer(&mut socket, "{}");
                }
            });
        }
    });
    (url, seen)
}

/// Run the hook over [`BUILDER_DISPATCH`]; return its stdout, when it was
/// spawned, and when it was seen to exit.
fn run_hook(url: &str) -> (String, Instant, Instant) {
    let home = tempfile::tempdir().expect("home");
    let cwd = tempfile::tempdir().expect("cwd");
    let started = Instant::now();
    let mut child = common::tm_command_in(home.path())
        .args(["--url", url, "hook", "--pm-guard"])
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .current_dir(cwd.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tm hook --pm-guard");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(BUILDER_DISPATCH.as_bytes())
        .expect("write payload");
    let status = child.wait().expect("wait for the hook");
    let exited = Instant::now();
    assert!(status.success(), "the hook always exits 0: {status}");
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#) && stdout.contains("cap unverifiable"),
        "an unanswered claim denies as unverifiable: {stdout}"
    );
    (stdout, started, exited)
}

/// 🔴 REGRESSION (#8794): the hook does not exit before a release the daemon
/// answers inside the cap. The stub starts answering 100 ms after the release
/// arrives, while the audit POST it answers at once is long done.
/// Fails before #8794: the hook exits once the audit returns, the runtime
/// drop cancels the release, and the stub has not begun to answer.
#[test]
fn the_hook_exits_only_after_a_release_answered_within_the_cap_8794() {
    let (url, seen) = spawn_daemon(Release::AnswerAfter(Duration::from_millis(100)));
    let (_, started, exited) = run_hook(&url);
    let seen = seen.lock().expect("seen");
    let arrived = seen
        .release_arrived
        .expect("the hook must send the release for the claim it gave up on");
    let answering = seen
        .release_answering
        .expect("the hook exited before the daemon began answering its release");
    eprintln!(
        "#8794 release answered in 100 ms: total {:?}, release arrival to exit {:?}",
        exited - started,
        exited - arrived
    );
    assert!(
        answering < exited,
        "the release was answered {:?} after the hook exited",
        answering - exited
    );
}

/// #8794: a release that never answers holds the exit for the cap, not for the
/// release's own 2 s timeout.
#[test]
fn a_release_that_never_answers_holds_the_exit_no_longer_than_the_cap_8794() {
    let (url, seen) = spawn_daemon(Release::Never);
    let (_, started, exited) = run_hook(&url);
    let arrived = seen
        .lock()
        .expect("seen")
        .release_arrived
        .expect("the hook must send the release for the claim it gave up on");
    let held = exited - arrived;
    eprintln!(
        "#8794 release never answered: total {:?}, release arrival to exit {held:?}",
        exited - started
    );
    let bound = RELEASE_WAIT_CAP + Duration::from_millis(600);
    assert!(
        held < bound,
        "the hook held its exit {held:?} for an unanswered release; the cap is {RELEASE_WAIT_CAP:?}"
    );
}
