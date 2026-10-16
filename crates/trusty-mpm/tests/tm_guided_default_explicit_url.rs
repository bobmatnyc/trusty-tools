//! End-to-end regression test for issue #1737's one confirmed LIVE bug — the
//! bare `tm` guided default acting on a DIFFERENT daemon than the one an
//! explicit `--url` named — under the #6288 client cutover.
//!
//! Why: before #6288, `tm --url http://127.0.0.1:1` (no subcommand) probed the
//! explicit URL and exited 75 rather than auto-starting or reconnecting to the
//! real daemon. Since #6288 an explicit URL no longer selects a daemon at all:
//! every client reaches the daemon over its unix socket. A loopback URL names
//! the daemon that socket reaches, so it is ignored with a notice; a URL naming
//! another host is the case #1737 guards, and it is refused before the guided
//! flow — or anything else — runs.
//! What: spawns `tm --url http://100.64.0.1:7880` (bare, no subcommand) with
//! stdin closed, from a hermetic non-git temp directory, and asserts a
//! non-zero exit, a stderr refusal naming the URL and #6288, and an empty
//! stdout. The command comes from `common::tm_command`, so the child also gets
//! a scratch `$HOME` (#7568).
//! Test: this test.

use crate::common;

#[test]
fn bare_tm_refuses_a_remote_url_before_the_guided_flow() {
    let tmp = tempfile::tempdir().expect("create temp cwd");

    let output = common::tm_command()
        .args(["--url", "http://100.64.0.1:7880"])
        .current_dir(tmp.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("spawn tm binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "a remote --url must stop bare `tm`; stderr={stderr}"
    );
    assert!(
        stderr.contains("http://100.64.0.1:7880") && stderr.contains("#6288"),
        "stderr must name the refused URL and the rule; got: {stderr}"
    );
    assert!(
        !stderr.contains("guided default"),
        "the guided flow must not start; got: {stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "nothing may reach stdout — any output means some daemon was used; got: {stdout}"
    );
}
