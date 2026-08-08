//! Unit tests for trusty-mpm's tmux adapter (#2398/#3386/#5151), split out
//! of `tmux.rs` to keep that file under the 500-SLOC production cap (#610) —
//! the inline `mod tests` counted as production source.
//!
//! Every process-spawning test drives a scripted fake `tmux` binary written
//! to a temp dir, so none of them needs a live tmux server.

use super::*;

#[test]
fn resolve_tmux_binary_does_not_panic() {
    // Works whether or not tmux is installed on the test host.
    let _ = resolve_tmux_binary();
}

#[test]
fn resolve_tmux_binary_or_bare_never_empty() {
    // Even when resolution fails entirely, the bare "tmux" fallback keeps
    // this non-empty — callers always get a spawnable binary name.
    assert!(!resolve_tmux_binary_or_bare().is_empty());
}

#[test]
fn run_tmux_with_bin_does_not_panic_on_missing_binary() {
    // A deliberately bogus binary name must surface as an Err, never panic.
    let result = run_tmux_with_bin(
        "definitely-not-a-real-tmux-binary-2398",
        &TmuxCommand::ListSessions,
    );
    assert!(result.is_err());
}

#[test]
fn create_managed_session_does_not_panic_on_missing_binary() {
    // Same smoke guarantee for the session-creation choke point: a bogus
    // binary must degrade to an Err (from the final new-session spawn),
    // never panic — including through the best-effort scrollback loop.
    let result = create_managed_session(
        Some("definitely-not-a-real-tmux-binary-2398"),
        "tmpm-test-2398-no-bin",
        None,
    );
    assert!(result.is_err());
}

#[test]
fn managed_session_command_sequence_matches_shared_layer() {
    // #3004 integration assertion: this crate's config-resolved
    // sequence must be EXACTLY what `trusty_common::tmux` would build
    // for `idempotent: true, command: None` (trusty-mpm's creation
    // semantics) with the same parameters — proving trusty-mpm's
    // session creation genuinely routes through the shared layer
    // rather than a local re-implementation drifting from it.
    let got =
        managed_session_command_sequence("tmpm-sess", Some("/tmp/proj"), 100_000, true, false);
    let want = trusty_common::tmux::managed_session_commands(
        "tmpm-sess",
        Some("/tmp/proj"),
        100_000,
        true,
        false,
        true,
        None,
    );
    assert_eq!(got, want);
}

#[test]
fn managed_session_command_sequence_applies_options_before_new_session() {
    let cmds = managed_session_command_sequence("tmpm-sess", None, 50_000, false, false);
    assert_eq!(cmds.len(), 4);
    assert!(matches!(cmds[0], TmuxCommand::SetGlobalOption { .. }));
    assert!(matches!(cmds[1], TmuxCommand::SetGlobalOption { .. }));
    // #5151: the alternate-screen entry rides the same pre-new-session
    // path, at the WINDOW scope.
    assert!(matches!(cmds[2], TmuxCommand::SetWindowGlobalOption { .. }));
    assert!(matches!(cmds[3], TmuxCommand::NewSession { .. }));
}

// ── #3386: server-up confirmed before any `set-option -g` ───────────
//
// All of these drive a scripted fake `tmux` binary (a `#!/bin/sh` script
// written to a temp dir) rather than a live tmux server, so they are
// deterministic and safe to run anywhere `sh` exists.

/// Write an executable shell script at `dir/name` with body `body`,
/// returning its path as a `String` suitable for `create_managed_session`
/// / `run_tmux_with_bin`'s `bin` parameter.
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
fn ensure_server_up_retries_then_succeeds() {
    // Fails the first two `start-server` invocations (simulating the
    // #3386 window right after a dead server's socket dir is torn down),
    // then succeeds on the third — exactly `START_SERVER_MAX_ATTEMPTS`.
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("attempts");
    std::fs::write(&counter, "0").unwrap();
    let script = format!(
        "#!/bin/sh\nn=$(cat '{counter}')\nn=$((n + 1))\necho \"$n\" > '{counter}'\nif [ \"$n\" -lt 3 ]; then\n  echo 'error connecting to socket (No such file or directory)' >&2\n  exit 1\nfi\nexit 0\n",
        counter = counter.display()
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-retry", &script);

    let result = ensure_server_up(&bin);

    assert!(
        result.is_ok(),
        "must eventually succeed within {START_SERVER_MAX_ATTEMPTS} attempts: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().trim(),
        "3",
        "must have retried exactly up to the succeeding attempt"
    );
}

#[test]
fn ensure_server_up_fails_loudly_after_exhausting_retries() {
    // Always fails — the probe must return a loud Err after exhausting
    // every attempt rather than silently reporting success.
    let dir = tempfile::tempdir().unwrap();
    let script =
        "#!/bin/sh\necho 'error connecting to socket (No such file or directory)' >&2\nexit 1\n";
    let bin = write_fake_tmux(dir.path(), "fake-tmux-always-fails", script);

    let result = ensure_server_up(&bin);

    assert!(
        result.is_err(),
        "an always-failing server probe must error loudly, never proceed as if it succeeded"
    );
    assert!(
        result.unwrap_err().contains("error connecting to socket"),
        "the Err must carry the underlying failure reason"
    );
}

#[test]
fn probe_history_limit_reads_back_configured_value() {
    let dir = tempfile::tempdir().unwrap();
    let script = "#!/bin/sh\necho 100000\nexit 0\n";
    let bin = write_fake_tmux(dir.path(), "fake-tmux-show-options", script);

    assert_eq!(probe_history_limit(&bin), Ok(100_000));
}

#[test]
fn probe_history_limit_errors_on_unparsable_output() {
    let dir = tempfile::tempdir().unwrap();
    let script = "#!/bin/sh\necho 'not-a-number'\nexit 0\n";
    let bin = write_fake_tmux(dir.path(), "fake-tmux-bad-output", script);

    let result = probe_history_limit(&bin);
    assert!(
        result.is_err(),
        "unparsable show-options output must error loudly, not silently pass verification"
    );
}

#[test]
fn probe_history_limit_errors_on_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let script = "#!/bin/sh\necho 'no server running' >&2\nexit 1\n";
    let bin = write_fake_tmux(dir.path(), "fake-tmux-show-options-fails", script);

    assert!(probe_history_limit(&bin).is_err());
}

// ── #5151: alternate-screen probe ───────────────────────────────────

/// A `show-options)` case body for a fake tmux that answers each probe
/// by OPTION NAME (`$4`), so the `history-limit` and `alternate-screen`
/// probes can be given different answers — the whole point of #5151 is
/// that they are separate options in separate scopes.
fn fake_show_options_case(history_limit: &str, alternate_screen: &str) -> String {
    format!(
        "  show-options)\n    case \"$4\" in\n      history-limit) echo {history_limit} ;;\n      alternate-screen) echo {alternate_screen} ;;\n    esac\n    ;;\n"
    )
}

#[test]
fn probe_alternate_screen_reads_back_on() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake_tmux(
        dir.path(),
        "fake-tmux-alt-on",
        "#!/bin/sh\necho on\nexit 0\n",
    );
    assert_eq!(probe_alternate_screen(&bin), Ok(true));
}

#[test]
fn probe_alternate_screen_reads_back_off() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake_tmux(
        dir.path(),
        "fake-tmux-alt-off",
        "#!/bin/sh\necho off\nexit 0\n",
    );
    assert_eq!(probe_alternate_screen(&bin), Ok(false));
}

#[test]
fn probe_alternate_screen_errors_on_unrecognised_value() {
    // Anything tmux does not document must NOT be guessed at — a wrong
    // guess here is a silent pass on an option that did not land.
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake_tmux(
        dir.path(),
        "fake-tmux-alt-garbage",
        "#!/bin/sh\necho maybe\nexit 0\n",
    );
    assert!(probe_alternate_screen(&bin).is_err());
}

#[test]
fn probe_alternate_screen_errors_on_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let bin = write_fake_tmux(
        dir.path(),
        "fake-tmux-alt-fails",
        "#!/bin/sh\necho 'no server running' >&2\nexit 1\n",
    );
    assert!(probe_alternate_screen(&bin).is_err());
}

#[test]
fn probe_alternate_screen_queries_the_window_scope() {
    // #5151 fail-open guard: the probe must read `-wg`, the same scope
    // `scrollback_option_commands` writes. Measured against live tmux
    // 3.6b, `set-option -pg alternate-screen off` exits 0, a `-pg`
    // readback reports "off", and the pane STILL enters the alternate
    // screen — a probe on any other scope would confirm nothing.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\necho off\nexit 0\n",
        log = log.display()
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-alt-argv", &script);

    assert_eq!(probe_alternate_screen(&bin), Ok(false));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap().trim(),
        "show-options -wg -v alternate-screen"
    );
}

#[test]
fn create_managed_session_confirms_server_before_applying_options() {
    // #3386 end-to-end (still no live tmux): a fake `tmux` that records
    // each invocation's tmux sub-command (argv[0]) to a log file, always
    // succeeds, and echoes back the CONFIGURED history-limit for
    // `show-options` so the apply-and-verify cycle matches on its first
    // attempt. This exercises the EXACT choke point the
    // resume-after-server-death path (`SessionManager::resume` →
    // `resume_workdir::create_and_verify_pane` → `RealTmuxDriver::
    // create_session` → `TmuxDriver::create_session`) routes through, so
    // asserting the recorded order here proves the fix for that path too.
    //
    // The expected history-limit is read via the SAME config-resolution
    // path `create_managed_session` itself uses (rather than a hardcoded
    // constant) so this test stays correct regardless of the host's own
    // `~/.trusty-mpm` config.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("calls.log");
    let config = crate::core::trusty_tools_config::TrustyToolsConfig::load();
    let opts = crate::core::trusty_tools_config::resolve_tmux_options(&config);
    let script = format!(
        "#!/bin/sh\necho \"$1\" >> '{log}'\ncase \"$1\" in\n{show_options}esac\nexit 0\n",
        log = log.display(),
        show_options = fake_show_options_case(
            &opts.history_limit.to_string(),
            if opts.alternate_screen { "on" } else { "off" },
        )
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-order", &script);

    let outcome = create_managed_session(Some(&bin), "tmpm-3386-order-test", None)
        .expect("fake tmux always exits 0");

    assert!(
        outcome.options_verified,
        "a fake tmux that echoes back the configured history-limit must verify successfully"
    );

    let calls = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = calls.lines().collect();

    assert_eq!(
        lines.first(),
        Some(&"start-server"),
        "the server must be confirmed up BEFORE any set-option -g (#3386): {lines:?}"
    );
    let new_session_pos = lines
        .iter()
        .position(|l| *l == "new-session")
        .expect("new-session must still run");
    let set_option_positions: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == "set-option")
        .map(|(i, _)| i)
        .collect();
    assert!(
        !set_option_positions.is_empty(),
        "the scrollback/mouse options must still be applied: {lines:?}"
    );
    assert!(
        set_option_positions
            .iter()
            .all(|&p| p > 0 && p < new_session_pos),
        "set-option -g must run strictly AFTER start-server and BEFORE new-session \
         (#3386 / pre-existing #2398 ordering guarantee): {lines:?}"
    );
}

// ── #3386 review: apply-and-verify retries the WHOLE cycle ──────────

#[test]
fn apply_and_verify_scrollback_options_succeeds_on_second_attempt() {
    // The probe reports the WRONG value on the first cycle, then the
    // correct value from the second cycle onward — the whole
    // apply-and-verify cycle (not just start-server) must retry and
    // eventually confirm success.
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("probe-attempts");
    std::fs::write(&counter, "0").unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  show-options)\n    case \"$4\" in\n      history-limit)\n        n=$(cat '{counter}')\n        n=$((n + 1))\n        echo \"$n\" > '{counter}'\n        if [ \"$n\" -lt 2 ]; then\n          echo 2000\n        else\n          echo 100000\n        fi\n        ;;\n      alternate-screen) echo on ;;\n    esac\n    ;;\nesac\nexit 0\n",
        counter = counter.display()
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-verify-retry", &script);
    let options = trusty_common::tmux::scrollback_option_commands(100_000, true, true);

    let verified = apply_and_verify_scrollback_options(&bin, &options, 100_000, true);

    assert!(
        verified,
        "must succeed once the probe reports the expected value on a later attempt"
    );
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().trim(),
        "2",
        "must have retried the WHOLE cycle (not just start-server) exactly once"
    );
}

#[test]
fn apply_and_verify_scrollback_options_returns_false_after_exhausting_retries() {
    // The probe ALWAYS reports the wrong value — every apply-and-verify
    // cycle must be attempted (never silently assumed to have worked),
    // and the function must return `false` (the caller-visible degraded
    // signal) rather than `true` once every attempt is exhausted.
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("probe-attempts");
    std::fs::write(&counter, "0").unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  show-options)\n    case \"$4\" in\n      history-limit)\n        n=$(cat '{counter}')\n        n=$((n + 1))\n        echo \"$n\" > '{counter}'\n        echo 2000\n        ;;\n      alternate-screen) echo on ;;\n    esac\n    ;;\nesac\nexit 0\n",
        counter = counter.display()
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-verify-always-wrong", &script);
    let options = trusty_common::tmux::scrollback_option_commands(100_000, true, true);

    let verified = apply_and_verify_scrollback_options(&bin, &options, 100_000, true);

    assert!(
        !verified,
        "must return false (never silently proceed as verified) once every \
         apply-and-verify attempt is exhausted — this is the #3386 review's caller-visible \
         degraded signal"
    );
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().trim(),
        APPLY_VERIFY_MAX_ATTEMPTS.to_string(),
        "must have made exactly APPLY_VERIFY_MAX_ATTEMPTS probe attempts"
    );
}

// ── #5151: alternate-screen rides the same verified path ────────────

#[test]
fn apply_and_verify_scrollback_options_fails_when_only_alternate_screen_is_wrong() {
    // #5151 regression: history-limit reads back correctly, but the
    // server reports `alternate-screen on` when `off` was configured —
    // exactly the shape a wrong-scope `set-option` produces. Verification
    // must fail. Before alternate-screen was probed at all this returned
    // `true`, reporting an ergonomics set that never landed.
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n{show_options}esac\nexit 0\n",
        show_options = fake_show_options_case("100000", "on")
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-alt-mismatch", &script);
    let options = trusty_common::tmux::scrollback_option_commands(100_000, true, false);

    let verified = apply_and_verify_scrollback_options(&bin, &options, 100_000, false);

    assert!(
        !verified,
        "a correct history-limit must not verify away a wrong alternate-screen — that is \
         the fail-open #5151 exists to close"
    );
}

#[test]
fn apply_and_verify_scrollback_options_verifies_alternate_screen_off() {
    // The positive counterpart: when the server DOES report the
    // configured `off`, the cycle verifies on its first attempt.
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n{show_options}esac\nexit 0\n",
        show_options = fake_show_options_case("100000", "off")
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-alt-match", &script);
    let options = trusty_common::tmux::scrollback_option_commands(100_000, true, false);

    assert!(apply_and_verify_scrollback_options(
        &bin, &options, 100_000, false
    ));
}

#[test]
fn apply_and_verify_probes_the_same_scope_it_sets() {
    // #5151 fail-open guard, asserted on the real argv the apply-and-
    // verify cycle emits: the `alternate-screen` set and its readback
    // must carry the SAME scope flag. A set at one scope verified by a
    // probe at another reports success for an option that did nothing.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("argv.log");
    let script = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\ncase \"$1\" in\n{show_options}esac\nexit 0\n",
        log = log.display(),
        show_options = fake_show_options_case("100000", "off")
    );
    let bin = write_fake_tmux(dir.path(), "fake-tmux-scope-log", &script);
    let options = trusty_common::tmux::scrollback_option_commands(100_000, true, false);

    assert!(apply_and_verify_scrollback_options(
        &bin, &options, 100_000, false
    ));

    let calls = std::fs::read_to_string(&log).unwrap();
    let scope_of = |prefix: &str| -> String {
        let line = calls
            .lines()
            .find(|l| l.starts_with(prefix) && l.contains("alternate-screen"))
            .unwrap_or_else(|| panic!("no {prefix} call for alternate-screen in: {calls}"));
        line.split_whitespace()
            .nth(1)
            .expect("scope flag")
            .to_string()
    };

    let set_scope = scope_of("set-option");
    let show_scope = scope_of("show-options");
    assert_eq!(set_scope, "-wg", "alternate-screen must be set at -wg");
    assert_eq!(
        show_scope, set_scope,
        "the readback must query the scope the set wrote: {calls}"
    );
}
