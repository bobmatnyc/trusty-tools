//! End-to-end proof that a here-document or a line continuation cannot hide
//! a root delete from the rm-root floor (#9180).
//!
//! Why: three shapes passed the floor on 1.7.11 — an outer here-document a
//! shell runs with no terminator, whose nested here-document was never read;
//! a shell name the operator-line scan missed because it was glued to `<<`, a
//! paren, a separator or a substitution; and a `\`-newline continuation the
//! segment splitter cut. Each is now denied, and a here-document the guard
//! cannot place fails closed.
//! What: spawns `tm hook --pm-guard` through the #8878 fixture. Every deny row
//! denies with no bypass and under each bypass, since the floor holds under
//! both; every allow row allows with no bypass.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_heredoc_continuation_9180::`.

use crate::tm_hook_pm_guard_trust_anchor_8878::{Fixture, bash_payload, run};

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// Commands whose verdict under `env` differs from `want_deny`.
fn wrong_verdicts(commands: &[&str], want_deny: bool, env: &[(&str, &str)]) -> Vec<String> {
    let fx = Fixture::new();
    commands
        .iter()
        .filter_map(|command| {
            let out = run(&fx, &bash_payload(&fx, command), env, Some("pm"));
            (out.contains("\"deny\"") != want_deny)
                .then(|| format!("{env:?} want deny={want_deny} {command:?}: {out:?}"))
        })
        .collect()
}

/// Every row denies, under each bypass.
fn assert_denied_everywhere(commands: &[&str]) {
    let wrong: Vec<String> = BYPASSES
        .iter()
        .flat_map(|bypass| {
            let env: Vec<(&str, &str)> = bypass.iter().copied().collect();
            wrong_verdicts(commands, true, &env)
        })
        .collect();
    assert!(wrong.is_empty(), "allowed:\n{}", wrong.join("\n"));
}

/// Class 1: the outer shell-run here-document has no terminator.
#[test]
fn an_unterminated_shell_heredoc_is_read_whole_9180() {
    assert_denied_everywhere(&[
        "bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX",
        "sudo -s <<'O'\ncat <<I\n'$(rm -rf /)'\nI",
        "bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\ncat <<'Z'\nno terminator",
    ]);
}

/// Class 2: the shell's name is glued to an operator, grouped, substituted or
/// quoted on the operator line.
#[test]
fn a_shell_hidden_on_the_operator_line_runs_its_body_9180() {
    assert_denied_everywhere(&[
        "bash<<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO",
        "(bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\n)",
        "true;bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO",
        "x=$(bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\n)",
        "\"bash\" <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO",
    ]);
}

/// Class 3: a continuation splits the delete verb, a substitution opener, or
/// a substitution body.
#[test]
fn a_line_continuation_is_joined_before_the_scan_9180() {
    assert_denied_everywhere(&[
        "r\\\nm -rf /",
        "echo $\\\n(rm -rf /)",
        "echo \"$(r\\\nm -rf /)\"",
        "bash <<'O'\nr\\\nm -rf /\nO",
    ]);
}

/// Error arms: a continuation that moves a body's first or last line, an
/// unterminated body and quotes that do not balance all fail closed.
#[test]
fn an_unplaceable_heredoc_fails_closed_9180() {
    assert_denied_everywhere(&[
        "cat <<X\na\\\nX\ncat <<'Y'\nX\nrm -rf /\nY",
        "cat <\\\n<X\ncat <<'Y'\nX\nrm -rf /\nY",
        "cat <<'X' # note \\\nX\nrm -rf /\nX",
        "bash <<X\nrm -rf build",
        "bash <<'O'\ncat <<X\n'$(rm -rf /)'\nX\nO\necho \"unbalanced",
    ]);
}

/// No-false-deny control: commit messages on stdin or in `$(cat <<'EOF')`,
/// continued argv, a quoted body that only names a delete, and an
/// unterminated body with no delete verb, all allow.
#[test]
fn ordinary_heredoc_and_continuation_commands_allow_9180() {
    let wrong = wrong_verdicts(
        &[
            "git commit -F - <<'EOF'\nfix: never run `rm -rf /` here\n\nRefs #9180\nEOF",
            "git commit -m \"$(cat <<'EOF'\nfix: thing\n\nbody line\nEOF\n)\"",
            "cat <<'EOF' | git commit -F -\nmsg\nEOF",
            "git log --oneline \\\n  -5",
            "python3 - <<'PY'\nprint('a' \\\n  'b')\nPY",
            "cat <<X\nhello there",
        ],
        false,
        &[],
    );
    assert!(wrong.is_empty(), "wrong verdicts:\n{}", wrong.join("\n"));
}
