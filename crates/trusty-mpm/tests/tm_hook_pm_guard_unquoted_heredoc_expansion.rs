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
