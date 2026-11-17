//! `tcode session|attach|cancel|transcript` reach the RUNNING daemon over its
//! Unix socket (#10169).
//!
//! Why: these thin clients spawned a fresh `serve --stdio` child per
//! invocation, so against a live daemon `tcode session list` printed
//! `(no sessions)` and `attach`/`cancel`/`transcript` answered
//! `session not found` for a session that daemon held. Each case here creates
//! the session on a real `tcode serve` daemon, then drives the real `tcode`
//! binary with that daemon's data directory; on the stdio route every case
//! fails.
//! What: one case per command, the unknown-id error cases that used to live
//! in `cli_e2e` (moved here because they now need a daemon to dial), and the
//! fail-open case: a socket whose daemon cannot be verified must be an error,
//! not an empty listing. Every case also checks that the CLI started no daemon
//! of its own, by the auto-spawn log's absence.
//! Test: this file is the test.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::support::{self, SocketDaemon};

/// How long one CLI invocation may run before the test kills it and fails.
const CLI_DEADLINE: Duration = Duration::from_secs(30);

/// A real `tcode serve --project <project>` daemon on its socket.
async fn daemon_for(project: &Path) -> SocketDaemon {
    support::spawn_socket_daemon_with_env(project, &[]).await
}

/// A canonical throwaway project root — the form the CLI compares a daemon's
/// reported binding against.
fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("project tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize project");
    (dir, canonical)
}

/// Run `tcode <args> --project <project>` with `data_dir` as its data root,
/// killing it at [`CLI_DEADLINE`] so a hung client fails instead of hanging.
fn tcode_against(data_dir: &Path, project: &Path, args: &[&str]) -> Output {
    let mut cmd = support::tcode_command();
    cmd.args(args)
        .arg("--project")
        .arg(project)
        .env(trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV, data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn tcode");
    let started = Instant::now();
    while child.try_wait().expect("try_wait").is_none() {
        if started.elapsed() > CLI_DEADLINE {
            let _ = child.kill();
            let output = child.wait_with_output().expect("reap tcode");
            panic!(
                "tcode {args:?} did not exit within {CLI_DEADLINE:?}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("collect tcode output")
}

/// `session.create` on the daemon, returning the new session's id.
async fn create_session(daemon: &SocketDaemon) -> String {
    let resp = daemon
        .call(
            1,
            "session.create",
            json!({"task": "#10169 thin-client probe"}),
        )
        .await;
    assert!(resp["error"].is_null(), "session.create failed: {resp}");
    resp["result"]["id"]
        .as_str()
        .expect("session.create must return an id")
        .to_string()
}

/// The CLI attached to the existing daemon rather than starting its own.
fn assert_no_daemon_spawned(data_dir: &Path) {
    let log = data_dir.join("trusty-code/tui-spawned-daemon.log");
    assert!(
        !log.exists(),
        "the CLI must reuse the running daemon, but it started one: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Assert `output` failed with the daemon's `session_not_found` refusal.
fn assert_session_not_found(output: &Output) {
    assert!(!output.status.success(), "must exit nonzero: {output:?}");
    let stderr = stderr(output);
    assert!(
        stderr.contains("session not found") || stderr.contains("-32007"),
        "expected a clear session_not_found error on stderr: {stderr}"
    );
}

/// #10169's headline acceptance: a session created on the socket-served
/// daemon is listed by `tcode session list --project P`.
#[tokio::test]
async fn session_list_shows_a_session_created_on_the_daemon() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let id = create_session(&daemon).await;

    let output = tcode_against(&daemon.data_dir(), &project, &["session", "list"]);

    let out = stdout(&output);
    assert!(output.status.success(), "must exit 0: {output:?}");
    assert!(
        out.contains(&id),
        "session list must show {id}, got: {out}\nstderr: {}",
        stderr(&output)
    );
    assert_no_daemon_spawned(&daemon.data_dir());
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// `tcode session status <id>` reads the daemon's session.
#[tokio::test]
async fn session_status_reads_a_session_created_on_the_daemon() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let id = create_session(&daemon).await;

    let output = tcode_against(&daemon.data_dir(), &project, &["session", "status", &id]);

    assert!(
        output.status.success(),
        "must exit 0: {output:?}\nstderr: {}",
        stderr(&output)
    );
    assert!(
        stdout(&output).starts_with(&id),
        "expected a status line for {id}"
    );
    assert_no_daemon_spawned(&daemon.data_dir());
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// `tcode cancel <id>` cancels the daemon's session, and the daemon agrees.
#[tokio::test]
async fn cancel_cancels_a_session_created_on_the_daemon() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let id = create_session(&daemon).await;

    let output = tcode_against(&daemon.data_dir(), &project, &["cancel", &id]);

    assert!(
        output.status.success(),
        "must exit 0: {output:?}\nstderr: {}",
        stderr(&output)
    );
    assert!(stdout(&output).contains("cancelled"), "{}", stdout(&output));
    let status = daemon
        .call(2, "session.status", json!({"session_id": id}))
        .await;
    assert_eq!(
        status["result"]["status"], "cancelled",
        "the cancel must land on the daemon's session: {status}"
    );
    assert_no_daemon_spawned(&daemon.data_dir());
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// `tcode transcript <id> --json` reads the daemon's transcript.
#[tokio::test]
async fn transcript_reads_a_session_created_on_the_daemon() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let id = create_session(&daemon).await;

    let output = tcode_against(&daemon.data_dir(), &project, &["transcript", &id, "--json"]);

    assert!(
        output.status.success(),
        "must exit 0: {output:?}\nstderr: {}",
        stderr(&output)
    );
    let parsed: Value = serde_json::from_str(&stdout(&output)).expect("transcript --json is JSON");
    assert!(
        parsed.to_string().contains(&id),
        "the transcript must be {id}'s: {parsed}"
    );
    assert_no_daemon_spawned(&daemon.data_dir());
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// `tcode attach <id>` replays the daemon's session through its terminal
/// event and exits 0. The session is cancelled first so the replay ends.
#[tokio::test]
async fn attach_replays_a_session_created_on_the_daemon() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let id = create_session(&daemon).await;
    let cancel = daemon
        .call(2, "session.cancel", json!({"session_id": id}))
        .await;
    assert!(cancel["error"].is_null(), "session.cancel failed: {cancel}");

    let output = tcode_against(&daemon.data_dir(), &project, &["attach", &id]);

    let out = stdout(&output);
    assert!(
        output.status.success(),
        "must exit 0: {output:?}\nstderr: {}",
        stderr(&output)
    );
    assert!(
        out.lines()
            .last()
            .is_some_and(|l| l.ends_with("] cancelled")),
        "attach must replay through the cancel: {out}"
    );
    assert_no_daemon_spawned(&daemon.data_dir());
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// `tcode transcript <unknown-id>` surfaces the daemon's `session_not_found`.
#[tokio::test]
async fn transcript_unknown_session_errors_cleanly() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let output = tcode_against(
        &daemon.data_dir(),
        &project,
        &["transcript", "nonexistent-id"],
    );
    assert_session_not_found(&output);
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// `tcode attach <unknown-id>` errors and exits nonzero rather than hanging.
#[tokio::test]
async fn attach_unknown_session_errors_cleanly() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let output = tcode_against(&daemon.data_dir(), &project, &["attach", "nonexistent-id"]);
    assert_session_not_found(&output);
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// `tcode cancel <unknown-id>` errors and exits nonzero.
#[tokio::test]
async fn cancel_unknown_session_errors_cleanly() {
    let (_dir, project) = project();
    let daemon = daemon_for(&project).await;
    let output = tcode_against(&daemon.data_dir(), &project, &["cancel", "nonexistent-id"]);
    assert_session_not_found(&output);
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}

/// Fail-open check: a daemon on the socket that does not report its project
/// is an error naming that socket. The client must neither fall back to a
/// stdio child nor print an empty listing — the stub would answer
/// `session.list` with no sessions if it were asked.
// Multi-threaded: the stub is served on this runtime while the test thread
// blocks on the CLI child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_list_refuses_an_unverifiable_daemon_instead_of_listing_nothing() {
    let (_dir, project) = project();
    let data = tempfile::tempdir().expect("data tempdir");
    let socket = data.path().join("trusty-code/trusty-code.sock");
    let _stub = support::uds_mock::serve_at(&socket, |method, _params| {
        let answer = match method {
            // No `binding` field: the daemon's project cannot be verified.
            "health" => json!({"status": "ok", "build": trusty_code::build_info::GIT_HASH}),
            _ => json!({"sessions": []}),
        };
        Box::pin(async move { Ok(answer) })
    });

    let output = tcode_against(data.path(), &project, &["session", "list"]);

    assert!(
        !output.status.success(),
        "an unverifiable daemon must be an error: {output:?}"
    );
    assert!(
        !stdout(&output).contains("no sessions"),
        "must not print an empty listing: {}",
        stdout(&output)
    );
    assert!(
        stderr(&output).contains("trusty-code.sock") && stderr(&output).contains("did not report"),
        "the error must name the socket: {}",
        stderr(&output)
    );
    assert_no_daemon_spawned(data.path());
}
