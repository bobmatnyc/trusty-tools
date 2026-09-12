//! Integration test for `tm compress --tool <name>` (issue #1956, Option 0
//! spike).
//!
//! Why: `tm compress` is meant to be invoked as the tail of a shell pipe
//! (`<original bash command> | tm compress --tool "<effective tool name>"`),
//! so its contract is genuinely process-level: read all of stdin, write the
//! compressed result to stdout, exit 0. Unit tests inside `commands::compress`
//! cover the compression logic directly; this file proves the actual binary
//! honors that stdin/stdout contract end to end.
//! What: Runs the built `tm` binary (via `common::tm_command`) as
//! `tm compress --tool <name>` with piped stdin. The three
//! native-chain tests force `TRUSTY_COMPRESS_NO_RTK=1` on the child so their
//! assertions hold on any host (#7325);
//! `tm_compress_takes_the_rtk_path_when_rtk_is_installed` covers the rtk arm
//! where a host has one. Every spawn also pins the child's `RUST_LOG` — see
//! [`STATS_LOG_LEVEL`].
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

mod common;

use std::io::Write;
use std::process::{Command, Stdio};
use trusty_agents_common::compress::ENV_COMPRESS_NO_RTK;

/// The log filter every spawned `tm compress` is pinned to (#7401, #7351).
///
/// Why: the stats line four tests below assert on is a `tracing::info!`, and
/// `commands::compress::init_stats_log_subscriber` falls back to `info` only
/// when `RUST_LOG` is ABSENT. CI exports `RUST_LOG: "warn"` workflow-wide
/// (`.github/workflows/ci.yml`), the spawned child inherits it, the subscriber
/// filters the line out, and each assertion fails against an empty stderr —
/// red on every push-to-main nextest run since #7401 landed (tracker #7351).
/// Whether the stats line should be emitted regardless of the ambient filter
/// is a production question, and #7607 wants that line SUPPRESSED rather than
/// forced, so the pin belongs here.
/// What: applied to the spawned `Command`'s environment only — never to this
/// process, which would trip the `src/bin/tm/**` env-isolation ratchet and
/// leak into sibling tests — exactly as `TRUSTY_COMPRESS_NO_RTK` is. The
/// caller's own `RUST_LOG` (unset, `warn`, `debug`) then cannot change what
/// these tests observe.
/// Test: `tm_compress_reports_unknown_for_an_unwrapped_invocation` is the
/// failure CI reported; `tm_compress_reports_the_wrapped_commands_exit_status`,
/// `tm_compress_reports_a_signal_killed_wrapped_command` and
/// `tm_compress_takes_the_rtk_path_when_rtk_is_installed` fail the same way.
const STATS_LOG_LEVEL: (&str, &str) = ("RUST_LOG", "info");

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
/// `common::tm_command` clears `CLAUDE_CODE_SESSION_ID` (#7514) and points the
/// child at a scratch `$HOME` (#7568) — the ledger it writes is the scratch
/// home's, not the developer's. [`STATS_LOG_LEVEL`] is applied before `env`,
/// so a caller that needs a different filter can still name one (#7401).
fn run_tm_compress_with(env: &[(&str, &str)], tool: &str, input: &str) -> (bool, String, String) {
    let mut child = common::tm_command()
        .args(["compress", "--tool", tool])
        .env_remove(ENV_COMPRESS_NO_RTK)
        // See #7401: the ambient RUST_LOG must not decide whether the stats
        // line the assertions read is emitted.
        .env(STATS_LOG_LEVEL.0, STATS_LOG_LEVEL.1)
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

/// Drop ANSI CSI sequences so a whole `field=value` pair can be asserted.
///
/// Why: the stats line interleaves colour escapes around `=`, so a raw
/// `contains("exit=3")` never matches even when the field is right there
/// (#7384). The sibling rtk test works around this by matching the value
/// alone, which cannot distinguish `exit=3` from a `3` anywhere else.
/// What: removes `ESC [ … <final byte>` runs; every escape `tracing`'s fmt
/// layer emits is of that form.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for c in chars.by_ref() {
            if ('@'..='~').contains(&c) {
                break;
            }
        }
    }
    out
}

/// Run `inner` through the pipeline shape `tm hook` rewrites a Bash call into.
///
/// Why: the exit status the stats line reports is the WRAPPED command's, which
/// only a real shell can supply — asserting it against a hand-fed stdin would
/// prove nothing about the mechanism (#7384).
/// What: builds `{ sh -c '<inner>'; printf '\n<sentinel>\n' "$?"; } | tm
/// compress --tool '<tool>'`, the exact string
/// `commands::compress::wrap_command_reporting_exit` produces, and returns the
/// filter's stdout plus its ANSI-stripped stderr. `inner` must not contain a
/// single quote.
fn run_wrapped_pipeline(inner: &str, tool: &str) -> (String, String) {
    // #7568: a shell pipeline needs the PATH, not a `Command`, so the isolation
    // goes on the `sh` that runs it — the `tm compress` at the tail inherits it.
    // That also covers #7514: the scrubbed environment carries no live
    // CLAUDE_CODE_SESSION_ID to key a savings row by.
    let bin = common::tm_bin();
    let script = format!(
        "{{ sh -c '{inner}'; printf '\\n__tm_compress_exit=%s__\\n' \"$?\"; }} \
         | '{bin}' compress --tool '{tool}'"
    );
    let mut command = Command::new("sh");
    common::isolate_spawned_tm(&mut command, common::tm_spawn_home());
    let output = command
        .arg("-c")
        .arg(&script)
        .env(ENV_COMPRESS_NO_RTK, "1")
        // See #7401: the `tm compress` at the tail of the pipeline inherits
        // this `sh`'s environment, so the filter pin goes here.
        .env(STATS_LOG_LEVEL.0, STATS_LOG_LEVEL.1)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to run the rewritten pipeline");
    (
        String::from_utf8(output.stdout).expect("stdout is utf8"),
        strip_ansi(&String::from_utf8_lossy(&output.stderr)),
    )
}

#[test]
fn tm_compress_reports_the_wrapped_commands_exit_status() {
    // #7384: `bytes_before=0 … compression_path=rtk_binary` read identically
    // whether the command found nothing, failed, or had its stdout dropped. The
    // filter's OWN status is always 0, so a fix that logged that would report
    // `exit=0` here and fail.
    let (stdout, stderr) = run_wrapped_pipeline("printf boom; exit 3", "cargo test");
    assert!(
        stderr.contains("exit=3"),
        "the stats line must carry the wrapped command's status: {stderr}"
    );
    assert_eq!(
        stdout, "boom",
        "the exit sentinel must never reach the caller's output"
    );
}

#[test]
fn tm_compress_reports_a_signal_killed_wrapped_command() {
    // A signal-killed command reaches the shell as 128 + signal — 137 for
    // SIGKILL — so it needs no separate spelling, but it does need proving.
    let (_stdout, stderr) = run_wrapped_pipeline("kill -9 $$", "cargo test");
    assert!(
        stderr.contains("exit=137"),
        "a SIGKILLed command must report 128+9: {stderr}"
    );
}

#[test]
fn tm_compress_reports_unknown_for_an_unwrapped_invocation() {
    // `tm compress < file`, and any rewrite older than #7384, carry no
    // sentinel. The field must still be present, and must not claim a status.
    //
    // #7607: the payload is one that actually COMPRESSES. A three-byte `"ok\n"`
    // passes through unchanged, and a run that changed nothing and reports no
    // failure now emits no stats line at all — so the old fixture would have
    // asserted `exit=unknown` against a deliberately empty stderr. The field
    // under test is unchanged; only the payload that makes the line exist is.
    let input = repetitive_cargo_test_payload();
    let (success, stdout, stderr) = run_tm_compress("cargo test", &input);
    assert!(success, "tm compress exited non-zero: stderr={stderr}");
    assert!(
        stdout.len() < input.len(),
        "the fixture must compress, or there is no stats line to read"
    );
    let stderr = strip_ansi(&stderr);
    assert!(
        stderr.contains("exit=unknown"),
        "a missing sentinel must read as unknown, not as success: {stderr}"
    );
}

/// A run that changed nothing writes nothing into the caller's tool result.
///
/// Why (#7607): `tm compress` is the tail of the rewritten Bash pipeline, and
/// Claude Code interleaves its stderr into the same tool result as its stdout.
/// A 668-byte `grep` payload came back byte-for-byte with
/// `bytes_before=668 bytes_after=668 pct_reduction=0.0` wedged into the middle
/// of it. Asserted end to end through the real binary because the leak IS the
/// process's stderr — a unit test on the predicate cannot see it.
/// What: a payload below the 80-byte size gate, run unwrapped so no exit status
/// is reported either, with `RUST_LOG=info` pinned so the filter is not what
/// silences the line (#7641: this must be proven with the line ENABLED).
/// Test: this function IS the test.
#[test]
fn a_passthrough_run_leaks_no_stats_line_into_the_tool_result() {
    let input = "ok\n";
    let (success, stdout, stderr) = run_tm_compress("cargo test", input);
    assert!(success, "tm compress exited non-zero: stderr={stderr}");
    assert_eq!(stdout, input, "a sub-gate payload passes through unchanged");
    let stderr = strip_ansi(&stderr);
    assert!(
        !stderr.contains("pct_reduction"),
        "a run that changed nothing must not narrate itself into the tool \
         result (#7607): {stderr}"
    );
    assert!(
        !stderr.contains("tool output compressed"),
        "the stats line's message must be absent too, not merely its fields: {stderr}"
    );
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

/// The operator's own savings ledger, when this process has a `$HOME` at all.
///
/// Why (#7618): the harm is rows in THAT file — not in whichever scratch root a
/// helper points a child at. The assertion has to name the real one.
/// What: `<HOME>/.trusty-mpm/usage/savings.jsonl`; `None` in a stripped
/// environment, which is the CI case and asserts vacuously.
/// Test: used by `a_fixture_payload_never_lands_as_a_session_savings_row`.
fn operator_savings_ledger() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| {
        std::path::Path::new(&home)
            .join(".trusty-mpm")
            .join("usage")
            .join("savings.jsonl")
    })
}

/// Count `compress` rows in `ledger` whose `tokens_before` is `tokens`.
///
/// Why: a whole-file byte comparison would flake on a developer machine, where
/// a live session appends real rows throughout the run. The fixture's own token
/// count is a signature no genuine traffic reproduces — 32 of the 34 rows #7618
/// reports are exactly this one number.
/// What: parses each line and counts the matches; an absent or unreadable
/// ledger counts zero, which is the correct reading for "no fixture row landed".
/// Test: used by both tests below.
fn fixture_rows_in(ledger: &std::path::Path, tokens: u64) -> usize {
    std::fs::read_to_string(ledger)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|row| {
            row["technique"] == "compress" && row["tokens_before"] == serde_json::json!(tokens)
        })
        .count()
}

/// The fixture payload's token count, as the producer derives it.
///
/// What: `bytes / 4`, the byte-proxy `core::savings_compress` uses. 1076 B of
/// [`repetitive_cargo_test_payload`] is the 269 that #7618 counted.
fn fixture_tokens() -> u64 {
    (repetitive_cargo_test_payload().len() / 4) as u64
}

/// This target's fixture payload never reaches the operator's savings ledger.
///
/// Why (#7618): 32 of one session's 34 `compress` rows were ONE event — 269
/// tokens in, 260 saved, `tool` "cargo test" — logged in `native_fallback` +
/// `rtk_binary` PAIRS every few minutes, carrying 99% of that session's reported
/// savings and inflating its `💸` segment ~9x. The pair is
/// [`tm_compress_shrinks_piped_cargo_test_output`] (which forces the native
/// chain) and [`tm_compress_takes_the_rtk_path_when_rtk_is_installed`] (which
/// does not), both running [`repetitive_cargo_test_payload`] through
/// `--tool "cargo test"`; "every few minutes" was an agent re-running
/// `cargo test -p trusty-mpm`. #7514 stopped the bleeding by clearing
/// `CLAUDE_CODE_SESSION_ID` from the child and #7568 gave it a scratch `$HOME`,
/// but nothing PINNED either — a new spawn site that skips
/// `common::tm_command` re-opens it silently, which is exactly how the first 270
/// rows got written.
/// What: drives both arms exactly as the tests above do, and asserts the
/// operator's own ledger gained no row carrying the fixture's token count.
/// Scoped to that signature rather than to the file's bytes, because a live
/// session on the same host appends genuine rows throughout the run.
/// Test: this function IS the test.
#[test]
fn a_fixture_payload_never_lands_as_a_session_savings_row() {
    let tokens = fixture_tokens();
    let ledger = operator_savings_ledger();
    let before = ledger
        .as_deref()
        .map_or(0, |path| fixture_rows_in(path, tokens));

    let input = repetitive_cargo_test_payload();
    // The native arm, then the rtk arm — the exact pair that wrote the 270 rows.
    let (native_ok, _, native_err) = run_tm_compress("cargo test", &input);
    assert!(native_ok, "the native arm must still run: {native_err}");
    if trusty_common::bin_resolve::resolve_binary("rtk").is_some() {
        let (rtk_ok, _, rtk_err) = run_tm_compress_with(&[], "cargo test", &input);
        assert!(rtk_ok, "the rtk arm must still run: {rtk_err}");
    }

    let after = ledger
        .as_deref()
        .map_or(0, |path| fixture_rows_in(path, tokens));
    assert_eq!(
        before, after,
        "this target's fixture payload reached the operator's savings ledger \
         ({:?}) — {tokens}-token `compress` rows went from {before} to {after}. \
         A spawn site here is not going through `common::tm_command` (#7618).",
        ledger
    );
}

/// The hazard, pinned: an unisolated run DOES write the fixture row.
///
/// Why: without this, a `fixture_rows_in` that returned zero for an unrelated
/// reason — a renamed technique, a changed token proxy, a producer that stopped
/// writing — would make the guard above pass while proving nothing. This is the
/// pre-#7514 shape, run against a decoy `$HOME` so the demonstration costs the
/// operator's ledger nothing.
/// What: the same payload and tool, through the sanctioned helper, with the two
/// isolations deliberately overridden — a decoy `$HOME` standing in for the
/// operator's, and a synthetic session id in place of the live one the helper
/// clears. `isolate_spawned_tm` documents that a caller's later `.env` wins,
/// which is what makes the override possible without a second spawn path.
/// Test: this function IS the test.
#[test]
fn an_unisolated_compress_run_writes_the_fixture_row_into_the_home_it_inherits() {
    let decoy = tempfile::tempdir().expect("decoy $HOME");
    let input = repetitive_cargo_test_payload();

    let mut child = common::tm_command()
        .args(["compress", "--tool", "cargo test"])
        .env(ENV_COMPRESS_NO_RTK, "1")
        .env(STATS_LOG_LEVEL.0, STATS_LOG_LEVEL.1)
        // #7618: deliberately NOT isolated — this test exists to show what
        // `common::tm_command` prevents.
        .env("HOME", decoy.path())
        .env(
            trusty_mpm::core::savings::CLAUDE_CODE_SESSION_ID_ENV,
            "fixture-session-7618",
        )
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

    let ledger = decoy
        .path()
        .join(".trusty-mpm")
        .join("usage")
        .join("savings.jsonl");
    assert_eq!(
        fixture_rows_in(&ledger, fixture_tokens()),
        1,
        "an unisolated `tm compress` must write the fixture row into the `$HOME` \
         it was handed — if this stops holding, the guard above is asserting on \
         something nothing produces any more and must be re-pointed. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
