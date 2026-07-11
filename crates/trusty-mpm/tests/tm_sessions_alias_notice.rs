//! Integration test for the `tm session` -> `tm sessions` top-level rename
//! (issue #2116, DOC-35 §2.2/§3.2).
//!
//! Why: `main.rs`'s dispatch match only fires the deprecation notice for the
//! `Command::Session` (singular) arm, never for `Command::Sessions` (plural).
//! That "exactly once per invocation, not per verb" invariant is a real
//! process-level property (stderr output of the actual binary), so it is
//! proven here by spawning the compiled `tm` binary — the same technique
//! `tests/tm_compress_pipe.rs` uses — rather than by unit-testing the pure
//! message builder alone (that narrower assertion already lives in
//! `top_level_alias_notice_message` in `src/bin/tm/tests.rs`).
//! What: runs `tm --url http://127.0.0.1:1 session list` and `tm --url
//! http://127.0.0.1:1 sessions list`. Port 1 is always refused (see
//! `core::discovery`'s own tests), so the daemon call fails fast without a
//! live daemon; the deprecation notice — printed before that network call —
//! is unaffected by the subsequent failure.
//! Test: `cargo test -p trusty-mpm --test tm_sessions_alias_notice`.

use std::process::Command;

/// Deterministic never-listening address (reserved port), so the daemon round
/// trip fails immediately instead of hanging or requiring a live daemon.
const DEAD_URL: &str = "http://127.0.0.1:1";

fn run_tm(args: &[&str]) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_tm");
    let output = Command::new(bin)
        .args(args)
        .output()
        .expect("failed to spawn `tm`");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn tm_session_singular_prints_deprecation_notice_once() {
    let (_, stderr) = run_tm(&["--url", DEAD_URL, "session", "list"]);
    let notice = "warning: 'session' is deprecated; use 'sessions'";
    assert_eq!(
        count_occurrences(&stderr, notice),
        1,
        "expected the deprecation notice exactly once, got stderr: {stderr:?}"
    );
}

#[test]
fn tm_sessions_plural_prints_no_deprecation_notice() {
    let (_, stderr) = run_tm(&["--url", DEAD_URL, "sessions", "list"]);
    assert!(
        !stderr.contains("is deprecated"),
        "canonical `sessions` must never print a deprecation notice, got stderr: {stderr:?}"
    );
}

#[test]
fn tm_session_alias_notice_fires_once_regardless_of_verb() {
    // Cross-check with a different verb (no positional id required) to prove
    // the "once per invocation, not once per verb/subcommand-layer" property
    // is not an artifact of picking `list` specifically.
    let (_, stderr) = run_tm(&["--url", DEAD_URL, "session", "breakers"]);
    let notice = "warning: 'session' is deprecated; use 'sessions'";
    assert_eq!(
        count_occurrences(&stderr, notice),
        1,
        "expected the deprecation notice exactly once for `session breakers`, got stderr: {stderr:?}"
    );
}
