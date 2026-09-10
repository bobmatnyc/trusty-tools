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
//! `tm compress --tool <name>` with piped stdin. The three
//! native-chain tests force `TRUSTY_COMPRESS_NO_RTK=1` on the child so their
//! assertions hold on any host (#7325);
//! `tm_compress_takes_the_rtk_path_when_rtk_is_installed` covers the rtk arm
//! where a host has one.
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
use trusty_agents_common::compress::ENV_COMPRESS_NO_RTK;

/// Run `tm compress` with the native fallback chain forced.
///
/// Why: every assertion below about passthrough and the 80-byte size gate
/// describes the NATIVE chain. `rtk` rewrites that output — it strips the
/// trailing newline, so a three-byte `"ok\n"` came back as `"ok"` — and
/// `resolve_binary` finds a Homebrew `rtk` whatever `PATH` says, which made
/// these tests pass or fail by host (#7325). A spawned process cannot be
/// handed a resolver, so it gets the env var the resolver honours.
/// What: [`run_tm_compress_with`] plus `TRUSTY_COMPRESS_NO_RTK=1`.
fn run_tm_compress(tool: &str, input: &str) -> (bool, String, String) {
    run_tm_compress_with(&[(ENV_COMPRESS_NO_RTK, "1")], tool, input)
}

/// Run `tm compress --tool <tool>` with `input` on stdin and `env` applied.
///
/// Setting the variables on the spawned `Command` (never on this process)
/// keeps the `crates/trusty-mpm/src/bin/tm/**` env-isolation ratchet intact
/// and leaves sibling tests in this binary untouched. The child inherits this
/// process's environment, so `TRUSTY_COMPRESS_NO_RTK` is cleared first and
/// then re-set only from `env` — otherwise an operator who exported it would
/// silently force the native chain on the rtk-arm test below (#7325).
fn run_tm_compress_with(env: &[(&str, &str)], tool: &str, input: &str) -> (bool, String, String) {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut child = Command::new(bin)
        .args(["compress", "--tool", tool])
        .env_remove(ENV_COMPRESS_NO_RTK)
        .envs(env.iter().copied())
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

/// The rtk arm of the same binary, where a host has rtk installed.
///
/// Why: the three tests above force the native chain, so without this one
/// nothing would exercise the `rtk pipe` invocation PR #7314 gave
/// `tm compress` — the seam that silences a host difference would also
/// silence the coverage (#7325).
/// What: skips when `resolve_binary` finds no rtk — a runtime check, not a
/// `cfg`, because rtk's presence is a property of the host and not of the
/// build. Otherwise spawns with no `TRUSTY_COMPRESS_NO_RTK` and requires the
/// stats line to report `rtk_binary`, which only a subprocess that ran and
/// exited zero can produce.
#[test]
fn tm_compress_takes_the_rtk_path_when_rtk_is_installed() {
    if trusty_common::bin_resolve::resolve_binary("rtk").is_none() {
        // Announce the skip: a silently-returning test reads as a pass.
        eprintln!("SKIP tm_compress_takes_the_rtk_path_when_rtk_is_installed: no rtk on this host");
        return;
    }
    let input = repetitive_cargo_test_payload();
    let (success, stdout, stderr) = run_tm_compress_with(&[], "cargo test", &input);
    assert!(success, "tm compress exited non-zero: stderr={stderr}");
    // The stats line interleaves ANSI escapes around `=`, so match the value
    // alone — `rtk_binary` appears nowhere else in the line.
    assert!(
        stderr.contains("rtk_binary"),
        "rtk is installed, so the stats line must report the rtk path: {stderr}"
    );
    assert!(
        !stderr.contains("native_fallback"),
        "the run must not have fallen back: {stderr}"
    );
    assert!(
        !stdout.is_empty(),
        "the rtk path must still return content on stdout"
    );
}
