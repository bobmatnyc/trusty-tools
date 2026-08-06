//! Unit tests for [`super::TmuxDriver`]/[`super::SessionInfo`].
//!
//! Why: extracted from `tmux.rs`'s inline `#[cfg(test)] mod tests` (issue
//! #3714 line-cap follow-up — the new `pane_exists_checked` method pushed
//! that file to 509 SLOC, over the 500-SLOC production cap) into its own
//! file, following this crate's established colocated-test-file convention
//! (e.g. `session_manager/rename.rs` + `rename_tests.rs`) rather than
//! counting inline test SLOC against the production cap. Pure code motion —
//! no behavior/assertion change from the pre-move version.
//! What: OAuth-token redaction coverage (#2246), session/pane row parsing,
//! and the `is_available`/`discover` availability probe.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;

// ── #2246: OAuth-token redaction in error/log strings ───────────────────

const FAKE_TOKEN: &str = "sk-ant-oat01-super-secret-value";

#[test]
fn redact_oauth_token_masks_quoted_value() {
    // The exact shape a managed spawn produces: a single-quoted token inside
    // the `env …` prefix. The value must be gone; the var name must remain.
    let input = format!(
        "export TM_MANAGED_SESSION_ID='id'; env -u ANTHROPIC_API_KEY \
         CLAUDE_CODE_OAUTH_TOKEN='{FAKE_TOKEN}' /usr/bin/claude --resume x"
    );
    let out = redact_oauth_token(&input);
    assert!(
        !out.contains(FAKE_TOKEN),
        "token value must not survive redaction: {out}"
    );
    assert!(
        out.contains("CLAUDE_CODE_OAUTH_TOKEN='<redacted>'"),
        "redacted marker must replace the value: {out}"
    );
    // Surrounding, non-secret context must be preserved intact.
    assert!(out.contains("env -u ANTHROPIC_API_KEY"));
    assert!(out.contains("/usr/bin/claude --resume x"));
}

#[test]
fn redact_oauth_token_masks_unquoted_value() {
    // Defensive: a bare (unquoted) assignment must also be masked, with the
    // value ending at the next whitespace.
    let input = format!("prefix CLAUDE_CODE_OAUTH_TOKEN={FAKE_TOKEN} suffix");
    let out = redact_oauth_token(&input);
    assert!(
        !out.contains(FAKE_TOKEN),
        "unquoted value must be masked: {out}"
    );
    assert_eq!(out, "prefix CLAUDE_CODE_OAUTH_TOKEN=<redacted> suffix");
}

#[test]
fn redact_oauth_token_leaves_unrelated_text_untouched() {
    let input = "tmux [\"send-keys\", \"-t\", \"tmpm-x\", \"-l\", \"echo hi\"] failed: bad";
    assert_eq!(redact_oauth_token(input), input);
}

#[test]
fn run_style_error_message_does_not_leak_oauth_token() {
    // Reproduce exactly how `run` formats a failed send-keys: build the real
    // SendKeys argv carrying a token-bearing pane command, format it the same
    // way, then redact. Proves the error string a failed spawn would surface
    // (and everything mark_errored persists from it) is token-free.
    let keys = format!(
        "export TM_MANAGED_SESSION_ID='abc'; env -u ANTHROPIC_API_KEY \
         CLAUDE_CODE_OAUTH_TOKEN='{FAKE_TOKEN}' claude --dangerously-skip-permissions"
    );
    let argv = tmux_argv(&TmuxCommand::SendKeys {
        target: TmuxTarget::session("tmpm-test"),
        keys,
        literal: true,
    });
    let stderr = "can't find pane";
    let raw = format!("tmux {argv:?} failed: {stderr}");
    // Sanity: the un-redacted message WOULD leak (guards against the test
    // silently passing if the token stopped appearing for another reason).
    assert!(
        raw.contains(FAKE_TOKEN),
        "precondition: raw message leaks the token"
    );
    let redacted = redact_oauth_token(&raw);
    assert!(
        !redacted.contains(FAKE_TOKEN),
        "the error string for a failed spawn must not contain the token: {redacted}"
    );
    assert!(
        redacted.contains("CLAUDE_CODE_OAUTH_TOKEN='<redacted>'"),
        "the token env var must be present but redacted: {redacted}"
    );
}

#[test]
fn find_quoted_value_end_stops_at_bare_closer() {
    // No embedded quote: the end is the first (and only) `'`.
    assert_eq!(find_quoted_value_end("abcdef' rest"), 7);
}

#[test]
fn find_quoted_value_end_skips_escaped_embedded_quote() {
    // shell_single_quote("abc'def") produces the raw command fragment
    // 'abc'\''def' — i.e. AFTER the opening quote: abc'\''def' ...
    // The escaped-quote unit '\'' at index 3 must be skipped so the TRUE
    // closer (index 10) is found, not the first `'` (index 3).
    let after_open = "abc'\\''def' trailing";
    let end = find_quoted_value_end(after_open);
    assert_eq!(
        &after_open[..end],
        "abc'\\''def'",
        "must consume the full escaped value through the true closer"
    );
    assert_eq!(&after_open[end..], " trailing");
}

#[test]
fn redact_oauth_token_masks_value_with_embedded_single_quote() {
    // Reviewer-caught edge case: a token containing a literal `'` is
    // shell-escaped by `runtime::claude_code::shell_single_quote` as
    // '\'' (close-escape-reopen — POSIX's standard way to embed a quote
    // inside a single-quoted string; see that function's doc). A naive
    // "find the first `'` after the opener" scan mistakes that
    // sequence's first byte for the closer and lets everything after it
    // leak in cleartext. Reproduces the exact repro from the review:
    //   RAW:      CLAUDE_CODE_OAUTH_TOKEN='abc'\''def' claude --resume x
    //   WRONG:    CLAUDE_CODE_OAUTH_TOKEN='<redacted>'\''def' ...  (leaks "def")
    //   CORRECT:  CLAUDE_CODE_OAUTH_TOKEN='<redacted>' claude --resume x
    // The secret's escaped encoding is hardcoded here (rather than calling
    // `shell_single_quote`, which is private to `runtime::claude_code`)
    // to keep this test's only dependency the string shape itself.
    let secret_with_quote = "abc'def";
    let input = "CLAUDE_CODE_OAUTH_TOKEN='abc'\\''def' claude --resume x";
    let redacted = redact_oauth_token(input);
    // No fragment of the secret — including the post-quote tail "def" —
    // may survive redaction.
    assert!(
        !redacted.contains("abc"),
        "no fragment of the secret must survive: {redacted}"
    );
    assert!(
        !redacted.contains("def"),
        "the tail after the embedded quote must not leak: {redacted}"
    );
    assert!(
        !redacted.contains(secret_with_quote),
        "the full secret must not survive redaction: {redacted}"
    );
    assert_eq!(
        redacted, "CLAUDE_CODE_OAUTH_TOKEN='<redacted>' claude --resume x",
        "the entire escaped quoted value must collapse to one redacted marker: {redacted}"
    );
}

#[test]
fn parses_session_row() {
    let info = SessionInfo::parse("trusty-mpm-abc:1700000000:1").unwrap();
    assert_eq!(info.name, "trusty-mpm-abc");
    assert_eq!(info.created, 1_700_000_000);
    assert!(info.attached);

    let detached = SessionInfo::parse("s:1:0").unwrap();
    assert!(!detached.attached);
}

/// `#{session_attached}` is a CLIENT COUNT, not a boolean.
///
/// Why: `tm ls` labelled `tm-dogfood-relaunch-01` — the session the operator
/// was sitting in, with two tmux clients on it — `(active)` rather than
/// `(attached)`, while nine single-client sessions read `(attached)`. The
/// parser compared the field to the literal `"1"`, so every count above one
/// fell through to `false`. Two clients on one session is ordinary (a second
/// terminal tab, an `attach -r` observer, a detached-and-reattached shell), so
/// the label failed exactly for the session most likely to be the operator's.
/// What: any count `> 0` is attached; `0` is not; a malformed field still
/// degrades to `false` rather than erroring the whole row.
/// Test: this function IS the test.
#[test]
fn parses_multi_client_session_row_as_attached() {
    for clients in ["1", "2", "3", "17"] {
        let info = SessionInfo::parse(&format!("tm-dogfood-relaunch-01:1785443731:{clients}"))
            .unwrap_or_else(|e| panic!("row with {clients} client(s) must parse: {e}"));
        assert!(
            info.attached,
            "session_attached={clients} is a client COUNT — any non-zero value \
             means a client is connected, so the row must read attached"
        );
    }

    assert!(
        !SessionInfo::parse("tm-detached-01:1785443731:0")
            .unwrap()
            .attached,
        "zero clients is the only value that means detached"
    );

    // A field tmux would never emit must not be read as attachment.
    assert!(
        !SessionInfo::parse("tm-garbage-01:1785443731:banana")
            .unwrap()
            .attached,
        "an unparseable count degrades to detached, never to attached"
    );
}

/// The label must FLIP when the last client goes away.
///
/// Why: a count-aware parser that only ever widened `attached` would still be
/// wrong — the point of reading live tmux is that detaching is observable on
/// the very next `tm ls`. This pins both directions of the transition.
/// What: the same session name parsed at two client counts yields
/// `attached == true` then `attached == false`.
/// Test: this function IS the test.
#[test]
fn attachment_flips_to_detached_when_client_count_drops_to_zero() {
    let name = "tm-dogfood-relaunch-01";
    let while_attached = SessionInfo::parse(&format!("{name}:1785443731:2")).unwrap();
    let after_detach = SessionInfo::parse(&format!("{name}:1785443731:0")).unwrap();

    assert_eq!(while_attached.name, after_detach.name);
    assert!(while_attached.attached);
    assert!(
        !after_detach.attached,
        "once the last client detaches the very next listing must report detached"
    );
}

#[test]
fn parses_managed_pane_row() {
    let row = TmuxDriver::parse_managed_pane_row("tmpm-brave-otter\tclaude\t12345\t%7").unwrap();
    assert_eq!(row.session_name, "tmpm-brave-otter");
    assert_eq!(row.pane_current_command, "claude");
    assert_eq!(row.pane_pid, Some(12345));
    assert_eq!(row.pane_id.as_deref(), Some("%7"));

    // Missing/garbage PID degrades to None but keeps the row (command is key).
    let no_pid = TmuxDriver::parse_managed_pane_row("tmpm-x\tzsh\t\t%9").unwrap();
    assert_eq!(no_pid.pane_pid, None);
    assert_eq!(no_pid.pane_current_command, "zsh");
    assert_eq!(no_pid.pane_id.as_deref(), Some("%9"));

    // A row from an older tmux with no pane_id column degrades to None
    // pane_id rather than dropping the row.
    let no_pane_id = TmuxDriver::parse_managed_pane_row("tmpm-y\tclaude\t222").unwrap();
    assert_eq!(no_pane_id.pane_id, None);
    assert_eq!(no_pane_id.pane_pid, Some(222));

    // Empty session name is dropped entirely.
    assert!(TmuxDriver::parse_managed_pane_row("\tzsh\t1\t%1").is_none());
}

#[test]
fn rejects_malformed_session_row() {
    assert!(SessionInfo::parse("").is_err());
    assert!(SessionInfo::parse("name:not-a-number:0").is_err());
}

#[test]
fn driver_reports_availability() {
    // Works whether or not tmux is installed: discover() either resolves a
    // path or returns a clean Protocol error — never panics.
    let available = TmuxDriver::is_available();
    if !available {
        assert!(TmuxDriver::discover().is_err());
    }
}

// ── #3823: ensure_server_up starts the tmux server before the caller's
// first tmux call, on a machine where tmux has never run ────────────────

/// Write an executable shell script at `dir/name` with body `body`.
///
/// Why: mirrors `core::tmux`'s own `write_fake_tmux` test helper — a
/// deterministic scripted `tmux` binary, no live tmux server required.
fn write_fake_tmux(dir: &std::path::Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write fake tmux script");
    let mut perms = std::fs::metadata(&path)
        .expect("stat fake tmux script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod fake tmux script");
    path.to_string_lossy().into_owned()
}

#[test]
fn ensure_server_up_issues_start_server_on_a_fresh_socket() {
    // Simulates a machine where tmux has never run: the fake binary logs
    // which sub-command it was invoked with and always succeeds. The FIRST
    // (and only) call `ensure_server_up` should make is `start-server`.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("calls.log");
    let script = format!(
        "#!/bin/sh\necho \"$1\" >> '{log}'\nexit 0\n",
        log = log.display()
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-fresh-socket", &script);
    let driver = TmuxDriver { tmux_path: bin };

    driver
        .ensure_server_up()
        .expect("a succeeding fake tmux must report the server as up");

    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.trim(),
        "start-server",
        "ensure_server_up must issue exactly one start-server call: {calls:?}"
    );
}

#[test]
fn ensure_server_up_fails_loudly_when_the_server_never_comes_up() {
    // The fake tmux ALWAYS fails with the exact "no such file" style stderr
    // real tmux emits on a machine where the socket directory itself does
    // not exist yet (distinct from "no server running", which list_sessions
    // already tolerates) — ensure_server_up must surface this as a loud
    // Err, never a silent Ok.
    let dir = tempfile::tempdir().unwrap();
    let script = "#!/bin/sh\necho 'error connecting to /tmp/tmux-502/default (No such file or directory)' >&2\nexit 1\n";
    let bin = write_fake_tmux(dir.path(), "fake-tmux-always-fails", script);
    let driver = TmuxDriver { tmux_path: bin };

    let err = driver
        .ensure_server_up()
        .expect_err("an always-failing tmux must error loudly, never report success");
    assert!(
        matches!(err, Error::Protocol(_)),
        "must surface as a Protocol error: {err:?}"
    );
}
