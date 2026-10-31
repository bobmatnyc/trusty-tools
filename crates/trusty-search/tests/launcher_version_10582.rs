//! `trusty-embedderd-py --version` / `-V` must not bootstrap (#10582).
//!
//! Why: the launcher ran `ensure_venv` before looking at its arguments, so a
//! version probe took the bootstrap lock and created the venv project dir.
//! What: runs the built launcher with an isolated HOME and data dir and checks
//! it prints the version, exits 0, and leaves the data dir empty.
//! Test: this file.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run the launcher with `flag` and assert a clean version exit.
fn assert_version_flag(flag: &str) {
    let home = tempfile::tempdir().expect("tempdir");
    let data = home.path().join("data");
    std::fs::create_dir(&data).expect("create data dir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_trusty-embedderd-py"))
        .arg(flag)
        .env("HOME", home.path())
        .env("TRUSTY_DATA_DIR_OVERRIDE", &data)
        .env("TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS", "5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn launcher");
    // A pre-fix launcher starts a full venv bootstrap; bound it.
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().expect("try_wait").is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().expect("wait launcher");
    let leftovers: Vec<_> = std::fs::read_dir(&data)
        .expect("read data dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    assert!(
        leftovers.is_empty(),
        "{flag} created {leftovers:?} under the data dir"
    );
    assert!(out.status.success(), "{flag} exited {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("trusty-embedderd-py "),
        "{flag} printed {stdout:?}"
    );
}

#[test]
fn version_flag_prints_version_without_bootstrap() {
    assert_version_flag("--version");
    assert_version_flag("-V");
}
