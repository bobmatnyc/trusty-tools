//! An auto-started daemon binds the socket its client resolved (#9214).
//!
//! Why: `daemon_guard::ensure_daemon_up` spawned `start --foreground` with no
//! socket, so the daemon bound `<TRUSTY_DATA_DIR>/trusty-search.sock` while a
//! client whose `TRUSTY_SEARCH_SOCKET` named another path polled that path for
//! 60 s and failed.
//! What: runs `trusty-search list` with `TRUSTY_SEARCH_SOCKET` in a scratch
//! directory, `TRUSTY_DATA_DIR` and `HOME` isolated, and no daemon running. A
//! daemon must answer `search.health` on the client's socket, and the CLI must
//! then exit 0. The test fails at once when the daemon answers on the data-dir
//! socket instead, so a red run does not wait out the CLI's 60 s budget. HTTP is
//! off (`TRUSTY_SEARCH_NO_HTTP=1`), so the daemon binds no TCP port.
//! Test: `cargo test -p trusty-search --test integration auto_start_socket_9214::`.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::test_daemon;
use trusty_search::service::daemon_client::DaemonClient;

/// Budget for the spawned daemon to answer, as the #8900 suite allows a boot.
const BOOT: Duration = Duration::from_secs(90);

/// Budget for the CLI to finish once the daemon answers.
const CLI_EXIT: Duration = Duration::from_secs(30);

/// The pid in `<data_dir>/daemon.lock`, once the daemon has written it.
fn locked_pid(data_dir: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(data_dir.join("daemon.lock")).ok()?;
    raw.trim().parse().ok().filter(|pid| *pid > 1)
}

/// Where the daemon answered, or why it did not.
async fn wait_for_daemon(
    wanted: &DaemonClient,
    data_dir_socket: &DaemonClient,
) -> Result<(), String> {
    let deadline = Instant::now() + BOOT;
    loop {
        if wanted.is_up().await {
            return Ok(());
        }
        if data_dir_socket.is_up().await {
            return Err(format!(
                "the auto-started daemon answered on {}, not on the client's socket {}",
                data_dir_socket.socket().display(),
                wanted.socket().display()
            ));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no daemon answered on {} within {BOOT:?}",
                wanted.socket().display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Why: #9214 — auto-start must bring up a daemon the waiting client can reach.
/// What: see the module doc.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn auto_start_binds_the_socket_the_client_resolved() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    let home = tmp.path().join("home");
    let socket_dir = tmp.path().join("sock");
    for dir in [&data_dir, &home, &socket_dir] {
        std::fs::create_dir_all(dir).expect("create a scratch dir");
    }
    let socket = socket_dir.join("custom.sock");

    let mut cli = test_daemon::command()
        .arg("list")
        .current_dir(tmp.path())
        .env("TRUSTY_SEARCH_SOCKET", &socket)
        .env("TRUSTY_DATA_DIR", &data_dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", &data_dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TRUSTY_NO_AUTO_DISCOVER", "1")
        .env("TRUSTY_SEARCH_NO_HTTP", "1")
        .env("TRUSTY_EMBEDDER", "stdio")
        .env("TRUSTY_SKIP_RAM_CHECK", "1")
        .env("RUST_LOG", "warn")
        .env_remove("TRUSTY_INDEX")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn trusty-search list");

    let wanted = DaemonClient::at(&socket);
    let data_dir_socket = DaemonClient::at(data_dir.join("trusty-search.sock"));
    let verdict = wait_for_daemon(&wanted, &data_dir_socket).await;

    let mut cli_status = None;
    if verdict.is_ok() {
        let until = Instant::now() + CLI_EXIT;
        while cli_status.is_none() && Instant::now() < until {
            cli_status = cli.try_wait().expect("poll the CLI");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    // Teardown before any assertion, so a failure leaks no process.
    let _ = cli.kill();
    let _ = cli.wait();
    if let Some(pid) = locked_pid(&data_dir) {
        // SAFETY: `kill` accepts any pid; a stale one returns ESRCH.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }

    verdict.expect("#9214: auto-start must bind the client's socket");
    let status = cli_status.expect("the CLI must exit once its daemon answers");
    assert!(status.success(), "trusty-search list exited {status}");
}
