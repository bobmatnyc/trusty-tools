//! End-to-end proof that a substitution in an unquoted here-document body
//! denies even inside single quotes.
//!
//! Why: with an unquoted delimiter (`<<PY`) the shell expands `$(…)` and
//! backticks in the body before the program reads it, and a `'` in the body is
//! a literal character, not quoting. The guard read a body that an interpreter
//! or shell runs (`python3`, `node`, `bash`) with the argv quote map, so
//! `print('$(rm -rf /)')` looked single-quoted and the rm-root floor allowed
//! it, while the same body under `cat`, or in double quotes, denied.
//! What: spawns `tm hook --pm-guard` through the #8878 fixture and checks that
//! each such body denies under each bypass, while the same body behind a
//! quoted delimiter (`<<'PY'`), which the shell never expands, still allows.
//! The security critic's round-2 probes (an unterminated body, a line
//! continuation, a here-document nested in a shell-run body or a `bash -c`
//! string) are pinned the same way.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_unquoted_heredoc_expansion::`.

use crate::tm_hook_pm_guard_trust_anchor_8878::{Fixture, bash_payload, run};

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// Unquoted-delimiter bodies whose single-quoted substitution the shell runs.
const EXPANDED_IN_SINGLE_QUOTES: [&str; 4] = [
    // The live-probe report, allowed on dcd33f9591.
    "python3 - <<PY\nprint('$(rm -rf /)')\nPY",
    "bash <<X\necho '$(rm -rf /)'\nX",
    "python3 - <<PY\nprint('`rm -rf /`')\nPY",
    "node - <<JS\nconsole.log('$(rm -rf /)')\nJS",
];

/// Every body above denies, under each bypass.
#[test]
fn a_single_quoted_substitution_in_an_unquoted_heredoc_body_denies() {
    let fx = Fixture::new();
    let mut allowed = Vec::new();
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in EXPANDED_IN_SINGLE_QUOTES {
            let out = run(&fx, &bash_payload(&fx, command), &env, Some("pm"));
            if !out.contains("\"deny\"") {
                allowed.push(format!("{bypass:?} {command:?}: {out:?}"));
            }
        }
    }
    assert!(allowed.is_empty(), "allowed:\n{}", allowed.join("\n"));
}

/// No-false-deny control: behind a quoted delimiter the body is literal text,
/// so the same `$(…)` never runs and the command allows. An unquoted body
/// whose single-quoted string holds no substitution expands to itself, and
/// allows too.
#[test]
fn a_single_quoted_substitution_in_a_quoted_heredoc_body_allows() {
    let fx = Fixture::new();
    for command in [
        "python3 - <<'PY'\nprint('$(rm -rf /)')\nPY",
        "node - <<'JS'\nconsole.log('$(rm -rf /)')\nJS",
        // #9155: the false-deny control for the expanding-body scan.
        "python3 - <<PY\nprint('hello, it is rm day')\nPY",
    ] {
        let out = run(&fx, &bash_payload(&fx, command), &[], Some("pm"));
        assert!(!out.contains("\"deny\""), "{command:?}: {out}");
    }
}

/// Every `deny` row denies and every `allow` row allows, with no bypass.
fn assert_verdicts(deny: &[&str], allow: &[&str]) {
    let fx = Fixture::new();
    let mut wrong = Vec::new();
    for (commands, want_deny) in [(deny, true), (allow, false)] {
        for command in commands {
            let out = run(&fx, &bash_payload(&fx, command), &[], Some("pm"));
            if out.contains("\"deny\"") != want_deny {
                wrong.push(format!("want deny={want_deny} {command:?}: {out:?}"));
            }
        }
    }
    assert!(wrong.is_empty(), "wrong verdicts:\n{}", wrong.join("\n"));
}

/// #9155 round 2: bash runs an unterminated unquoted body to end of input, a
/// trailing-space line included, and expands it there. A benign unterminated
/// body allows.
#[test]
fn an_unterminated_unquoted_heredoc_body_is_expanded_9155() {
    assert_verdicts(
        &[
            "cat <<X\n'$(rm -rf /)'",
            "bash <<X\necho '$(rm -rf /)'",
            "python3 - <<PY\nprint('$(rm -rf /)')",
            "cat <<X\n'$(rm -rf /)'\nX ",
        ],
        &["cat <<X\nhello there"],
    );
}

/// #9155 round 2: bash removes `\`+newline from an unquoted body before it
/// expands it, so `$\`, newline, `(` is an opener. An even backslash run is
/// not, and a benign continuation allows.
#[test]
fn a_line_continuation_in_an_unquoted_heredoc_body_is_joined_9155() {
    assert_verdicts(
        &[
            "cat <<X\n'$\\\n(rm -rf /)'\nX",
            "python3 - <<PY\nprint('$\\\n(rm -rf /)')\nPY",
        ],
        &[
            "cat <<X\n'$\\\\\n(rm -rf /)'\nX",
            "python3 - <<PY\nprint('a' \\\n  'b')\nPY",
        ],
    );
}

/// #9155 round 2: an unquoted here-document nested in a shell-run body or a
/// wrapper string is read whole. A benign nested body allows.
#[test]
fn a_heredoc_nested_in_a_shell_run_body_or_wrapper_is_expanded_9155() {
    assert_verdicts(
        &[
            "bash <<'O'\nbash <<I\necho '$(rm -rf /)'\nI\nO",
            "cat <<'O' | bash\nbash <<I\necho '$(rm -rf /)'\nI\nO",
            "sudo -s <<'O'\ncat <<I\n'$(rm -rf /)'\nI\nO",
            "bash -c \"bash <<I\necho '\\$(rm -rf /)'\nI\"",
        ],
        &["bash <<'O'\nbash <<I\necho hello\nI\nO"],
    );
}
