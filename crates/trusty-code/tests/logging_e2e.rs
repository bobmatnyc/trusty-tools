//! Subprocess coverage for the default tracing filter, stdio separation, and
//! (#6537) the file-log directory/file permissions.

use std::process::Command;

const CHILD_ENV: &str = "TCODE_LOGGING_TEST_CHILD";
const WARNING: &str = "durable-memory-test-warning";
const PROTOCOL: &str = r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#;
const FILE_LOG_CHILD_ENV: &str = "TCODE_FILE_LOG_TEST_CHILD";

#[test]
fn logging_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    trusty_code::logging::init_tracing();
    tracing::warn!("{WARNING}");
    println!("{PROTOCOL}");
}

#[test]
fn unset_rust_log_keeps_warnings_on_stderr_and_stdout_protocol_clean() {
    for rust_log in [None, Some("[invalid")] {
        let mut command = Command::new(std::env::current_exe().expect("current test binary"));
        command
            .args(["--exact", "logging_child", "--nocapture"])
            .env(CHILD_ENV, "1")
            .env_remove("RUST_LOG");
        if let Some(value) = rust_log {
            command.env("RUST_LOG", value);
        }
        let output = command.output().expect("run logging child");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(WARNING),
            "warning missing from stderr: {stderr}"
        );
        assert!(
            !stdout.contains(WARNING),
            "warning contaminated stdout: {stdout}"
        );
        assert!(stdout.lines().any(|line| line == PROTOCOL));
    }
}

/// Child process: init the file-log subscriber and emit one line, then exit —
/// the parent inspects the on-disk directory/file modes it left behind.
#[test]
fn file_log_child() {
    if std::env::var_os(FILE_LOG_CHILD_ENV).is_none() {
        return;
    }
    let _guard = trusty_code::logging::init_tracing_with_file_log();
    tracing::info!("file-log-permissions-test-line");
}

/// `init_tracing_with_file_log` leaves `~/.trusty-code/logs` at `0700` and
/// the rolled log file it just opened at `0600` (#6537 code-review fix
/// round) — a raw `create_dir_all` (the pre-fix behavior) would instead
/// apply the process umask and commonly leave `0755`/`0644`.
///
/// Why a subprocess: this crate's env-isolation rules forbid mutating
/// `HOME` in-process (`crates/trusty-mpm/src/bin/tm/**`'s own ratchet is the
/// stricter sibling rule; this crate follows the same spirit for
/// `paths::private_state`, which resolves `HOME` via `dirs::home_dir()`), and
/// `init_tracing_with_file_log` installs a GLOBAL subscriber that cannot
/// coexist with this test binary's own `begin_capture` — the same
/// constraint `logging::tests::file_log_dir_is_under_private_state`'s doc
/// comment already documents.
#[test]
#[cfg(unix)]
fn file_log_dir_and_current_file_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().expect("tempdir");
    let mut command = Command::new(std::env::current_exe().expect("current test binary"));
    command
        .args(["--exact", "file_log_child", "--nocapture"])
        .env(FILE_LOG_CHILD_ENV, "1")
        .env("HOME", home.path());
    let output = command.output().expect("run file_log_child");
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let logs_dir = home.path().join(".trusty-code").join("logs");
    let dir_mode = std::fs::metadata(&logs_dir)
        .expect("logs dir must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "logs dir mode was {dir_mode:o}");

    let mut checked_a_file = false;
    for entry in std::fs::read_dir(&logs_dir).expect("read logs dir") {
        let entry = entry.expect("dir entry");
        if !entry.metadata().expect("entry metadata").is_file() {
            continue;
        }
        let file_mode = entry
            .metadata()
            .expect("entry metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            file_mode,
            0o600,
            "log file {:?} mode was {file_mode:o}",
            entry.path()
        );
        checked_a_file = true;
    }
    assert!(checked_a_file, "expected at least one rolled log file");
}
