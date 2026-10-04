//! End-to-end proof that a quoted here-document delimiter holding a word-break
//! byte cannot hide a substitution from the guard (#9150).
//!
//! Why: the guard's here-document scanner cut `<<'A B'` at the space and
//! ended the body at a later `A` line, while bash and zsh keep the whole
//! quoted word and end the body at `A B`. The `$(rm -rf /)` between the two
//! terminators then read as quoted body text to the #8735 substitution scan,
//! but runs in the shell, so the #8878 hard floor allowed it.
//! What: spawns `tm hook --pm-guard` through the #8878 fixture and checks that
//! every shape bash and zsh delimit differently from the old scanner denies,
//! under each bypass, while a correctly delimited quoted body that only holds
//! `$(…)` text still allows. The shell behaviour each row relies on was
//! probed with `/bin/bash` 3.2 and `/bin/zsh` 5.9 (recorded on #9150).
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_heredoc_delimiter_9150::`.

use crate::tm_hook_pm_guard_trust_anchor_8878::{Fixture, bash_payload, run};

/// The bypass variables, and `None` for no bypass.
const BYPASSES: [Option<(&str, &str)>; 3] = [
    None,
    Some(("TRUSTY_MPM_PM_UNRESTRICTED", "1")),
    Some(("TRUSTY_MPM_DISABLE_HOOKS", "1")),
];

/// Shapes the shell delimits at the whole quoted word, so the `$(…)` after
/// the real terminator runs. Each allowed before the #9150 fix.
const HIDDEN_SUBSTITUTIONS: [&str; 10] = [
    // The #9150 report.
    "cat <<'A B'\nx\nA B\necho \"$(rm -rf /)\"\nA",
    "cat <<'A>B'\nx\nA>B\necho \"$(rm -rf /)\"\nA",
    "cat <<'A<B'\nx\nA<B\necho \"$(rm -rf /)\"\nA",
    "cat <<-'A B'\n\tx\n\tA B\necho \"$(rm -rf /)\"\n\tA",
    "cat <<'A B'\nx\nA B\necho \"$(rm -rf $HOME)\"\nA",
    "cat <<'A B'\nx\nA B\necho \"$(rm -rf ~)\"\nA",
    "cat <<'A\tB'\nx\nA\tB\necho \"$(rm -rf /)\"\nA",
    "cat <<\"A B\"\nx\nA B\necho \"$(rm -rf /)\"\nA",
    "cat <<A\\ B\nx\nA B\necho \"$(rm -rf /)\"\nA",
    // A trailing `\` continues the line: the shell's word is the unquoted
    // `AB`, so the body expands and its `$(…)` runs.
    "cat <<A\\\nB\n$(rm -rf /)\nA\nAB",
];

/// #9150: every shape above denies, under each bypass.
#[test]
fn a_quoted_heredoc_delimiter_with_a_word_break_denies() {
    let fx = Fixture::new();
    let mut allowed = Vec::new();
    for bypass in BYPASSES {
        let env: Vec<(&str, &str)> = bypass.into_iter().collect();
        for command in HIDDEN_SUBSTITUTIONS {
            let out = run(&fx, &bash_payload(&fx, command), &env, Some("pm"));
            if !out.contains("\"deny\"") {
                allowed.push(format!("{bypass:?} {command:?}: {out:?}"));
            }
        }
    }
    assert!(allowed.is_empty(), "allowed:\n{}", allowed.join("\n"));
}

/// #9150 no-false-deny: a correctly delimited quoted body is data, so `$(…)`
/// text in it stays inert and a benign command after it still allows.
#[test]
fn a_plain_quoted_heredoc_with_substitution_text_still_allows() {
    let fx = Fixture::new();
    for command in [
        "cat <<'EOF'\n$(rm -rf /)\nEOF\necho benign",
        "cat <<-'EOF'\n\t$(rm -rf /)\n\tEOF\necho benign",
    ] {
        let out = run(&fx, &bash_payload(&fx, command), &[], Some("pm"));
        assert!(!out.contains("\"deny\""), "{command:?}: {out}");
    }
}
