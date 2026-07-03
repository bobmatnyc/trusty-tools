//! Integration test for `tm compress --tool <name>` (issue #1956, Option 0
//! spike).
//!
//! Why: `tm compress` is meant to be invoked as the tail of a shell pipe
//! (`<original bash command> | tm compress --tool "<effective tool name>"`),
//! so its contract is genuinely process-level: read all of stdin, write the
//! compressed result to stdout, exit 0. Unit tests inside `commands::compress`
//! cover the compression logic directly; this file proves the actual binary
//! honors that stdin/stdout contract end to end.
//! What: Runs the built `tm` binary (`CARGO_BIN_EXE_tm`) as
//! `tm compress --tool <name>` with piped stdin.
//! Test: `cargo test -p trusty-mpm --test tm_compress_pipe`.
//!
//! Note on `--tool` values: `commands::hook_rewrite::effective_tool_name`
//! derives a dispatch-relevant value (e.g. `"cargo test"`, `"git diff"`) from
//! the wrapped command — NOT a hardcoded `"bash"` — because
//! `compress_tool_output`'s dispatch table matches filters by substring
//! against the tool name and has no branch for the literal string `"bash"`.
//! `tm_compress_passes_through_unmatched_tool_name_unchanged` below documents
//! that a tool name outside the dispatch table's coverage (whether that's
//! literally `"bash"` or any other unmatched name) is always a safe,
//! byte-for-byte passthrough — never a corruption or crash.

use std::io::Write;
use std::process::{Command, Stdio};

fn run_tm_compress(tool: &str, input: &str) -> (bool, String, String) {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut child = Command::new(bin)
        .args(["compress", "--tool", tool])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn `tm compress`");

    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");

    let output = child.wait_with_output().expect("wait for tm compress");
    (
        output.status.success(),
        String::from_utf8(output.stdout).expect("stdout is utf8"),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn repetitive_cargo_test_payload() -> String {
    let mut input = String::new();
    for i in 0..50 {
        input.push_str(&format!("test mod::t{i} ... ok\n"));
    }
    input.push_str("test result: ok. 50 passed; 0 failed\n");
    input
}

#[test]
fn tm_compress_shrinks_piped_cargo_test_output() {
    // "cargo test" matches compress_tool_output's cargo/test dispatch
    // branch — this is the `--tool` value `effective_tool_name` derives for
    // a wrapped `cargo test ...` command, mirroring real hook usage.
    let input = repetitive_cargo_test_payload();
    let (success, stdout, stderr) = run_tm_compress("cargo test", &input);
    assert!(success, "tm compress exited non-zero: stderr={stderr}");
    assert!(
        stdout.len() < input.len(),
        "expected compressed stdout ({} bytes) to be shorter than input ({} bytes)",
        stdout.len(),
        input.len()
    );
    assert!(
        stdout.contains("test result"),
        "compressed output must retain the summary line, got: {stdout}"
    );
}

#[test]
fn tm_compress_passes_through_unmatched_tool_name_unchanged() {
    // A tool name outside compress_tool_output's dispatch coverage (e.g. a
    // literal "bash", or any command domain without a filter branch yet,
    // such as "grep"/"ls" per the design doc's own documented gap) must be a
    // safe, byte-for-byte passthrough — never corrupted, never a crash.
    let input = repetitive_cargo_test_payload();
    let (success, stdout, stderr) = run_tm_compress("bash", &input);
    assert!(success, "tm compress exited non-zero: stderr={stderr}");
    assert_eq!(
        stdout, input,
        "expected byte-for-byte passthrough for a tool name with no dispatch match"
    );
}

#[test]
fn tm_compress_passes_through_short_input_unchanged() {
    // Below the 80-byte size gate in `compress_tool_output` — must be a
    // verbatim passthrough regardless of `--tool` value.
    let input = "ok\n";
    let (success, stdout, stderr) = run_tm_compress("cargo test", input);
    assert!(success, "tm compress exited non-zero: stderr={stderr}");
    assert_eq!(stdout, input);
}
