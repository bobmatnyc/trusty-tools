//! A launchd copy that loses the lock to a healthy daemon stands by (#10358).
//!
//! Why: at boot a client's auto-start and launchd start the same job. The loser
//! exited 1 and `KeepAlive` respawned it every 30 s (751 runs observed). It must
//! stay alive while the holder serves, take over when the holder goes, exit
//! cleanly on SIGTERM, and keep today's refusal when it is not supervised or the
//! holder does not answer.
//! What: real `start --foreground` processes on a scratch data dir. launchd
//! cannot be faked for a child, so `TRUSTY_SEARCH_TEST_ASSUME_LAUNCHD_SUPERVISED=1`
//! stands in for `launchd_supervision()`, and `TRUSTY_SEARCH_STANDBY_RETRY_MS`
//! shortens the 5 s retry gap.
//! Test: `cargo test -p trusty-search --test launchd_standby_10358`.

#[path = "support/test_daemon.rs"]
mod test_daemon;

use std::fs::File;
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use fs4::FileExt;
use trusty_search::service::daemon_client::DaemonClient;

const BOOT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(100);

/// Spawn `start --foreground` on `data_dir`, stderr to `<data_dir>/<log>`.
fn spawn_start(data_dir: &Path, log: &str, supervised: bool) -> Child {
    let home = data_dir.join("home");
    std::fs::create_dir_all(&home).expect("create fake HOME");
    let log = File::create(data_dir.join(log)).expect("create log");
    let mut cmd = test_daemon::command();
    cmd.args(["start", "--foreground", "--no-auto-discover"])
        .env("TRUSTY_DATA_DIR", data_dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data_dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TRUSTY_EMBEDDER", "stdio")
        .env("TRUSTY_SKIP_RAM_CHECK", "1")
        .env("TRUSTY_SEARCH_STANDBY_RETRY_MS", "200")
        .env("RUST_LOG", "info")
        .env_remove("TRUSTY_SEARCH_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    if supervised {
        cmd.env("TRUSTY_SEARCH_TEST_ASSUME_LAUNCHD_SUPERVISED", "1");
    }
    cmd.spawn().expect("spawn trusty-search start --foreground")
}

fn client(data_dir: &Path) -> DaemonClient {
    DaemonClient::at(data_dir.join("trusty-search.sock"))
}

fn locked_pid(data_dir: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(data_dir.join("daemon.lock")).ok()?;
    raw.trim().parse().ok().filter(|pid| *pid > 1)
}

fn term(pid: u32) {
    // SAFETY: `kill` accepts any pid; a stale one returns ESRCH.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
}

async fn wait_up(data_dir: &Path) {
    let deadline = Instant::now() + BOOT;
    while !client(data_dir).is_up().await {
        assert!(
            Instant::now() < deadline,
            "no daemon answered within {BOOT:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Wait for `child` to exit, killing it at the bound.
fn wait_exit(child: &mut Child, bound: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(st) = child.try_wait().expect("poll child") {
            return Some(st);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(POLL);
    }
}

fn log_of(data_dir: &Path, name: &str) -> String {
    std::fs::read_to_string(data_dir.join(name)).unwrap_or_default()
}

/// Why: the core of #10358 - stand by, take over, stop cleanly.
/// What: a standby stays alive across many retry ticks behind a healthy holder,
/// serves after the holder is SIGTERMed, then exits 0 on its own SIGTERM.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn standby_outlives_ticks_takes_over_and_stops_on_sigterm() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let mut holder = spawn_start(dir, "holder.log", false);
    wait_up(dir).await;
    let mut standby = spawn_start(dir, "standby.log", true);

    tokio::time::sleep(Duration::from_millis(2500)).await;
    let early = standby.try_wait().expect("poll standby");
    let standby_log = log_of(dir, "standby.log");

    term(holder.id());
    let holder_status = wait_exit(&mut holder, Duration::from_secs(30));

    let deadline = Instant::now() + BOOT;
    let mut took_over = false;
    while Instant::now() < deadline && standby.try_wait().expect("poll").is_none() {
        if locked_pid(dir) == Some(standby.id()) && client(dir).is_up().await {
            took_over = true;
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    term(standby.id());
    let end = wait_exit(&mut standby, Duration::from_secs(30));

    assert!(early.is_none(), "standby exited {early:?}: {standby_log}");
    assert!(
        standby_log.contains("standing by"),
        "no standby log: {standby_log}"
    );
    assert!(holder_status.is_some(), "holder ignored SIGTERM");
    assert!(
        took_over,
        "standby never took over: {}",
        log_of(dir, "standby.log")
    );
    assert_eq!(
        end.map(|s| s.success()),
        Some(true),
        "SIGTERM exit: {end:?}"
    );
}

/// Why: SIGTERM while waiting must not hang launchd's stop.
/// What: SIGTERM a standing-by copy; it exits 0 and the holder keeps serving.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_while_standing_by_exits_zero_and_leaves_the_holder() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let mut holder = spawn_start(dir, "holder.log", false);
    wait_up(dir).await;
    let mut standby = spawn_start(dir, "standby.log", true);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    term(standby.id());
    let end = wait_exit(&mut standby, Duration::from_secs(15));
    let holder_up = client(dir).is_up().await;
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(end.map(|s| s.success()), Some(true), "exit: {end:?}");
    assert!(holder_up, "the holder must keep serving");
}

/// Why: a manual `start --foreground` has no respawn loop to protect and wants
/// its error.
/// What: no supervision + healthy holder exits 1 at once.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn unsupervised_start_still_refuses_a_healthy_holder() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let mut holder = spawn_start(dir, "holder.log", false);
    wait_up(dir).await;
    let mut second = spawn_start(dir, "second.log", false);
    let end = wait_exit(&mut second, Duration::from_secs(15));
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(end.and_then(|s| s.code()), Some(1), "exit: {end:?}");
}

/// Why: a holder that does not answer may be wedged; standing by behind it
/// would hide that.
/// What: this test holds the lock with its own pid and serves nothing; a
/// supervised start refuses with exit 1.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn supervised_start_refuses_a_holder_that_does_not_answer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let lock = File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .read(true)
        .open(dir.join("daemon.lock"))
        .expect("open lock");
    lock.try_lock_exclusive().expect("take the lock");
    std::fs::write(dir.join("daemon.lock"), std::process::id().to_string()).expect("pid");
    let mut second = spawn_start(dir, "second.log", true);
    let end = wait_exit(&mut second, Duration::from_secs(15));
    drop(lock);
    assert_eq!(end.and_then(|s| s.code()), Some(1), "exit: {end:?}");
}

/// Why: a refused manual `start --foreground` used to reach `save_daemon_env`
/// and replace the live holder's `daemon.env` with the caller's memory-limit
/// variables, which the next launchd restart inherited.
/// What: with a healthy holder up and a `daemon.env` on disk, an unsupervised
/// start carrying `TRUSTY_MEMORY_LIMIT_MB` is refused and the file is
/// byte-identical afterwards.
/// Test: itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_start_leaves_the_holders_daemon_env_untouched() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let mut holder = spawn_start(dir, "holder.log", false);
    wait_up(dir).await;
    let env_file = dir.join("daemon.env");
    let original = "TRUSTY_MEMORY_LIMIT_MB=2048\n";
    std::fs::write(&env_file, original).expect("write daemon.env");

    let home = dir.join("home");
    let log = File::create(dir.join("second.log")).expect("log");
    let mut second = test_daemon::command()
        .args(["start", "--foreground", "--no-auto-discover"])
        .env("TRUSTY_DATA_DIR", dir)
        .env("TRUSTY_DATA_DIR_OVERRIDE", dir)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env("TRUSTY_EMBEDDER", "stdio")
        .env("TRUSTY_SKIP_RAM_CHECK", "1")
        .env("TRUSTY_MEMORY_LIMIT_MB", "999")
        .env_remove("TRUSTY_SEARCH_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .expect("spawn the second start");
    let end = wait_exit(&mut second, Duration::from_secs(15));
    let after = std::fs::read_to_string(&env_file).expect("read daemon.env");
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(end.and_then(|s| s.code()), Some(1), "exit: {end:?}");
    assert_eq!(after, original, "a refused start rewrote daemon.env");
}
