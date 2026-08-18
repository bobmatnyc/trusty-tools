//! Subprocess coverage for the default tracing filter and stdio separation.

use std::process::Command;

const CHILD_ENV: &str = "TCODE_LOGGING_TEST_CHILD";
const WARNING: &str = "durable-memory-test-warning";
const PROTOCOL: &str = r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#;

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
