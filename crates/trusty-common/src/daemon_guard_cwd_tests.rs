//! #11904: a detached daemon must not keep its spawner's cwd.
//!
//! Why: `trusty-memory serve --stdio` auto-starts `serve --foreground` through
//! [`super::spawn_detached`]'s family. The daemon inherited the bridge's cwd —
//! an agent worktree — and held it after the agent finished, so `tm pr
//! cleanup` refused to reclaim the worktree.
//! What: re-invokes this test binary as a stub parent whose cwd is one temp
//! dir and whose `HOME` is another. The stub starts `/bin/sh` through the real
//! helper; the shell writes `pwd -P` to a file, and the test asserts that file
//! names the stub's `HOME`, not its cwd. No trusty daemon is started and the
//! real `HOME` is never read or written.
//! Test: `a_detached_daemon_starts_in_home_not_the_spawners_cwd`, with
//! `stub_parent_spawns_a_pwd_writer` as its child half;
//! `neutral_cwd_falls_back_to_root_without_a_usable_home` for the fallback.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{neutral_cwd_from, spawn_detached};

/// Set on the stub parent only, to the file the shell writes; the child half
/// is a no-op without it.
const STUB_ENV: &str = "TRUSTY_COMMON_DETACH_CWD_STUB";

/// Hang guard for the stub's exit and for the detached shell's write.
const BOUND: Duration = Duration::from_secs(60);

/// Wait for `path` to exist, then return its trimmed contents.
fn read_when_written(path: &Path) -> String {
    let give_up = Instant::now() + BOUND;
    while Instant::now() < give_up {
        if let Ok(text) = std::fs::read_to_string(path) {
            return text.trim().to_owned();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "the detached shell wrote nothing to {} within {BOUND:?}",
        path.display()
    );
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).expect("canonicalize a temp dir")
}

#[test]
fn a_detached_daemon_starts_in_home_not_the_spawners_cwd() {
    let spawner_cwd = tempfile::tempdir().expect("temp cwd");
    let home = tempfile::tempdir().expect("temp home");
    let out_dir = tempfile::tempdir().expect("temp out dir");
    let out = out_dir.path().join("pwd.txt");

    let exe = std::env::current_exe().expect("the test binary's own path");
    let mut stub = Command::new(exe)
        .args([
            "--exact",
            "daemon_guard::cwd_tests::stub_parent_spawns_a_pwd_writer",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(spawner_cwd.path())
        .env("HOME", home.path())
        .env(STUB_ENV, &out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the stub parent");

    let give_up = Instant::now() + BOUND;
    let status = loop {
        if let Some(status) = stub.try_wait().expect("poll the stub parent") {
            break status;
        }
        if Instant::now() >= give_up {
            let _ = stub.kill();
            let _ = stub.wait();
            panic!("the stub parent did not exit within {BOUND:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "the stub parent failed: {status:?}");

    let daemon_cwd = PathBuf::from(read_when_written(&out));
    assert_ne!(
        daemon_cwd,
        canonical(spawner_cwd.path()),
        "the detached child kept its spawner's cwd (#11904)"
    );
    assert_eq!(
        daemon_cwd,
        canonical(home.path()),
        "the detached child must start in the spawner's HOME (#11904)"
    );
}

/// Child half of `a_detached_daemon_starts_in_home_not_the_spawners_cwd`.
///
/// What: returns at once unless `STUB_ENV` is set. When set, starts a shell
/// through [`spawn_detached`] that writes its `pwd -P` to that path, via a
/// temp file and a rename so the reader never sees a partial line.
#[test]
fn stub_parent_spawns_a_pwd_writer() {
    let Some(out) = std::env::var_os(STUB_ENV) else {
        return;
    };
    let out = out.into_string().expect("a UTF-8 temp path");
    spawn_detached(
        "/bin/sh",
        &[
            "-c",
            r#"pwd -P > "$1.tmp" && mv "$1.tmp" "$1""#,
            "sh",
            out.as_str(),
        ],
    )
    .expect("spawn the detached pwd writer");
}

/// Why: a `current_dir` that does not exist fails the spawn, so an unset or
/// stale `HOME` must fall back to `/` instead of failing every daemon start.
/// What: no home, a missing home, and a real directory.
/// Test: this test.
#[test]
fn neutral_cwd_falls_back_to_root_without_a_usable_home() {
    let root = PathBuf::from("/");
    assert_eq!(neutral_cwd_from(None), root);
    assert_eq!(
        neutral_cwd_from(Some(PathBuf::from("/nonexistent/trusty-home-11904"))),
        root
    );
    let home = tempfile::tempdir().expect("temp home");
    assert_eq!(
        neutral_cwd_from(Some(home.path().to_path_buf())),
        home.path()
    );
}
