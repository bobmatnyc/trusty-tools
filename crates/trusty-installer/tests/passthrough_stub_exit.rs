//! Regression test for #10585 — an unimplemented passthrough must not exit 0.
//!
//! Why: `trusty-installer health <members...>` parses as the generic
//! passthrough, whose Phase-0 stub printed "not yet implemented" and exited 0,
//! so fresh-install validation read the stub as a passing health check.
//!
//! What: runs the real binary with the issue's exact argv, in human and JSON
//! mode, and asserts a non-zero exit plus the unimplemented message.
//!
//! Test: `health_passthrough_stub_exits_non_zero`.

use std::process::Command;

#[test]
fn health_passthrough_stub_exits_non_zero() {
    for json in [false, true] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-installer"));
        cmd.args(["--timeout", "5"]);
        if json {
            cmd.arg("--json");
        }
        cmd.args(["health", "trusty-memory", "trusty-search", "trusty-mpm"]);
        let out = cmd.output().expect("spawn trusty-installer");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_ne!(
            out.status.code(),
            Some(0),
            "json={json}: unimplemented passthrough must not report success; stdout: {stdout}"
        );
        assert!(
            stdout.contains("not yet implemented") || stdout.contains("not yet fully implemented"),
            "json={json}: output must state the command is unimplemented; stdout: {stdout}"
        );
    }
}
