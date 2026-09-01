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
    let row = TmuxDriver::parse_managed_pane_row("tmpm-brave-otter\tclaude\t12345\t%7\t/work/repo")
        .unwrap();
    assert_eq!(row.session_name, "tmpm-brave-otter");
    assert_eq!(row.pane_current_command, "claude");
    assert_eq!(row.pane_pid, Some(12345));
    assert_eq!(row.pane_id.as_deref(), Some("%7"));
    assert_eq!(row.pane_current_path.as_deref(), Some("/work/repo"));

    // #6529: the columns the format string asks for are all required. A short,
    // over-long, or non-numeric row is a reply we did not request, and guessing
    // which column is which is exactly what produced the live failure.
    assert!(TmuxDriver::parse_managed_pane_row("tmpm-x\tzsh\t\t%9\t/w").is_none());
    assert!(TmuxDriver::parse_managed_pane_row("tmpm-y\tclaude\t222").is_none());
    assert!(TmuxDriver::parse_managed_pane_row("tmpm-z\tzsh\t1\t%1\t/w\textra").is_none());
    assert!(TmuxDriver::parse_managed_pane_row("\tzsh\t1\t%1\t/w").is_none());
    assert!(TmuxDriver::parse_managed_pane_row("tmpm-q\t\t1\t%1\t/w").is_none());
}

/// #6118: the fifth column is the only OPTIONAL one — an empty value is "tmux
/// told us nothing about this pane's cwd", which the orphan-GC can never reap
/// on. It must parse to `None`, not to an empty path that would resolve to the
/// daemon's own working directory.
#[test]
fn parses_pane_row_with_empty_current_path() {
    let row = TmuxDriver::parse_managed_pane_row("tm-a\tclaude\t12\t%1\t").expect("row parses");
    assert_eq!(row.pane_current_path, None);
    let row = TmuxDriver::parse_managed_pane_row("tm-a\tclaude\t12\t%1\t   ").expect("row parses");
    assert_eq!(row.pane_current_path, None);
}

/// #6118: a row carrying only the pre-#6118 four columns is DROPPED, not
/// accepted with a guessed-absent trailing field. Dropping a row can only ever
/// spare a session — accepting a shape we did not ask for is how #6529 happened.
#[test]
fn four_column_pane_row_is_dropped() {
    assert!(TmuxDriver::parse_managed_pane_row("tm-a\tclaude\t12\t%1").is_none());
}

/// A captured REAL `list-panes -a -F` listing parses column-for-column (#6529).
///
/// Why: every pre-#6529 test built `PaneInfo` from a literal, so nothing
/// asserted that a row tmux actually emits survives the parse. These four rows
/// are verbatim output from `tmux 3.6b` on the host where the orphan-GC stalled,
/// tabs and `%NNN` pane ids included.
/// What: drives the production listing parser and pins every field of every row.
/// Test: this is the test.
#[test]
fn parses_a_real_tab_delimited_listing() {
    let captured = "tm-00b14f3b-dd31-4474-9\tzsh\t74149\t%1860\t/tmp/gone-worktree\n\
                    tm-trusty-tools-01\tzsh\t10302\t%2306\t/Users/masa/trusty-tools\n\
                    tm-trusty-tools-02\ttrusty-mpm\t10738\t%2307\t/Users/masa/trusty-tools\n\
                    tm-writing-03\tzsh\t53398\t%2105\t/Users/masa/writing\n";
    let rows = TmuxDriver::parse_managed_pane_rows(captured);
    assert_eq!(rows.len(), 4, "every captured row must parse: {rows:?}");
    assert_eq!(rows[0].session_name, "tm-00b14f3b-dd31-4474-9");
    assert_eq!(rows[0].pane_current_command, "zsh");
    assert_eq!(rows[0].pane_pid, Some(74149));
    assert_eq!(rows[0].pane_id.as_deref(), Some("%1860"));
    // #6118: the cwd column is what makes a declined-adopt pane reapable.
    assert_eq!(
        rows[0].pane_current_path.as_deref(),
        Some("/tmp/gone-worktree")
    );
    assert_eq!(rows[2].session_name, "tm-trusty-tools-02");
    assert_eq!(rows[2].pane_current_command, "trusty-mpm");
    assert_eq!(rows[2].pane_id.as_deref(), Some("%2307"));
}

/// The #6529 live failure: tmux's own sanitized reply must be DROPPED (#6529).
///
/// Why: a tmux client that declares no UTF-8 locale gets every byte below
/// `0x20` replaced with `_` by the server, so the tab columns arrive joined.
/// The pre-fix parser folded the whole row into `session_name` with an empty
/// command; `classify_session` then matched no registry entry and read
/// `is_idle_shell("")` as busy, so 456 orphaned sessions survived every sweep
/// for days. These rows are verbatim from that daemon's log.
/// What: asserts the sanitized rows produce NO panes at all — never a pane
/// whose name is the joined row.
/// Test: this is the test. RED before the fix: three coerced `PaneInfo` rows.
#[test]
fn sanitized_pane_row_is_dropped_not_coerced() {
    let sanitized = "tm-trusty-tools-01_zsh_10302_%2306\n\
                     tm-trusty-tools-02_trusty-mpm_10738_%2307\n\
                     tm-writing-03_zsh_53398_%2105\n";
    let rows = TmuxDriver::parse_managed_pane_rows(sanitized);
    assert!(
        rows.is_empty(),
        "a row with no tab columns must be dropped, never coerced into a \
         session name: {rows:?}"
    );
}

/// The live listing against this host's real tmux server (#6529).
///
/// Why: the parse tests above prove the pure function; only a real listing
/// proves the SPAWN asks tmux for — and receives — the columns, which is where
/// #6529 actually broke. Read-only: `list-panes` creates and kills nothing.
/// What: lists every pane on the operator's server and asserts each row carries
/// a non-empty command and a `%`-prefixed pane id, i.e. that the delimiters
/// survived. Skips cleanly when no tmux binary or no server is present.
/// Test: this is the test. `#[ignore]` because it needs the operator's server.
#[test]
#[ignore = "needs the host's live tmux server"]
fn live_listing_keeps_its_columns() {
    let Ok(driver) = TmuxDriver::discover() else {
        eprintln!("no tmux binary — skipping");
        return;
    };
    let panes = driver
        .list_managed_panes()
        .expect("list_managed_panes must not error against a live server");
    if panes.is_empty() {
        eprintln!("no tmux server running — skipping");
        return;
    }
    for pane in &panes {
        assert!(
            !pane.pane_current_command.is_empty(),
            "a parsed pane always carries its command: {pane:?}"
        );
        assert!(
            !pane.session_name.contains('_') || !pane.session_name.contains('%'),
            "session_name must never hold a joined row (#6529): {pane:?}"
        );
        assert!(
            pane.pane_id
                .as_deref()
                .is_some_and(|id| id.starts_with('%')),
            "a parsed pane always carries its tmux pane id: {pane:?}"
        );
    }
}

#[test]
fn rejects_malformed_session_row() {
    assert!(SessionInfo::parse("").is_err());
    assert!(SessionInfo::parse("name:not-a-number:0").is_err());
}

#[serial_test::serial]
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

#[serial_test::serial]
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

#[serial_test::serial]
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

// ── #6411: the empty-server stderr classification boundary ──────────────

#[test]
fn empty_server_stderr_is_an_empty_list() {
    // The two spellings real tmux emits when the SERVER is up and holds
    // nothing. Both mean "there is nothing to list", so every listing maps
    // them to an empty collection rather than an error.
    assert!(stderr_means_empty_server(
        "no server running on /tmp/tmux-501/default\n"
    ));
    assert!(stderr_means_empty_server("no sessions\n"));
}

#[test]
fn unreachable_socket_stderr_is_not_an_empty_list() {
    // What real tmux prints on a host where no server has EVER run: the
    // socket path does not exist yet. That is not an empty server, and
    // classifying it as one would let `reap_dead_sessions` see an empty live
    // set and delete every registered tmux session, so it must stay an Err.
    //
    // This is the gap that made `main` red: `TmuxDriver::discover()` succeeds
    // on such a host (the BINARY resolves) while every listing fails, so
    // `discover().is_ok()` must never be used to predict a reap outcome.
    assert!(!stderr_means_empty_server(
        "error connecting to /tmp/tmux-501/default (No such file or directory)"
    ));
    assert!(!stderr_means_empty_server("lost server"));
}

/// #3707: tmux's refusal to reuse a live name is the retryable outcome the
/// managed create path keys on.
#[test]
fn duplicate_session_stderr_is_recognised() {
    assert!(stderr_means_duplicate_session(
        "duplicate session: tm-proj-01\n"
    ));
}

/// #3707: any OTHER non-zero exit stays an error. Reading every failure as a
/// name collision would send the caller round a rename-and-retry loop against
/// a server that is actually broken.
#[test]
fn unrelated_stderr_is_not_a_name_collision() {
    assert!(!stderr_means_duplicate_session(
        "error connecting to /tmp/tmux-501/default (No such file or directory)"
    ));
    assert!(!stderr_means_duplicate_session("no server running"));
    assert!(!stderr_means_duplicate_session("can't create socket"));
}

#[test]
fn list_sessions_errors_on_a_host_whose_tmux_server_never_ran() {
    // The end-to-end shape of the gap, through the real method: a tmux that
    // resolves fine but cannot reach a server. `list_sessions` must return
    // Err, because `reap_dead_sessions` turns an Err into "reap nothing" and
    // an Ok(empty) into "every registered tmux session is dead". A caller
    // that gated on binary resolution alone would predict the wrong one.
    let dir = tempfile::tempdir().unwrap();
    let script = "#!/bin/sh\necho 'error connecting to /tmp/tmux-501/default (No such file or directory)' >&2\nexit 1\n";
    let bin = write_fake_tmux(dir.path(), "fake-tmux-no-server", script);
    let driver = TmuxDriver { tmux_path: bin };

    let err = driver
        .list_sessions()
        .expect_err("an unreachable tmux server must not read as an empty session list");
    assert!(
        matches!(err, Error::Protocol(_)),
        "must surface as a Protocol error: {err:?}"
    );
}
